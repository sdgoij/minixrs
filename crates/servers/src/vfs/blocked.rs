//! Suspended read/write requests on the objects VFS answers itself.
//!
//! C keeps a suspended request's arguments in `scratch(fp)` and hands the caller to another
//! worker (`pipe_suspend` and `do_pending_pipe` behind `suspend(FP_BLOCKED_ON_PIPE)`). This
//! port has one VFS thread: a handler that cannot proceed returns `SUSPEND`, which the main
//! loop reads as "do not reply", and the arguments wait here until the module that can make
//! progress resumes the request and sends the answer.
//!
//! Only a pipe, an eventfd and a timerfd park here. A character device has the driver's own
//! `CDEV_REPLY`, and `select`/`poll`/`epoll` keep their own wait table in `vfs::select`.
//!
//! `fp_blocked_on` stays the truth for "this process is suspended", as it does for a lock
//! (`FP_BLOCKED_ON_LOCK`) and a select (`FP_BLOCKED_ON_SELECT`): a slot with a record here
//! always has that flag, and clearing one clears the other.

use core::cell::UnsafeCell;

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::*;

/// Which object a parked request waits on.
pub const PIPE: u8 = 1;
pub const EVENTFD: u8 = 2;
pub const TIMERFD: u8 = 3;

/// One suspended request.
#[derive(Clone, Copy)]
pub struct Parked {
    /// [`PIPE`], [`EVENTFD`] or [`TIMERFD`].
    pub kind: u8,
    /// Pipe: the filp index. Eventfd and timerfd: the object id.
    pub id: i32,
    /// Pipe direction (`pipe::READING`/`pipe::WRITING`); 0 for the others.
    pub op: i32,
    /// The caller's buffer and the byte count it asked for.
    pub buf: u64,
    pub count: usize,
}

const FREE: Parked = Parked {
    kind: 0,
    id: -1,
    op: -1,
    buf: 0,
    count: 0,
};

pub const SLOTS: usize = arch_common::consts::NR_PROCS;

struct Table(UnsafeCell<[Parked; SLOTS]>);
unsafe impl Sync for Table {}
impl Table {
    const fn new() -> Self {
        Self(UnsafeCell::new([FREE; SLOTS]))
    }
    fn get(&self) -> *mut [Parked; SLOTS] {
        self.0.get()
    }
}

static TABLE: Table = Table::new();

/// The slot an endpoint decodes to, which is also its fproc table index.
pub fn slot_of(ep: i32) -> usize {
    (ep & 0xff) as usize
}

/// The fproc of `slot`.
///
/// # Safety
///
/// VFS dispatch only.
pub unsafe fn fproc(slot: usize) -> *mut Fproc {
    let glob = vfs_global();
    (core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc).add(slot.min(SLOTS - 1))
}

/// The record parked for `slot`, without consuming it.
///
/// # Safety
///
/// VFS dispatch only.
pub unsafe fn peek(slot: usize) -> Option<Parked> {
    let p = unsafe { &*(*TABLE.get()).as_ptr().add(slot) };
    if p.id < 0 { None } else { Some(*p) }
}

/// Suspend `fp`'s request, returning `SUSPEND` for the handler to pass upwards.
///
/// # Safety
///
/// VFS dispatch only, for the process `fp` names.
pub unsafe fn park(fp: *mut Fproc, p: Parked) -> i32 {
    *(*TABLE.get()).as_mut_ptr().add(slot_of((*fp).fp_endpoint)) = p;
    (*fp).fp_blocked_on = FP_BLOCKED_ON_PIPE;
    SUSPEND
}

/// Take the record parked for `slot` when it waits on `kind`, clearing both it and the
/// blocked flag. The caller owns the request from here: it either answers it or puts it
/// back with [`restore`].
///
/// # Safety
///
/// VFS dispatch only.
pub unsafe fn take(slot: usize, kind: u8) -> Option<Parked> {
    let p = unsafe { peek(slot) }?;
    if p.kind != kind {
        return None;
    }
    *(*TABLE.get()).as_mut_ptr().add(slot) = FREE;
    (*unsafe { fproc(slot) }).fp_blocked_on = FP_BLOCKED_ON_NONE;
    Some(p)
}

/// Put a taken request back: it still cannot proceed.
///
/// # Safety
///
/// VFS dispatch only.
pub unsafe fn restore(slot: usize, p: Parked) {
    *(*TABLE.get()).as_mut_ptr().add(slot) = p;
    (*unsafe { fproc(slot) }).fp_blocked_on = FP_BLOCKED_ON_PIPE;
}

/// Forget the request of a process that is going away. Nothing is answered: its fds are
/// about to be closed, so the request could no longer be completed anyway.
///
/// # Safety
///
/// `fp` must be a live fproc slot during exit cleanup.
pub unsafe fn clear(fp: &mut Fproc) {
    if fp.fp_blocked_on == FP_BLOCKED_ON_PIPE {
        *(*TABLE.get()).as_mut_ptr().add(slot_of(fp.fp_endpoint)) = FREE;
        fp.fp_blocked_on = FP_BLOCKED_ON_NONE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_is_free_until_something_parks_on_it() {
        unsafe {
            // Reset, because the table is global and the tests share it.
            for i in 0..SLOTS {
                *(*TABLE.get()).as_mut_ptr().add(i) = FREE;
            }
            assert!(peek(7).is_none());
            let fp = fproc(7);
            (*fp).fp_endpoint = 7;
            let r = park(
                fp,
                Parked {
                    kind: EVENTFD,
                    id: 3,
                    op: 0,
                    buf: 0x1000,
                    count: 8,
                },
            );
            assert_eq!(r, SUSPEND);
            assert_eq!((*fp).fp_blocked_on, FP_BLOCKED_ON_PIPE);
            let p = peek(7).expect("parked");
            assert_eq!((p.kind, p.id, p.count), (EVENTFD, 3, 8));
        }
    }

    #[test]
    fn take_matches_the_kind_and_restore_puts_it_back() {
        unsafe {
            for i in 0..SLOTS {
                *(*TABLE.get()).as_mut_ptr().add(i) = FREE;
            }
            let fp = fproc(9);
            (*fp).fp_endpoint = 9;
            park(
                fp,
                Parked {
                    kind: TIMERFD,
                    id: 4,
                    op: 0,
                    buf: 0x2000,
                    count: 8,
                },
            );
            assert!(
                take(9, PIPE).is_none(),
                "a timerfd request must not be taken as a pipe one"
            );
            assert!(peek(9).is_some(), "and must still be parked");
            let p = take(9, TIMERFD).expect("taken");
            assert_eq!(p.id, 4);
            assert!(peek(9).is_none());
            assert_eq!((*fp).fp_blocked_on, FP_BLOCKED_ON_NONE);
            restore(9, p);
            assert!(peek(9).is_some());
            assert_eq!((*fp).fp_blocked_on, FP_BLOCKED_ON_PIPE);
        }
    }
}
