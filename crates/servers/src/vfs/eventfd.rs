//! `eventfd` — an anonymous 64-bit counter an event loop polls to wake itself.
//!
//! MINIX 3.3.0 has no `eventfd`, and neither the reference nor the port has a
//! self-pipe substitute built in. A Wayland compositor (and `calloop` under it)
//! needs one: a thread or a signal handler writes 8 bytes to wake a blocked
//! `poll`/`epoll_wait` without a byte of real data.
//!
//! The object is VFS-internal, like a `memfd` (`vfs::memfd`): a vnode with a
//! synthetic identity ([`arch_common::com::EVENTFD_DEV`] and a never-reused id),
//! no filesystem behind it (`v_fs_e` = NONE, `v_fs_count` = 0), and VFS answers
//! `read`, `write` and `fstat` on it directly. The counter and flags live in the
//! table below, keyed by the vnode's inode number; `dup` and `fork` share one
//! object through the shared vnode, and `close` of the last reference releases
//! the slot (`vfs::mount::put_vnode`).
//!
//! Readiness is computed here and read by the readiness engine
//! (`vfs::select::scan`), so a blocked `poll`/`select` on the eventfd is woken
//! by a write from any process (`vfs::select::wake_eventfd`) — no driver
//! round-trip, because VFS *is* the device.
//!
//! Deviation from Linux: a `read` on a counter of 0 returns `EAGAIN` rather than
//! blocking. The port's pipes already deviate the same way (there is no
//! suspend/revive for reads yet — see `vfs::pipe`), and an event-loop consumer
//! polls before it reads.

use core::cell::UnsafeCell;

use minix_std::fs::Stat;

use crate::vfs::call::{SELF, sys_vircopy};
use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::Vnode;

/// `S_IFREG` with the permissions an eventfd is created with.
const EVENTFD_MODE: u32 = S_IFREG | 0o600;

/// `eventfd(2)` flags, matching `<sys/eventfd.h>` in `tools/c-include`.
pub const EFD_SEMAPHORE: i32 = 1;
/// `O_NONBLOCK`; accepted, but a read never blocks here (see the module docs).
pub const EFD_NONBLOCK: i32 = 0o4000;
/// The port's `O_CLOEXEC` (see `tools/c-include/fcntl.h`).
pub const EFD_CLOEXEC: i32 = 0o200000;

/// Maximum live eventfds.
const MAX_EVENTFDS: usize = 64;

struct EventFd {
    /// Object id (the vnode's `v_inode_nr`); meaningful only while `used`.
    id: u32,
    counter: u64,
    flags: i32,
    used: bool,
}

impl EventFd {
    const fn new() -> Self {
        Self {
            id: 0,
            counter: 0,
            flags: 0,
            used: false,
        }
    }
}

struct Table(UnsafeCell<[EventFd; MAX_EVENTFDS]>);
unsafe impl Sync for Table {}
impl Table {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { EventFd::new() }; MAX_EVENTFDS]))
    }
    fn get(&self) -> *mut [EventFd; MAX_EVENTFDS] {
        self.0.get()
    }
}

static TABLE: Table = Table::new();

fn slot(i: usize) -> &'static mut EventFd {
    unsafe { &mut *(*TABLE.get()).as_mut_ptr().add(i) }
}

fn lookup(id: u32) -> Option<&'static mut EventFd> {
    (0..MAX_EVENTFDS).map(slot).find(|e| e.used && e.id == id)
}

/// True for an eventfd's vnode.
///
/// # Safety
///
/// `vp` must be a live vnode pointer.
pub unsafe fn is_eventfd(vp: *const Vnode) -> bool {
    !vp.is_null() && (*vp).v_dev == arch_common::com::EVENTFD_DEV
}

/// The ready ops of the object with this id (`SEL_RD`/`SEL_WR`) — readable when
/// the counter is nonzero, writable while a value could still be added.
///
/// # Safety
///
/// Single-threaded VFS context (the table is an `UnsafeCell`).
pub unsafe fn ready(id: u32) -> u32 {
    use crate::vfs::select::{SEL_RD, SEL_WR};
    match lookup(id) {
        Some(o) => {
            let mut r = 0u32;
            if o.counter != 0 {
                r |= SEL_RD;
            }
            if o.counter < u64::MAX {
                r |= SEL_WR;
            }
            r
        }
        None => 0,
    }
}

/// `eventfd(initval, flags)`: one counter object, returned as an fd.
pub fn do_create() -> i32 {
    let glob = vfs_global();
    let initval = r_i32(unsafe { &(*glob).fs_m_in }, 8) as u32;
    let flags = r_i32(unsafe { &(*glob).fs_m_in }, 12);

    // Reserve the table slot before creating the descriptor, so a full table is
    // refused without leaving a descriptor to unwind.
    let Some(i) = (0..MAX_EVENTFDS).find(|&i| !slot(i).used) else {
        return ENFILE;
    };

    let (_vp, id, fd) = match unsafe {
        crate::vfs::anon::create_anon(arch_common::com::EVENTFD_DEV, EVENTFD_MODE, 8)
    } {
        Ok(v) => v,
        Err(e) => return e,
    };

    let e = slot(i);
    e.id = id;
    e.counter = initval as u64;
    e.flags = flags;
    e.used = true;

    if flags & EFD_CLOEXEC != 0 {
        unsafe { crate::vfs::anon::set_cloexec(fd) };
    }
    unsafe { crate::vfs::anon::reply_fd(fd) };
    OK
}

/// `read(eventfd)`: consume the counter. With `EFD_SEMAPHORE` one is returned
/// and the counter decremented; otherwise the whole counter is returned and
/// reset to 0.
///
/// # Safety
///
/// `vp` must be a live eventfd vnode; VFS dispatch only.
pub unsafe fn read(vp: *mut Vnode, ep: i32, buf: u64, count: usize) -> i32 {
    if count < 8 {
        return EINVAL;
    }
    let id = (*vp).v_inode_nr;
    let Some(obj) = lookup(id) else {
        return EBADF;
    };
    if obj.counter == 0 {
        // Not blocking (see the module docs): the caller polls first.
        return EAGAIN;
    }
    let value = if obj.flags & EFD_SEMAPHORE != 0 {
        obj.counter -= 1;
        1u64
    } else {
        let v = obj.counter;
        obj.counter = 0;
        v
    };
    let bytes = value.to_ne_bytes();
    let r = sys_vircopy(SELF, bytes.as_ptr() as u64, ep, buf, 8);
    if r != OK {
        return r;
    }
    // The counter drained: a waiter for writability may now be satisfied.
    crate::vfs::select::wake_eventfd(id);
    8
}

/// `write(eventfd)`: add an 8-byte value to the counter.
///
/// # Safety
///
/// `vp` must be a live eventfd vnode; VFS dispatch only.
pub unsafe fn write(vp: *mut Vnode, ep: i32, buf: u64, count: usize) -> i32 {
    if count < 8 {
        return EINVAL;
    }
    let id = (*vp).v_inode_nr;
    let Some(obj) = lookup(id) else {
        return EBADF;
    };
    let mut raw = [0u8; 8];
    let r = sys_vircopy(ep, buf, SELF, raw.as_mut_ptr() as u64, 8);
    if r != OK {
        return r;
    }
    let value = u64::from_ne_bytes(raw);
    // A value of `u64::MAX` is rejected; any value that would overflow blocks
    // (here: EAGAIN, since a write never blocks in this port).
    if value == u64::MAX {
        return EINVAL;
    }
    if obj.counter > u64::MAX - value {
        return EAGAIN;
    }
    obj.counter += value;
    // The counter became nonzero: wake any blocked reader.
    crate::vfs::select::wake_eventfd(id);
    8
}

/// Release the object a closed vnode held. Called from `put_vnode` when the last
/// reference goes.
///
/// # Safety
///
/// `id` must be an eventfd object id.
pub unsafe fn release(id: u32) {
    if let Some(obj) = lookup(id) {
        *obj = EventFd::new();
    }
}

/// `fstat` on an eventfd, built here rather than asked of a filesystem.
///
/// # Safety
///
/// `vp` must be a live eventfd vnode; VFS dispatch only.
pub unsafe fn fstat(vp: *const Vnode, ep: i32, buf: u64) -> i32 {
    let st = Stat {
        st_dev: (*vp).v_dev as u64,
        st_ino: (*vp).v_inode_nr as u64,
        st_mode: (*vp).v_mode,
        // One anonymous object, named by nothing.
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
