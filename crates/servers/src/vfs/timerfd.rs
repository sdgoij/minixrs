//! `timerfd` — a file descriptor that becomes readable when a timer expires.
//!
//! MINIX 3.3.0 has no `timerfd`. An event loop (`calloop`, and so smithay) needs
//! one: a timeout that is a *descriptor* it can sit in the same `poll`/`select`
//! as its sockets, rather than a separate timer mechanism. It is the second
//! VFS-internal anonymous object after `eventfd` (`vfs::eventfd`), built the
//! same way: a synthetic identity ([`arch_common::com::TIMERFD_DEV`] and a
//! never-reused id), no filesystem, and VFS answering `read`/`fstat` and the
//! `timerfd_settime`/`timerfd_gettime` calls directly.
//!
//! A `read` returns the number of expirations since the previous read (or
//! `settime`); 0 expirations is `EAGAIN` rather than a block, matching the
//! version use (`poll` first) and the port's pipes.
//!
//! The timer itself rides the one process alarm VFS already owns for
//! `select`/`poll` deadlines (`vfs::alarm`): the earliest pending timerfd
//! expiry participates in [`crate::vfs::select`]'s re-arm, and the `CLOCK`
//! notification that fires calls [`expire`], which advances the counters and
//! wakes any blocked `poll` on the object.
//!
//! Granularity is the system tick (`hz`, 60): a value that rounds to zero ticks
//! waits one tick, as `select`'s deadline does.

use core::cell::UnsafeCell;

use minix_std::fs::Stat;

use crate::vfs::alarm;
use crate::vfs::call::{SELF, sys_vircopy};
use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::{Filp, Vnode};

/// `S_IFREG` with the permissions a timerfd is created with.
const TIMERFD_MODE: u32 = S_IFREG | 0o600;

/// `timerfd_create(2)` / `timerfd_settime(2)` flags, matching
/// `<sys/timerfd.h>` in `tools/c-include`.
pub const TFD_NONBLOCK: i32 = 0o4000; // O_NONBLOCK
pub const TFD_CLOEXEC: i32 = 0o200000; // the port's O_CLOEXEC
pub const TFD_TIMER_ABSTIME: i32 = 1;

/// `CLOCK_MONOTONIC` (the clock `calloop` uses).
pub const CLOCK_MONOTONIC: i32 = 1;

/// Maximum live timerfds.
const MAX_TIMERFDS: usize = 64;

// `struct itimerspec` field offsets: interval then value, each `{ i64 sec; i64 nsec; }`.
const IT_INTERVAL_SEC: usize = 0;
const IT_INTERVAL_NSEC: usize = 8;
const IT_VALUE_SEC: usize = 16;
const IT_VALUE_NSEC: usize = 24;
const ITIMERSPEC_LEN: usize = 32;

struct TimerFd {
    /// Object id (the vnode's `v_inode_nr`); meaningful only while `used`.
    id: u32,
    /// `CLOCK_REALTIME` or `CLOCK_MONOTONIC`.
    clockid: i32,
    /// Next expiry as an absolute monotonic tick; 0 = disarmed.
    next: u64,
    /// Period in ticks; 0 = one-shot.
    interval: u64,
    /// Expirations since the last read.
    fired: u64,
    used: bool,
}

impl TimerFd {
    const fn new() -> Self {
        Self {
            id: 0,
            clockid: 0,
            next: 0,
            interval: 0,
            fired: 0,
            used: false,
        }
    }
}

struct Table(UnsafeCell<[TimerFd; MAX_TIMERFDS]>);
unsafe impl Sync for Table {}
impl Table {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { TimerFd::new() }; MAX_TIMERFDS]))
    }
    fn get(&self) -> *mut [TimerFd; MAX_TIMERFDS] {
        self.0.get()
    }
}

static TABLE: Table = Table::new();

fn slot(i: usize) -> &'static mut TimerFd {
    unsafe { &mut *(*TABLE.get()).as_mut_ptr().add(i) }
}

fn lookup(id: u32) -> Option<&'static mut TimerFd> {
    (0..MAX_TIMERFDS).map(slot).find(|t| t.used && t.id == id)
}

/// True for a timerfd's vnode.
///
/// # Safety
///
/// `vp` must be a live vnode pointer.
pub unsafe fn is_timerfd(vp: *const Vnode) -> bool {
    !vp.is_null() && (*vp).v_dev == arch_common::com::TIMERFD_DEV
}

/// The ready ops of the object with this id — readable when it has fired or its
/// expiry has passed. Pure (no expiry is materialised here) so it is safe to call
/// from a scan; [`expire`] does the accounting.
///
/// # Safety
///
/// Single-threaded VFS context.
pub unsafe fn ready(id: u32) -> u32 {
    use crate::vfs::select::SEL_RD;
    match lookup(id) {
        Some(o) => {
            let now = alarm::now();
            if o.fired > 0 || (o.next != 0 && now >= o.next) {
                SEL_RD
            } else {
                0
            }
        }
        None => 0,
    }
}

/// The earliest pending expiry among live timerfds, or 0 if none is armed.
///
/// # Safety
///
/// Single-threaded VFS context.
pub unsafe fn earliest() -> u64 {
    let mut min = 0u64;
    for i in 0..MAX_TIMERFDS {
        let o = slot(i);
        if o.used && o.next != 0 && (min == 0 || o.next < min) {
            min = o.next;
        }
    }
    min
}

/// Advance every armed timerfd whose expiry is at or before `now`, counting the
/// expirations missed (a periodic timer that was late counts each interval) and
/// waking any blocked `poll` on the object. Called by the alarm tick and before
/// an observation (`read`/`gettime`).
///
/// # Safety
///
/// Single-threaded VFS context.
pub unsafe fn expire(now: u64) {
    let mut woke = [0u32; MAX_TIMERFDS];
    let mut nw = 0usize;
    for i in 0..MAX_TIMERFDS {
        let o = slot(i);
        if !o.used || o.next == 0 || now < o.next {
            continue;
        }
        // A periodic timer may have missed several intervals; count them all so
        // the count does not drift. A one-shot timer disarms after firing.
        let interval = o.interval;
        let n = 1 + (now - o.next) / interval.max(1);
        o.fired = o.fired.saturating_add(n);
        if interval == 0 {
            o.next = 0;
        } else {
            o.next += n * interval;
        }
        if nw < MAX_TIMERFDS {
            woke[nw] = o.id;
            nw += 1;
        }
    }
    // Wake after the sweep, so no table reference is held across a completion
    // that re-enters the table.
    for &id in &woke[..nw] {
        crate::vfs::select::wake_timerfd(id);
    }
}

/// Ticks remaining until `o`'s next expiry (0 if disarmed).
fn remaining(o: &TimerFd) -> u64 {
    if o.next == 0 {
        0
    } else {
        o.next.saturating_sub(alarm::now())
    }
}

/// Resolve a descriptor to its timerfd vnode.
///
/// # Safety
///
/// VFS dispatch only; reads the caller's fd table.
unsafe fn vnode_for(fd: i32) -> Result<*mut Vnode, i32> {
    let glob = vfs_global();
    let fp = (*glob).fp;
    if fp.is_null() {
        return Err(EINVAL);
    }
    if fd < 0 || fd as usize >= OPEN_MAX {
        return Err(EBADF);
    }
    let filp_idx = (*fp).fp_filp[fd as usize];
    if filp_idx < 0 {
        return Err(EBADF);
    }
    let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
    let filp = &*filp_arr.add(filp_idx as usize);
    let vp = filp.filp_vno;
    if vp.is_null() {
        return Err(EBADF);
    }
    if !is_timerfd(vp) {
        return Err(EINVAL);
    }
    Ok(vp)
}

/// Write an `itimerspec` (interval, value) to user memory.
///
/// # Safety
///
/// VFS dispatch only.
unsafe fn write_itimerspec(ptr: u64, ep: i32, interval_ticks: u64, value_ticks: u64) {
    let (isec, insec) = alarm::ticks_to_timespec(interval_ticks);
    let (vsec, vnsec) = alarm::ticks_to_timespec(value_ticks);
    let mut raw = [0u8; ITIMERSPEC_LEN];
    raw[IT_INTERVAL_SEC..IT_INTERVAL_SEC + 8].copy_from_slice(&isec.to_ne_bytes());
    raw[IT_INTERVAL_NSEC..IT_INTERVAL_NSEC + 8].copy_from_slice(&insec.to_ne_bytes());
    raw[IT_VALUE_SEC..IT_VALUE_SEC + 8].copy_from_slice(&vsec.to_ne_bytes());
    raw[IT_VALUE_NSEC..IT_VALUE_NSEC + 8].copy_from_slice(&vnsec.to_ne_bytes());
    let _ = sys_vircopy(SELF, raw.as_ptr() as u64, ep, ptr, ITIMERSPEC_LEN);
}

/// `timerfd_create(clockid, flags)`: one timer object, returned as an fd.
pub fn do_create() -> i32 {
    let glob = vfs_global();
    let clockid = r_i32(unsafe { &(*glob).fs_m_in }, 8);
    let flags = r_i32(unsafe { &(*glob).fs_m_in }, 12);
    if clockid != alarm::CLOCK_REALTIME && clockid != CLOCK_MONOTONIC {
        return EINVAL;
    }

    let Some(i) = (0..MAX_TIMERFDS).find(|&i| !slot(i).used) else {
        return ENFILE;
    };

    let (_vp, id, fd) = match unsafe {
        crate::vfs::anon::create_anon(arch_common::com::TIMERFD_DEV, TIMERFD_MODE, 8)
    } {
        Ok(v) => v,
        Err(e) => return e,
    };

    let o = slot(i);
    o.id = id;
    o.clockid = clockid;
    o.next = 0;
    o.interval = 0;
    o.fired = 0;
    o.used = true;

    if flags & TFD_CLOEXEC != 0 {
        unsafe { crate::vfs::anon::set_cloexec(fd) };
    }
    unsafe { crate::vfs::anon::reply_fd(fd) };
    OK
}

/// `timerfd_settime(fd, flags, new, old)`: arm, re-arm or (with a zero value)
/// disarm the timer, optionally returning the previous setting.
pub fn do_settime() -> i32 {
    let glob = vfs_global();
    let fd = r_i32(unsafe { &(*glob).fs_m_in }, 8);
    let flags = r_i32(unsafe { &(*glob).fs_m_in }, 12);
    let new_p = r_u64(unsafe { &(*glob).fs_m_in }, 16);
    let old_p = r_u64(unsafe { &(*glob).fs_m_in }, 24);

    let vp = match unsafe { vnode_for(fd) } {
        Ok(v) => v,
        Err(e) => return e,
    };
    let id = unsafe { (*vp).v_inode_nr };
    let Some(o) = lookup(id) else {
        return EINVAL;
    };
    let ep = unsafe { (*(*glob).fp).fp_endpoint };

    // The previous setting, before it is replaced.
    if old_p != 0 {
        unsafe { write_itimerspec(old_p, ep, o.interval, remaining(o)) };
    }

    if new_p == 0 {
        return EFAULT;
    }
    let mut raw = [0u8; ITIMERSPEC_LEN];
    if unsafe { sys_vircopy(ep, new_p, SELF, raw.as_mut_ptr() as u64, ITIMERSPEC_LEN) } != OK {
        return EFAULT;
    }
    let isec = i64::from_ne_bytes(
        raw[IT_INTERVAL_SEC..IT_INTERVAL_SEC + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let insec = i64::from_ne_bytes(
        raw[IT_INTERVAL_NSEC..IT_INTERVAL_NSEC + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let vsec = i64::from_ne_bytes(
        raw[IT_VALUE_SEC..IT_VALUE_SEC + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let vnsec = i64::from_ne_bytes(
        raw[IT_VALUE_NSEC..IT_VALUE_NSEC + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );

    o.interval = alarm::timespec_to_ticks(isec, insec);
    o.next = alarm::deadline_at(o.clockid, flags & TFD_TIMER_ABSTIME != 0, vsec, vnsec);
    // A fresh setting counts expirations from now.
    o.fired = 0;

    unsafe { crate::vfs::select::rearm() };
    OK
}

/// `timerfd_gettime(fd, cur)`: the remaining time and the interval.
pub fn do_gettime() -> i32 {
    let glob = vfs_global();
    let fd = r_i32(unsafe { &(*glob).fs_m_in }, 8);
    let cur_p = r_u64(unsafe { &(*glob).fs_m_in }, 16);

    let vp = match unsafe { vnode_for(fd) } {
        Ok(v) => v,
        Err(e) => return e,
    };
    let id = unsafe { (*vp).v_inode_nr };
    let Some(o) = lookup(id) else {
        return EINVAL;
    };
    if cur_p == 0 {
        return EFAULT;
    }
    let ep = unsafe { (*(*glob).fp).fp_endpoint };
    // Materialise any expiry whose deadline has passed, so `remaining` is current.
    unsafe { expire(alarm::now()) };
    unsafe { write_itimerspec(cur_p, ep, o.interval, remaining(o)) };
    OK
}

/// `read(timerfd)`: the number of expirations since the last read, 8 bytes.
///
/// # Safety
///
/// `vp` must be a live timerfd vnode; VFS dispatch only.
pub unsafe fn read(vp: *mut Vnode, ep: i32, buf: u64, count: usize) -> i32 {
    if count < 8 {
        return EINVAL;
    }
    let id = (*vp).v_inode_nr;
    let Some(o) = lookup(id) else {
        return EBADF;
    };
    expire(alarm::now());
    if o.fired == 0 {
        // Not blocking (see the module docs): the caller polls first.
        return EAGAIN;
    }
    let bytes = o.fired.to_ne_bytes();
    let r = sys_vircopy(SELF, bytes.as_ptr() as u64, ep, buf, 8);
    if r != OK {
        return r;
    }
    o.fired = 0;
    8
}

/// Release the object a closed vnode held. Called from `put_vnode`.
///
/// # Safety
///
/// `id` must be a timerfd object id.
pub unsafe fn release(id: u32) {
    if let Some(o) = lookup(id) {
        *o = TimerFd::new();
    }
}

/// `fstat` on a timerfd, built here rather than asked of a filesystem.
///
/// # Safety
///
/// `vp` must be a live timerfd vnode; VFS dispatch only.
pub unsafe fn fstat(vp: *const Vnode, ep: i32, buf: u64) -> i32 {
    let st = Stat {
        st_dev: (*vp).v_dev as u64,
        st_ino: (*vp).v_inode_nr as u64,
        st_mode: (*vp).v_mode,
        st_nlink: 0,
        st_uid: (*vp).v_uid as u32,
        st_gid: (*vp).v_gid as u32,
        st_rdev: 0,
        st_size: 8,
        st_blksize: 0,
        st_blocks: 0,
        st_atime: 0,
        st_mtime: 0,
        st_ctime: 0,
    };
    let bytes = core::mem::size_of::<Stat>();
    sys_vircopy(SELF, &st as *const Stat as u64, ep, buf, bytes)
}

fn r_i32(buf: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn r_u64(buf: &[u8; 64], off: usize) -> u64 {
    u64::from_ne_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}
