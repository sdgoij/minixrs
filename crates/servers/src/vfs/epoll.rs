//! `epoll` — a readiness instance with a persistent interest set.
//!
//! MINIX 3.3.0 has no `epoll`; `poll` is emulated over `select` there, and
//! `calloop`/smithay require a real one. An epoll instance is a VFS-internal
//! anonymous object in the `eventfd`/`timerfd` shape — a synthetic identity
//! ([`arch_common::com::EPOLL_DEV`] and a never-reused id), no filesystem
//! (`v_fs_e` = NONE, `v_fs_count` = 0), VFS answering `fstat` and rejecting
//! `read`/`write` — but it is not itself a readiness source. It holds a
//! **persistent** set of interests, each `(fd, events, data)`.
//!
//! `epoll_wait` copies that set into one wait entry of the shared readiness
//! engine (`vfs::select`) and scans it exactly as `poll` scans its array, so a
//! suspended `epoll_wait` is completed by the same paths that complete a
//! suspended `poll`: a driver's `CDEV_SEL2_REPLY`, a VFS-internal object's
//! `wake_*` (`wake_object`), or the shared deadline. Level-triggered by
//! construction — each wait re-scans the whole set.
//!
//! Deviations from Linux, all recorded in `WAYLAND.md` §5: the instance holds at
//! most [`MAX_INTERESTS`] registrations (the wait engine's per-entry bound);
//! `EPOLLET`/`EPOLLONESHOT` are accepted in the mask but behave level-triggered;
//! an interest whose fd the waiter has closed is dropped lazily at the next
//! `epoll_wait` (`prune`) rather than at close; and `EPOLLERR`/`EPOLLHUP`/
//! `EPOLLRDHUP` are not synthesised (the engine does not produce them for `poll`
//! either).

use core::cell::UnsafeCell;

use minix_std::fs::Stat;

use crate::vfs::call::{SELF, sys_vircopy};
use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::{Filp, Vnode};

/// `S_IFREG` with the permissions an epoll instance is created with.
const EPOLL_MODE: u32 = S_IFREG | 0o600;

/// `epoll_create1(2)` flags, matching `<sys/epoll.h>` in `tools/c-include`.
pub const EPOLL_CLOEXEC: i32 = 0o200000; // the port's O_CLOEXEC

/// `epoll_ctl(2)` operations, matching `<sys/epoll.h>`.
pub const EPOLL_CTL_ADD: i32 = 1;
pub const EPOLL_CTL_DEL: i32 = 2;
pub const EPOLL_CTL_MOD: i32 = 3;

/// `struct epoll_event.events` bits, matching Linux `<sys/epoll.h>`. The low
/// bits coincide with `poll`'s (`EPOLLIN == POLLIN`, and so on).
pub const EPOLLIN: u32 = 0x001;
pub const EPOLLPRI: u32 = 0x002;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;
pub const EPOLLNVAL: u32 = 0x020;
pub const EPOLLRDHUP: u32 = 0x2000;

/// `struct epoll_event` is packed on Linux (`__attribute__((packed))`):
/// `u32 events; u64 data;` is 12 bytes, not 16. The ABI is shared, so match it.
pub const EPOLL_EVENT_SIZE: usize = 12;

/// Maximum live epoll instances.
const MAX_EPOLLS: usize = 64;

/// Maximum interests one instance holds. Equal to the wait engine's `MAX_WATCH`,
/// because one `epoll_wait` copies the whole set into a single wait entry.
pub const MAX_INTERESTS: usize = crate::vfs::select::MAX_WATCH;

/// One registration: a watched descriptor, the events asked for, and the
/// caller's opaque `data`.
#[derive(Clone, Copy)]
pub struct Interest {
    pub fd: i32,
    pub events: u32,
    pub data: u64,
}

impl Interest {
    pub const fn new() -> Self {
        Self {
            fd: -1,
            events: 0,
            data: 0,
        }
    }
}

impl Default for Interest {
    fn default() -> Self {
        Self::new()
    }
}

struct Epoll {
    /// Object id (the vnode's `v_inode_nr`); meaningful only while `used`.
    id: u32,
    /// Number of live interests in `interests[0..n]`.
    n: usize,
    interests: [Interest; MAX_INTERESTS],
    used: bool,
}

impl Epoll {
    const fn new() -> Self {
        Self {
            id: 0,
            n: 0,
            interests: [Interest::new(); MAX_INTERESTS],
            used: false,
        }
    }
}

struct Table(UnsafeCell<[Epoll; MAX_EPOLLS]>);
unsafe impl Sync for Table {}
impl Table {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { Epoll::new() }; MAX_EPOLLS]))
    }
    fn get(&self) -> *mut [Epoll; MAX_EPOLLS] {
        self.0.get()
    }
}

static TABLE: Table = Table::new();

fn slot(i: usize) -> &'static mut Epoll {
    unsafe { &mut *(*TABLE.get()).as_mut_ptr().add(i) }
}

fn lookup(id: u32) -> Option<&'static mut Epoll> {
    (0..MAX_EPOLLS).map(slot).find(|e| e.used && e.id == id)
}

fn find(ep: &Epoll, fd: i32) -> Option<usize> {
    (0..ep.n).find(|&i| ep.interests[i].fd == fd)
}

/// True for an epoll instance's vnode.
///
/// # Safety
///
/// `vp` must be a live vnode pointer.
pub unsafe fn is_epoll(vp: *const Vnode) -> bool {
    !vp.is_null() && (*vp).v_dev == arch_common::com::EPOLL_DEV
}

/// Resolve a descriptor in the calling fproc to its vnode.
///
/// # Safety
///
/// VFS dispatch only; reads the caller's fd table.
unsafe fn vnode_of(fd: i32) -> Result<*mut Vnode, i32> {
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
    Ok(vp)
}

/// The epoll object id behind a descriptor, or the errno to return.
///
/// # Safety
///
/// VFS dispatch only; reads the caller's fd table.
pub unsafe fn id_of(fd: i32) -> Result<u32, i32> {
    let vp = unsafe { vnode_of(fd) }?;
    if !unsafe { is_epoll(vp) } {
        return Err(EINVAL);
    }
    Ok(unsafe { (*vp).v_inode_nr })
}

/// Read a `struct epoll_event` from user memory.
///
/// # Safety
///
/// VFS dispatch only; `ep` must name the calling process.
unsafe fn read_event(ep: i32, p: u64) -> Option<(u32, u64)> {
    if p == 0 {
        return None;
    }
    let mut raw = [0u8; EPOLL_EVENT_SIZE];
    if unsafe { sys_vircopy(ep, p, SELF, raw.as_mut_ptr() as u64, EPOLL_EVENT_SIZE) } != OK {
        return None;
    }
    let events = u32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0; 4]));
    let data = u64::from_ne_bytes(raw[4..12].try_into().unwrap_or([0; 8]));
    Some((events, data))
}

/// `epoll_create1(flags)`: one readiness instance, returned as an fd.
pub fn do_create() -> i32 {
    let glob = vfs_global();
    let flags = r_i32(unsafe { &(*glob).fs_m_in }, EPOLL_CREATE_FLAGS_OFF);
    // Only EPOLL_CLOEXEC is meaningful here; anything else is an error (as on
    // Linux, which rejects unknown flags the same way).
    if flags & !EPOLL_CLOEXEC != 0 {
        return EINVAL;
    }

    let Some(i) = (0..MAX_EPOLLS).find(|&i| !slot(i).used) else {
        return ENFILE;
    };

    let (_vp, id, fd) = match unsafe {
        crate::vfs::anon::create_anon(arch_common::com::EPOLL_DEV, EPOLL_MODE, 0)
    } {
        Ok(v) => v,
        Err(e) => return e,
    };

    let ep = slot(i);
    ep.id = id;
    ep.n = 0;
    for k in 0..MAX_INTERESTS {
        ep.interests[k] = Interest::new();
    }
    ep.used = true;

    if flags & EPOLL_CLOEXEC != 0 {
        unsafe { crate::vfs::anon::set_cloexec(fd) };
    }
    unsafe { crate::vfs::anon::reply_fd(fd) };
    OK
}

/// `epoll_ctl(epfd, op, fd, event)`: add, modify or remove one interest.
pub fn do_ctl() -> i32 {
    let glob = vfs_global();
    let epfd = r_i32(unsafe { &(*glob).fs_m_in }, EPOLL_CTL_EPFD_OFF);
    let op = r_i32(unsafe { &(*glob).fs_m_in }, EPOLL_CTL_OP_OFF);
    let fd = r_i32(unsafe { &(*glob).fs_m_in }, EPOLL_CTL_FD_OFF);
    let event_p = r_u64(unsafe { &(*glob).fs_m_in }, EPOLL_CTL_EVENT_OFF);

    let epid = match unsafe { id_of(epfd) } {
        Ok(id) => id,
        Err(e) => return e,
    };
    // Linux: an epoll instance cannot watch itself, and epoll instances cannot
    // be nested.
    if fd == epfd {
        return EINVAL;
    }
    let Some(ep) = lookup(epid) else {
        return EBADF;
    };
    // `fd` must be a live descriptor in the caller's table (EBADF otherwise),
    // for every operation including DEL.
    let target = match unsafe { vnode_of(fd) } {
        Ok(vp) => vp,
        Err(e) => return e,
    };
    if unsafe { is_epoll(target) } {
        return EINVAL;
    }
    let ep_endpt = unsafe { (*(*glob).fp).fp_endpoint };

    match op {
        EPOLL_CTL_ADD => {
            if find(ep, fd).is_some() {
                return EEXIST;
            }
            let Some((events, data)) = (unsafe { read_event(ep_endpt, event_p) }) else {
                return EFAULT;
            };
            if ep.n >= MAX_INTERESTS {
                return ENOSPC;
            }
            ep.interests[ep.n] = Interest { fd, events, data };
            ep.n += 1;
            OK
        }
        EPOLL_CTL_MOD => {
            let Some(idx) = find(ep, fd) else {
                return ENOENT;
            };
            let Some((events, data)) = (unsafe { read_event(ep_endpt, event_p) }) else {
                return EFAULT;
            };
            ep.interests[idx].events = events;
            ep.interests[idx].data = data;
            OK
        }
        EPOLL_CTL_DEL => {
            let Some(idx) = find(ep, fd) else {
                return ENOENT;
            };
            // Shift down, so the remaining registrations keep their order.
            for j in idx..ep.n - 1 {
                ep.interests[j] = ep.interests[j + 1];
            }
            ep.n -= 1;
            OK
        }
        _ => EINVAL,
    }
}

/// Drop interests whose fd the caller has closed.
///
/// Linux removes an interest when the last reference to its file goes; the port
/// has no close hook on a VFS-internal object, so the caller's next
/// `epoll_wait` prunes instead. Without this a stale registration would report
/// `EPOLLNVAL` on every level-triggered re-scan and spin the loop.
///
/// # Safety
///
/// VFS dispatch only; reads the calling fproc's fd table.
pub unsafe fn prune(id: u32) {
    let glob = vfs_global();
    let fp = (*glob).fp;
    if fp.is_null() {
        return;
    }
    let Some(ep) = lookup(id) else {
        return;
    };
    let mut w = 0usize;
    for r in 0..ep.n {
        let fd = ep.interests[r].fd;
        let open = fd >= 0 && (fd as usize) < OPEN_MAX && (*fp).fp_filp[fd as usize] >= 0;
        if open {
            ep.interests[w] = ep.interests[r];
            w += 1;
        }
    }
    ep.n = w;
}

/// Copy an instance's interest set into `out`, returning how many were copied
/// (at most `out.len()`), or `None` if `id` is not a live epoll.
///
/// # Safety
///
/// VFS dispatch only.
pub unsafe fn snapshot(id: u32, out: &mut [Interest]) -> Option<usize> {
    let ep = lookup(id)?;
    let n = ep.n.min(out.len());
    out[..n].copy_from_slice(&ep.interests[..n]);
    Some(n)
}

/// Release the object a closed vnode held. Called from `put_vnode`.
///
/// # Safety
///
/// `id` must be an epoll object id.
pub unsafe fn release(id: u32) {
    if let Some(ep) = lookup(id) {
        *ep = Epoll::new();
    }
}

/// `fstat` on an epoll instance, built here rather than asked of a filesystem.
///
/// # Safety
///
/// `vp` must be a live epoll vnode; VFS dispatch only.
pub unsafe fn fstat(vp: *const Vnode, ep: i32, buf: u64) -> i32 {
    let st = Stat {
        st_dev: (*vp).v_dev as u64,
        st_ino: (*vp).v_inode_nr as u64,
        st_mode: (*vp).v_mode,
        st_nlink: 0,
        st_uid: (*vp).v_uid as u32,
        st_gid: (*vp).v_gid as u32,
        st_rdev: 0,
        st_size: 0,
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
