//! VFS readiness engine: `select(2)`, `poll(2)`, and the deadline they share.
//!
//! Both calls ask the same question — which of these fds is ready, and wake me
//! when one is (or my timeout expires) — so they share one table of suspended
//! waits. A wait is built from either a triple of fd sets (`select`) or an array
//! of `struct pollfd` (`poll`), scanned once, and either completed immediately or
//! suspended until a driver's `CDEV_SEL2_REPLY` reports readiness or the
//! deadline passes.
//!
//! Readiness per fd:
//! - a regular file or block device is always ready;
//! - a pipe (`S_IFIFO`) is checked against the pipe buffer (`pipe_check`);
//! - a character device is asked (`CDEV_SELECT`), and when nothing is ready the
//!   driver keeps a late watch that answers with `CDEV_SEL2_REPLY`.
//!
//! Deadlines come from `vfs::alarm`: the earliest deadline among the suspended
//! waits is armed as the process's `SYS_SETALARM` timer, and the `CLOCK`
//! notification that fires calls [`alarm_ticks`] to complete every wait whose
//! deadline has passed. Before this, a bounded timeout blocked forever — an
//! event loop with a deadline hung.

use crate::vfs::consts::*;
use crate::vfs::types::*;

/// Maximum number of concurrent suspended waits (`select` + `poll` together).
const MAX_WAITS: usize = 16;

/// Maximum fds one wait may watch. `select` is bounded by `FD_SETSIZE`; `poll`
/// by this. Both fit the per-slot arrays below.
pub const MAX_WATCH: usize = 64;

/// fd_set: 64-bit bitmask, representing fds 0..63.
pub type FdSet = u64;
pub const FD_SETSIZE: usize = 64;

/// Operations a wait watches for (match the drivers' `CDEV_OP_*`).
pub const SEL_RD: u32 = 0x01;
pub const SEL_WR: u32 = 0x02;
pub const SEL_EX: u32 = 0x04;

/// Internal ready bit for a descriptor `poll` was handed that is not open.
/// `select` reports the same condition as `EBADF` instead.
const SEL_NVAL: u32 = 0x40;

/// `poll(2)` event bits (match `<poll.h>`).
pub const POLLIN: i16 = 0x001;
pub const POLLPRI: i16 = 0x002;
pub const POLLOUT: i16 = 0x004;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;

/// Which call a suspended wait belongs to.
const KIND_SELECT: u8 = 0;
const KIND_POLL: u8 = 1;
const KIND_EPOLL: u8 = 2;

#[inline]
pub fn fd_zero(set: &mut FdSet) {
    *set = 0;
}

#[inline]
pub fn fd_set(fd: i32, set: &mut FdSet) {
    if fd >= 0 && (fd as usize) < FD_SETSIZE {
        *set |= 1u64 << fd;
    }
}

#[inline]
pub fn fd_clr(fd: i32, set: &mut FdSet) {
    if fd >= 0 && (fd as usize) < FD_SETSIZE {
        *set &= !(1u64 << fd);
    }
}

#[inline]
pub fn fd_isset(fd: i32, set: &FdSet) -> bool {
    fd >= 0 && (fd as usize) < FD_SETSIZE && (*set & (1u64 << fd)) != 0
}

/// Convert fd_set bits in range [0..nfds) to a SEL_* bitmask for a given fd.
fn tab2ops(fd: i32, nfds: i32, readfds: FdSet, writefds: FdSet, errorfds: FdSet) -> u32 {
    if fd < 0 || fd >= nfds {
        return 0;
    }
    let mut ops = 0u32;
    if fd_isset(fd, &readfds) {
        ops |= SEL_RD;
    }
    if fd_isset(fd, &writefds) {
        ops |= SEL_WR;
    }
    if fd_isset(fd, &errorfds) {
        ops |= SEL_EX;
    }
    ops
}

/// Convert a SEL_* bitmask to ready fd_set bits; returns the count added.
fn ops2tab(
    ops: u32,
    fd: i32,
    readfds: &mut FdSet,
    writefds: &mut FdSet,
    errorfds: &mut FdSet,
) -> i32 {
    let mut count = 0;
    if ops & SEL_RD != 0 {
        fd_set(fd, readfds);
        count += 1;
    }
    if ops & SEL_WR != 0 {
        fd_set(fd, writefds);
        count += 1;
    }
    if ops & SEL_EX != 0 {
        fd_set(fd, errorfds);
        count += 1;
    }
    count
}

/// `poll` events → the SEL_* ops the scanner understands.
fn poll_events_to_ops(events: i16) -> u32 {
    let mut ops = 0u32;
    if events & POLLIN != 0 {
        ops |= SEL_RD;
    }
    if events & POLLOUT != 0 {
        ops |= SEL_WR;
    }
    if events & POLLPRI != 0 {
        ops |= SEL_EX;
    }
    ops
}

/// Received SEL_* ops → `poll` revents.
fn ops_to_poll(ops: u32) -> i16 {
    let mut events = 0i16;
    if ops & SEL_RD != 0 {
        events |= POLLIN;
    }
    if ops & SEL_WR != 0 {
        events |= POLLOUT;
    }
    if ops & SEL_EX != 0 {
        events |= POLLPRI;
    }
    if ops & SEL_NVAL != 0 {
        events |= POLLNVAL;
    }
    events
}

/// `epoll` event bits → the SEL_* ops the scanner understands. `EPOLLIN`,
/// `EPOLLPRI` and `EPOLLOUT` coincide with the `poll` bits, so this mirrors
/// `poll_events_to_ops`; the level-triggering and one-shot flags (`EPOLLET`,
/// `EPOLLONESHOT`, ...) carry no readiness op and are ignored.
fn epoll_events_to_ops(events: u32) -> u32 {
    use crate::vfs::epoll::{EPOLLIN, EPOLLOUT, EPOLLPRI};
    let mut ops = 0u32;
    if events & EPOLLIN != 0 {
        ops |= SEL_RD;
    }
    if events & EPOLLOUT != 0 {
        ops |= SEL_WR;
    }
    if events & EPOLLPRI != 0 {
        ops |= SEL_EX;
    }
    ops
}

/// SEL_* ops → `epoll` event bits.
fn ops_to_epoll(ops: u32) -> u32 {
    use crate::vfs::epoll::{EPOLLIN, EPOLLNVAL, EPOLLOUT, EPOLLPRI};
    let mut events = 0u32;
    if ops & SEL_RD != 0 {
        events |= EPOLLIN;
    }
    if ops & SEL_WR != 0 {
        events |= EPOLLOUT;
    }
    if ops & SEL_EX != 0 {
        events |= EPOLLPRI;
    }
    if ops & SEL_NVAL != 0 {
        events |= EPOLLNVAL;
    }
    events
}

/// One suspended `select`/`poll`.
struct WaitEntry {
    /// Owning fproc (NULL = free slot).
    requestor: *mut Fproc,
    /// Endpoint to reply to when the wait completes.
    req_endpt: i32,
    /// `KIND_SELECT` or `KIND_POLL`.
    kind: u8,
    /// Number of watched fds.
    n: i32,
    /// Watched fd numbers (`select`: `fds[i] == i`).
    fds: [i32; MAX_WATCH],
    /// Requested ops per fd (`SEL_*`).
    want: [u32; MAX_WATCH],
    /// Ready ops accumulated per fd.
    got: [u32; MAX_WATCH],
    /// `poll` keeps the caller's original event mask to echo back.
    pevents: [i16; MAX_WATCH],
    /// `epoll` keeps the caller's opaque `data` to echo back per ready event.
    edata: [u64; MAX_WATCH],
    /// `epoll_wait`'s `maxevents`: the most events written back in one call.
    maxevents: i32,
    /// Character-device minor watched per fd (`u32::MAX` = none), for matching a
    /// later `CDEV_SEL2_REPLY`.
    minor: [u32; MAX_WATCH],
    /// Which VFS-internal object each fd watches: 0 = none, 1 = eventfd,
    /// 2 = timerfd. The object's state is behind the fd, not a driver, so a
    /// change wakes the wait directly (`wake_object`).
    rkind: [u8; MAX_WATCH],
    /// The object id for `rkind`.
    rid: [u32; MAX_WATCH],
    /// User-space pointers to the `select` fd_set buffers.
    vir_readfds: u64,
    vir_writefds: u64,
    vir_errorfds: u64,
    /// User-space pointer to the `poll` `struct pollfd` array.
    vir_pollfds: u64,
    /// Accumulated error (select only; `EBADF`).
    error: i32,
    /// TRUE = the wait should block (timeout != {0} / poll timeout != 0).
    block: bool,
    /// Absolute monotonic-tick deadline, 0 = none.
    deadline: u64,
}

impl WaitEntry {
    const fn new() -> Self {
        Self {
            requestor: core::ptr::null_mut(),
            req_endpt: 0,
            kind: KIND_SELECT,
            n: 0,
            fds: [0; MAX_WATCH],
            want: [0; MAX_WATCH],
            got: [0; MAX_WATCH],
            pevents: [0; MAX_WATCH],
            edata: [0; MAX_WATCH],
            maxevents: 0,
            minor: [u32::MAX; MAX_WATCH],
            rkind: [0; MAX_WATCH],
            rid: [0; MAX_WATCH],
            vir_readfds: 0,
            vir_writefds: 0,
            vir_errorfds: 0,
            vir_pollfds: 0,
            error: 0,
            block: false,
            deadline: 0,
        }
    }

    fn reset(&mut self, requestor: *mut Fproc, req_endpt: i32, kind: u8, n: i32) {
        *self = WaitEntry::new();
        self.requestor = requestor;
        self.req_endpt = req_endpt;
        self.kind = kind;
        self.n = n;
    }
}

use core::cell::UnsafeCell;

struct WaitTable(UnsafeCell<[WaitEntry; MAX_WAITS]>);
unsafe impl Sync for WaitTable {}
impl WaitTable {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { WaitEntry::new() }; MAX_WAITS]))
    }
    fn get(&self) -> *mut [WaitEntry; MAX_WAITS] {
        self.0.get()
    }
}

static WAIT_TABLE: WaitTable = WaitTable::new();

unsafe fn se_slot(i: usize) -> &'static mut WaitEntry {
    unsafe { &mut *(*WAIT_TABLE.get()).as_mut_ptr().add(i) }
}

unsafe fn se_slot_ref(i: usize) -> &'static WaitEntry {
    unsafe { &*(*WAIT_TABLE.get()).as_ptr().add(i) }
}

/// Release a slot (caller has already replied or is returning the result).
unsafe fn free_slot(e: &mut WaitEntry) {
    e.requestor = core::ptr::null_mut();
    e.minor = [u32::MAX; MAX_WATCH];
    e.rkind = [0; MAX_WATCH];
    e.rid = [0; MAX_WATCH];
}

/// Readiness-object kinds recorded in `WaitEntry::rkind`.
const R_EVENTFD: u8 = 1;
const R_TIMERFD: u8 = 2;

/// Check whether a pipe fd is ready for the given ops.
///
/// Readiness is derived from the cached pipe size and the filp table
/// (C `pipe_check` with `notouch`): readable when data is buffered or the
/// last writer has closed; writable when a reader is open and space remains.
fn select_request_pipe(filp: &Filp, ops: u32) -> u32 {
    use crate::vfs::pipe;
    let mut ready = 0u32;
    if ops & SEL_RD != 0 {
        let r = pipe::pipe_check(filp, pipe::READING, 0, 1, true);
        if r != EAGAIN {
            ready |= SEL_RD;
        }
    }
    if ops & SEL_WR != 0 {
        let r = pipe::pipe_check(filp, pipe::WRITING, 0, 1, true);
        if r != EPIPE && r != EAGAIN {
            ready |= SEL_WR;
        }
    }
    ready
}

/// Check a character device fd: `CDEV_SELECT` round-trip with the driver. The
/// driver replies the currently-ready ops (`SEL_* == CDEV_OP_*`). If nothing is
/// ready, the driver has registered a late watch (`CDEV_NOTIFY`) and the minor
/// is recorded so a later `CDEV_SEL2_REPLY` can complete the wait.
fn select_request_char(filp: &Filp, ops: u32, e: &mut WaitEntry, i: usize) -> u32 {
    // The **filp's** device, not the vnode's: a socket's minor is the clone the
    // driver handed back at open, and the driver knows nothing of the base
    // `/dev/uds` minor every socket's vnode still carries (`do_open` records the
    // clone in `filp_dev`; read/write/ioctl already use it).
    let dev = filp.filp_dev;
    if dev == 0 {
        return 0;
    }
    let r = crate::vfs::device::cdev_select(dev, ops as i32);
    if r < 0 {
        // A driver that cannot answer (a broken dmap) reports nothing ready
        // rather than a mask of all-ones.
        return 0;
    }
    let ready = r as u32;
    if ready == 0 && ops != 0 {
        e.minor[i] = dev & 0xFFFF;
    }
    ready
}

/// Scan every watched fd and accumulate its ready ops into `got`. Records the
/// select-only `EBADF` error and stops; `poll` records `POLLNVAL` per fd instead.
unsafe fn scan(fp: *mut Fproc, e: &mut WaitEntry) {
    use crate::vfs::glo::vfs_global;
    let glob = unsafe { &mut *vfs_global() };
    let filp_arr = core::ptr::addr_of_mut!(glob.filp) as *mut Filp;

    for i in 0..e.n as usize {
        if e.got[i] != 0 {
            continue; // already answered (a late driver reply preceded the scan)
        }
        let fd = e.fds[i];
        let ops = e.want[i];
        if fd < 0 || ops == 0 {
            continue;
        }

        let filp_idx = unsafe { (*fp).fp_filp[fd as usize] };
        if filp_idx < 0 {
            if e.kind == KIND_SELECT {
                e.error = EBADF;
                return;
            }
            e.got[i] |= SEL_NVAL;
            continue;
        }
        let filp = unsafe { &*filp_arr.add(filp_idx as usize) };
        let vp = filp.filp_vno;
        if vp.is_null() {
            if e.kind == KIND_SELECT {
                e.error = EBADF;
                return;
            }
            e.got[i] |= SEL_NVAL;
            continue;
        }

        // Fds not opened for the requested direction are immediately ready
        // (an operation would fail instantly — POSIX). filp_mode holds the
        // permission-style R_BIT/W_BIT bits (see do_open).
        let mut want = ops;
        if ops & SEL_RD != 0 && (filp.filp_mode & crate::vfs::protect::R_BIT) == 0 {
            e.got[i] |= SEL_RD;
            want &= !SEL_RD;
        }
        if ops & SEL_WR != 0 && (filp.filp_mode & crate::vfs::protect::W_BIT) == 0 {
            e.got[i] |= SEL_WR;
            want &= !SEL_WR;
        }
        if want == 0 {
            continue;
        }

        // An eventfd computes its readiness from its own counter; VFS is the
        // device, so there is no driver to ask and no minor to watch. Record the
        // id so a later read/write wakes this wait (`wake_eventfd`).
        if crate::vfs::eventfd::is_eventfd(vp) {
            let id = unsafe { (*vp).v_inode_nr };
            e.rkind[i] = R_EVENTFD;
            e.rid[i] = id;
            e.got[i] |= crate::vfs::eventfd::ready(id) & want;
            continue;
        }

        // A timerfd likewise: its readiness is "has it fired", computed here.
        if crate::vfs::timerfd::is_timerfd(vp) {
            let id = unsafe { (*vp).v_inode_nr };
            e.rkind[i] = R_TIMERFD;
            e.rid[i] = id;
            e.got[i] |= crate::vfs::timerfd::ready(id) & want;
            continue;
        }

        let ready = {
            let mode = unsafe { (*vp).v_mode };
            if mode & S_IFIFO != 0 {
                select_request_pipe(filp, want)
            } else if mode & S_IFCHR != 0 {
                select_request_char(filp, want, e, i)
            } else {
                want // regular and block devices: always ready
            }
        };
        e.got[i] |= ready;
    }
}

/// The number of ready fds: for `select`, the count of ready op-bits (a fd ready
/// in two sets counts twice, as the reference does); for `poll`, the number of
/// fds with any revents.
fn count_ready(e: &WaitEntry) -> i32 {
    let mut n = 0;
    for i in 0..e.n as usize {
        if e.kind == KIND_SELECT {
            n += (e.got[i] & (SEL_RD | SEL_WR | SEL_EX)).count_ones() as i32;
        } else if e.got[i] != 0 {
            // `poll` counts fds with any revents; `epoll` counts ready
            // interests (the write-back caps the list at `maxevents`).
            n += 1;
        }
    }
    n
}

/// Write a completed `select`'s ready sets back to the caller.
unsafe fn write_fdsets(e: &WaitEntry) {
    let mut rf: FdSet = 0;
    let mut wf: FdSet = 0;
    let mut ef: FdSet = 0;
    for i in 0..e.n as usize {
        let fd = e.fds[i];
        let ops = e.got[i];
        ops2tab(ops, fd, &mut rf, &mut wf, &mut ef);
    }
    if e.vir_readfds != 0 {
        let _ = user_copy_out(e.req_endpt, e.vir_readfds, &rf.to_le_bytes());
    }
    if e.vir_writefds != 0 {
        let _ = user_copy_out(e.req_endpt, e.vir_writefds, &wf.to_le_bytes());
    }
    if e.vir_errorfds != 0 {
        let _ = user_copy_out(e.req_endpt, e.vir_errorfds, &ef.to_le_bytes());
    }
}

/// Write a completed `poll`'s revents back to the caller's `struct pollfd`
/// array (fd i32, events i16, revents i16 — 8 bytes each).
unsafe fn write_pollfds(e: &WaitEntry) {
    let mut buf = [0u8; MAX_WATCH * 8];
    for i in 0..e.n as usize {
        let off = i * 8;
        buf[off..off + 4].copy_from_slice(&e.fds[i].to_ne_bytes());
        buf[off + 4..off + 6].copy_from_slice(&e.pevents[i].to_ne_bytes());
        buf[off + 6..off + 8].copy_from_slice(&ops_to_poll(e.got[i]).to_ne_bytes());
    }
    if e.vir_pollfds != 0 && e.n > 0 {
        let _ = user_copy_out(e.req_endpt, e.vir_pollfds, &buf[..e.n as usize * 8]);
    }
}

/// Write a completed `epoll_wait`'s ready events back to the caller's packed
/// `struct epoll_event` array (`u32 events`, `u64 data` — 12 bytes each), at
/// most `maxevents` of them, in interest order. Returns the number written.
unsafe fn write_epoll_events(e: &WaitEntry) -> i32 {
    let cap = crate::vfs::epoll::EPOLL_EVENT_SIZE;
    let mut buf = [0u8; MAX_WATCH * crate::vfs::epoll::EPOLL_EVENT_SIZE];
    let mut k = 0usize;
    for i in 0..e.n as usize {
        if k as i32 >= e.maxevents {
            break;
        }
        if e.got[i] == 0 {
            continue;
        }
        let off = k * cap;
        buf[off..off + 4].copy_from_slice(&ops_to_epoll(e.got[i]).to_ne_bytes());
        buf[off + 4..off + 12].copy_from_slice(&e.edata[i].to_ne_bytes());
        k += 1;
    }
    if e.vir_pollfds != 0 && k > 0 {
        let _ = user_copy_out(e.req_endpt, e.vir_pollfds, &buf[..k * cap]);
    }
    k as i32
}

/// Complete a wait: write its results back and return the ready count.
unsafe fn deliver(e: &mut WaitEntry) -> i32 {
    if e.kind == KIND_EPOLL {
        return unsafe { write_epoll_events(e) };
    }
    let n = count_ready(e);
    if e.kind == KIND_POLL {
        unsafe { write_pollfds(e) };
    } else {
        unsafe { write_fdsets(e) };
    }
    n
}

/// Arm the process alarm for the earliest deadline among the suspended waits,
/// or cancel it when none has one.
unsafe fn arm_earliest() {
    let mut min = 0u64;
    for i in 0..MAX_WAITS {
        let e = unsafe { se_slot_ref(i) };
        if !e.requestor.is_null() && e.deadline != 0 && (min == 0 || e.deadline < min) {
            min = e.deadline;
        }
    }
    // A timerfd's expiry is the same kind of deadline and shares the one alarm.
    let t = unsafe { crate::vfs::timerfd::earliest() };
    if t != 0 && (min == 0 || t < min) {
        min = t;
    }
    crate::vfs::alarm::set(min);
}

/// Re-arm the alarm for the earliest deadline across waits and timerfds. Called
/// by `vfs::timerfd` when a timer is set or cleared, so an armed timerfd takes
/// effect without waiting for another event.
///
/// # Safety
///
/// Must be called from VFS dispatch.
pub unsafe fn rearm() {
    unsafe { arm_earliest() };
}

/// Find a free wait slot.
fn free_slot_index() -> Option<usize> {
    (0..MAX_WAITS).find(|&i| unsafe { se_slot_ref(i) }.requestor.is_null())
}

/// Complete a suspended wait and reply to its (blocked) caller. Used by the
/// asynchronous completion paths, where no `handle_work` return will reply.
unsafe fn complete_and_reply(e: &mut WaitEntry) {
    let endpt = e.req_endpt;
    unsafe {
        let n = deliver(e);
        free_slot(e);
        arm_earliest();
        send_reply(endpt, n);
    }
}

/// Perform the `select(nfds, readfds, writefds, errorfds, timeout)` system call.
/// Returns the number of ready fds (copied into the caller's sets), a negative
/// errno, or `SUSPEND` when the caller must block (`select_driver_reply` /
/// `alarm_ticks` send the final reply).
///
/// # Safety
///
/// Must be called from a valid VFS dispatch context (`handle_work`), where
/// `glob.fp` names the caller.
pub unsafe fn do_select() -> i32 {
    use crate::vfs::glo::vfs_global;
    let glob = unsafe { &mut *vfs_global() };

    let nfds = r2_i32(&glob.fs_m_in, SEL_NFDS_OFF);
    if nfds < 0 || nfds > FD_SETSIZE as i32 {
        return EINVAL;
    }
    let rdfds_p = r2_u64(&glob.fs_m_in, SEL_RDFDS_OFF);
    let wrfds_p = r2_u64(&glob.fs_m_in, SEL_WRFDS_OFF);
    let exfds_p = r2_u64(&glob.fs_m_in, SEL_EXFDS_OFF);
    let timeout_p = r2_u64(&glob.fs_m_in, SEL_TIMEOUT_OFF);

    let fp = match unsafe { glob.fp.as_mut() } {
        Some(fp) => fp as *mut Fproc,
        None => return EINVAL,
    };
    let caller_ep = unsafe { (*fp).fp_endpoint };

    // Timeout: NULL → block forever; {0,0} → poll; otherwise a real deadline.
    let mut block = true;
    let mut deadline = 0u64;
    if timeout_p != 0 {
        let mut tv = [0u8; 8];
        if unsafe { user_copy_in(caller_ep, timeout_p, &mut tv) } != 0 {
            return EFAULT;
        }
        let sec = i32::from_le_bytes([tv[0], tv[1], tv[2], tv[3]]);
        let usec = i32::from_le_bytes([tv[4], tv[5], tv[6], tv[7]]);
        if sec == 0 && usec == 0 {
            block = false;
        } else {
            deadline = crate::vfs::alarm::deadline_timeval(sec, usec);
        }
    }

    // Copy the fd sets from the caller (NULL pointers = empty set).
    let mut readfds: FdSet = 0;
    let mut writefds: FdSet = 0;
    let mut errorfds: FdSet = 0;
    if rdfds_p != 0 && unsafe { user_copy_in(caller_ep, rdfds_p, as_bytes_mut(&mut readfds)) } != 0
    {
        return EFAULT;
    }
    if wrfds_p != 0 && unsafe { user_copy_in(caller_ep, wrfds_p, as_bytes_mut(&mut writefds)) } != 0
    {
        return EFAULT;
    }
    if exfds_p != 0 && unsafe { user_copy_in(caller_ep, exfds_p, as_bytes_mut(&mut errorfds)) } != 0
    {
        return EFAULT;
    }

    let Some(slot_idx) = free_slot_index() else {
        return ENOMEM;
    };
    let e = unsafe { se_slot(slot_idx) };
    e.reset(fp, caller_ep, KIND_SELECT, nfds);
    e.vir_readfds = rdfds_p;
    e.vir_writefds = wrfds_p;
    e.vir_errorfds = exfds_p;
    e.block = block;
    e.deadline = deadline;
    for fd in 0..nfds {
        e.fds[fd as usize] = fd;
        e.want[fd as usize] = tab2ops(fd, nfds, readfds, writefds, errorfds);
    }

    unsafe { scan(fp, e) };

    if e.error != OK {
        let err = e.error;
        unsafe { free_slot(e) };
        return err;
    }
    if count_ready(e) > 0 || !e.block {
        let n = unsafe { deliver(e) };
        unsafe { free_slot(e) };
        return n;
    }

    // Nothing ready and blocking: suspend the caller. The entry stays; a
    // driver's `CDEV_SEL2_REPLY` or the alarm (`alarm_ticks`) sends the result.
    unsafe { arm_earliest() };
    unsafe {
        (*fp).fp_blocked_on = FP_BLOCKED_ON_SELECT;
    }
    SUSPEND
}

/// Perform the `poll(fds, nfds, timeout)` system call. `timeout` is in
/// milliseconds, `-1` blocks forever, `0` polls. Returns the number of ready
/// fds (revents written into the caller's array), a negative errno, or
/// `SUSPEND` when the caller must block.
///
/// # Safety
///
/// Must be called from a valid VFS dispatch context, where `glob.fp` names the
/// caller.
pub unsafe fn do_poll() -> i32 {
    use crate::vfs::glo::vfs_global;
    let glob = unsafe { &mut *vfs_global() };

    let fds_p = r2_u64(&glob.fs_m_in, POLL_FDS_OFF);
    let nfds = r2_i32(&glob.fs_m_in, POLL_NFDS_OFF);
    let timeout_ms = r2_i32(&glob.fs_m_in, POLL_TIMEOUT_OFF);
    // `nfds == 0` with a null array is a valid "just wait" poll.
    if !(0..=MAX_WATCH as i32).contains(&nfds) || (nfds > 0 && fds_p == 0) {
        return EINVAL;
    }

    let fp = match unsafe { glob.fp.as_mut() } {
        Some(fp) => fp as *mut Fproc,
        None => return EINVAL,
    };
    let caller_ep = unsafe { (*fp).fp_endpoint };

    // Copy the pollfd array in (8 bytes each).
    let mut buf = [0u8; MAX_WATCH * 8];
    if nfds > 0 {
        let bytes = &mut buf[..nfds as usize * 8];
        if unsafe { user_copy_in(caller_ep, fds_p, bytes) } != 0 {
            return EFAULT;
        }
    }

    let mut block = true;
    let mut deadline = 0u64;
    if timeout_ms == 0 {
        block = false;
    } else if timeout_ms > 0 {
        deadline = crate::vfs::alarm::deadline_ms(timeout_ms);
    }

    let Some(slot_idx) = free_slot_index() else {
        return ENOMEM;
    };
    let e = unsafe { se_slot(slot_idx) };
    e.reset(fp, caller_ep, KIND_POLL, nfds);
    e.vir_pollfds = fds_p;
    e.block = block;
    e.deadline = deadline;
    for i in 0..nfds as usize {
        let off = i * 8;
        let fd = i32::from_ne_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]));
        let events = i16::from_ne_bytes(buf[off + 4..off + 6].try_into().unwrap_or([0; 2]));
        e.fds[i] = fd;
        e.pevents[i] = events;
        e.want[i] = poll_events_to_ops(events);
    }

    unsafe { scan(fp, e) };

    if count_ready(e) > 0 || !e.block {
        let n = unsafe { deliver(e) };
        unsafe { free_slot(e) };
        return n;
    }

    unsafe { arm_earliest() };
    unsafe {
        (*fp).fp_blocked_on = FP_BLOCKED_ON_SELECT;
    }
    SUSPEND
}

/// Perform the `epoll_wait(epfd, events, maxevents, timeout)` system call.
/// Builds one wait entry from the instance's persistent interest set and scans
/// it exactly as `poll` scans its array, so a driver's `CDEV_SEL2_REPLY`, a
/// VFS-internal object's `wake_*`, or the shared deadline completes it.
/// `maxevents` caps the events written back; `timeout` is milliseconds (`-1`
/// blocks forever, `0` polls). Returns the number of ready events, a negative
/// errno, or `SUSPEND`.
///
/// # Safety
///
/// Must be called from a valid VFS dispatch context, where `glob.fp` names the
/// caller.
pub unsafe fn do_epoll_wait() -> i32 {
    use crate::vfs::glo::vfs_global;
    let glob = unsafe { &mut *vfs_global() };

    let epfd = r2_i32(&glob.fs_m_in, EPOLL_WAIT_EPFD_OFF);
    let maxevents = r2_i32(&glob.fs_m_in, EPOLL_WAIT_MAXEVENTS_OFF);
    let timeout_ms = r2_i32(&glob.fs_m_in, EPOLL_WAIT_TIMEOUT_OFF);
    let events_p = r2_u64(&glob.fs_m_in, EPOLL_WAIT_EVENTS_OFF);
    if maxevents <= 0 || events_p == 0 {
        return EINVAL;
    }

    let fp = match unsafe { glob.fp.as_mut() } {
        Some(fp) => fp as *mut Fproc,
        None => return EINVAL,
    };
    let caller_ep = unsafe { (*fp).fp_endpoint };

    let epid = match unsafe { crate::vfs::epoll::id_of(epfd) } {
        Ok(id) => id,
        Err(e) => return e,
    };
    // Drop interests whose fd the caller has closed: a stale registration would
    // report EPOLLNVAL on every level-triggered re-scan and spin the loop.
    unsafe { crate::vfs::epoll::prune(epid) };

    let Some(slot_idx) = free_slot_index() else {
        return ENOMEM;
    };
    let mut interests = [crate::vfs::epoll::Interest::new(); MAX_WATCH];
    let n = match unsafe { crate::vfs::epoll::snapshot(epid, &mut interests) } {
        Some(n) => n,
        None => return EBADF,
    };

    let e = unsafe { se_slot(slot_idx) };
    e.reset(fp, caller_ep, KIND_EPOLL, n as i32);
    e.vir_pollfds = events_p;
    e.maxevents = maxevents;
    for (i, it) in interests.iter().enumerate().take(n) {
        e.fds[i] = it.fd;
        e.want[i] = epoll_events_to_ops(it.events);
        e.edata[i] = it.data;
    }

    e.block = true;
    if timeout_ms == 0 {
        e.block = false;
    } else if timeout_ms > 0 {
        e.deadline = crate::vfs::alarm::deadline_ms(timeout_ms);
    }

    unsafe { scan(fp, e) };

    if count_ready(e) > 0 || !e.block {
        let n = unsafe { deliver(e) };
        unsafe { free_slot(e) };
        return n;
    }

    unsafe { arm_earliest() };
    unsafe {
        (*fp).fp_blocked_on = FP_BLOCKED_ON_SELECT;
    }
    SUSPEND
}

/// A driver reports readiness for a watched minor (`CDEV_SEL1_REPLY` /
/// `CDEV_SEL2_REPLY`; `status` = the ops that became ready). Matches the wait
/// watching that minor, marks the ops ready, and — when the wait can complete —
/// copies the results back and replies to the caller.
///
/// Returns `OK` if a wait consumed the reply; `ENOENT` for a stray reply (the
/// wait already completed — its driver watch was not cancelled, `cdev_cancel` is
/// not implemented yet).
///
/// # Safety
///
/// Must be called from the VFS main loop (driver reply dispatch).
pub unsafe fn select_driver_reply(minor: u32, status: i32) -> i32 {
    let ops = (status as u32) & (SEL_RD | SEL_WR | SEL_EX);
    for i in 0..MAX_WAITS {
        let e = unsafe { se_slot(i) };
        if e.requestor.is_null() {
            continue;
        }
        for fd in 0..e.n as usize {
            if e.minor[fd] == minor {
                e.minor[fd] = u32::MAX;
                e.got[fd] |= ops & e.want[fd];
                if count_ready(e) > 0 || !e.block {
                    unsafe { complete_and_reply(e) };
                }
                return OK;
            }
        }
    }
    ENOENT
}

/// Re-ask every suspended wait's unanswered descriptors whether they are ready,
/// and complete the ones that now are.
///
/// This is the pull half of char-driver readiness, and it exists because the
/// push half cannot be relied on: a driver can only *send* VFS a readiness
/// report while VFS is blocked in `RECEIVE`, and a short retry alarm can sit
/// permanently out of phase with VFS's own deadline tick — measured on the
/// `uds` path, where VFS was awake at every retry and `SENDNB` answered
/// `ENOTREADY` for the whole run (`KNOWN_ISSUES` 36). A driver therefore ends
/// such a report with a `NOTIFY`, which the kernel remembers for a destination
/// that is not receiving; VFS re-asks here, on that notification. Readiness is
/// level-triggered, so nothing has to survive the notification.
///
/// # Safety
///
/// Must be called from VFS dispatch.
pub unsafe fn rescan_suspended() {
    for i in 0..MAX_WAITS {
        let e = unsafe { se_slot(i) };
        let fp = e.requestor;
        if fp.is_null() || !e.block {
            continue;
        }
        unsafe { scan(fp, e) };
        if count_ready(e) > 0 {
            unsafe { complete_and_reply(e) };
        }
    }
}

/// A VFS-internal object's state changed: mark every suspended wait that watches
/// it and complete the ones now satisfiable. `kind`/`id` name the object and
/// `ops` its ready ops. There is no driver reply because VFS *is* the device.
unsafe fn wake_object(kind: u8, id: u32, ops: u32) {
    for i in 0..MAX_WAITS {
        let e = unsafe { se_slot(i) };
        if e.requestor.is_null() {
            continue;
        }
        let mut touched = false;
        for j in 0..e.n as usize {
            if e.rkind[j] == kind && e.rid[j] == id {
                let add = ops & e.want[j];
                if add != 0 {
                    e.got[j] |= add;
                    touched = true;
                }
            }
        }
        if touched && (count_ready(e) > 0 || !e.block) {
            unsafe { complete_and_reply(e) };
        }
    }
}

/// An eventfd's counter changed (a read drained it, or a write filled it): wake
/// the suspended waits watching that object.
///
/// # Safety
///
/// Must be called from VFS dispatch.
pub unsafe fn wake_eventfd(id: u32) {
    let ops = crate::vfs::eventfd::ready(id);
    unsafe { wake_object(R_EVENTFD, id, ops) };
}

/// A timerfd fired: wake the suspended waits watching that object.
///
/// # Safety
///
/// Must be called from VFS dispatch.
pub unsafe fn wake_timerfd(id: u32) {
    let ops = crate::vfs::timerfd::ready(id);
    unsafe { wake_object(R_TIMERFD, id, ops) };
}

/// The `CLOCK` alarm fired: complete every suspended wait whose deadline has
/// passed (the ready sets hold whatever became ready, often nothing), then
/// re-arm for the next-earliest deadline. Called from the VFS main loop's
/// notification branch.
///
/// # Safety
///
/// Must be called from the VFS main loop.
pub unsafe fn alarm_ticks() {
    unsafe { expire_at(crate::vfs::alarm::now()) };
}

/// Complete every wait whose deadline is at or before `now`, then re-arm. Split
/// from [`alarm_ticks`] so the host tests can drive expiry without a clock.
unsafe fn expire_at(now: u64) {
    for i in 0..MAX_WAITS {
        let e = unsafe { se_slot(i) };
        if e.requestor.is_null() {
            continue;
        }
        if e.deadline != 0 && now >= e.deadline {
            unsafe { complete_and_reply(e) };
        }
    }
    // Timerfds share the alarm: advance any that are due before re-arming.
    unsafe { crate::vfs::timerfd::expire(now) };
    unsafe { arm_earliest() };
}

fn r2_i32(buf: &[u8; 64], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn r2_u64(buf: &[u8; 64], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}

unsafe fn as_bytes_mut(v: &mut FdSet) -> &mut [u8] {
    unsafe { core::slice::from_raw_parts_mut((v as *mut FdSet) as *mut u8, 8) }
}

// User-memory copies go through SYS_VIRCOPY (VFS is a separate address
// space). Host builds have no user spaces; the host tests avoid the copy
// paths (fd_set pointers are NULL in the tests).

#[cfg(target_os = "minix")]
unsafe fn user_copy_in(endpt: i32, src: u64, dst: &mut [u8]) -> i32 {
    unsafe {
        crate::vfs::call::sys_vircopy(
            endpt,
            src,
            crate::vfs::call::SELF,
            dst.as_mut_ptr() as u64,
            dst.len(),
        )
    }
}

#[cfg(target_os = "minix")]
unsafe fn user_copy_out(endpt: i32, dst: u64, src: &[u8]) -> i32 {
    unsafe {
        crate::vfs::call::sys_vircopy(
            crate::vfs::call::SELF,
            src.as_ptr() as u64,
            endpt,
            dst,
            src.len(),
        )
    }
}

#[cfg(not(target_os = "minix"))]
unsafe fn user_copy_in(_endpt: i32, _src: u64, _dst: &mut [u8]) -> i32 {
    -1
}

#[cfg(not(target_os = "minix"))]
unsafe fn user_copy_out(_endpt: i32, _dst: u64, _src: &[u8]) -> i32 {
    -1
}

/// Send the final select/poll result to the caller (blocked in `sendrec(VFS)`).
/// VFS reply convention: result in m_type @ 4 (matches `main.rs reply()`).
#[cfg(target_os = "minix")]
unsafe fn send_reply(endpt: i32, result: i32) {
    let mut out = [0u8; 64];
    out[4..8].copy_from_slice(&result.to_le_bytes());
    const SEND_CALL: u64 = 46;
    if endpt >= 0 {
        let _ = minix_rt::syscall2(SEND_CALL, endpt as u64, out.as_mut_ptr() as u64);
    }
}

#[cfg(not(target_os = "minix"))]
unsafe fn send_reply(_endpt: i32, _result: i32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fd_zero() {
        let mut s: FdSet = !0;
        fd_zero(&mut s);
        assert_eq!(s, 0);
    }

    #[test]
    fn test_fd_set_isset() {
        let mut s: FdSet = 0;
        fd_set(0, &mut s);
        fd_set(5, &mut s);
        assert!(fd_isset(0, &s));
        assert!(!fd_isset(1, &s));
        assert!(fd_isset(5, &s));
    }

    #[test]
    fn test_fd_clr() {
        let mut s: FdSet = 0;
        fd_set(3, &mut s);
        fd_clr(3, &mut s);
        assert!(!fd_isset(3, &s));
    }

    #[test]
    fn test_fd_set_out_of_range() {
        let mut s: FdSet = 0;
        fd_set(100, &mut s);
        assert_eq!(s, 0);
        fd_set(-1, &mut s);
        assert_eq!(s, 0);
    }

    #[test]
    fn test_tab2ops_read() {
        let r: FdSet = 1u64 << 3;
        assert_eq!(tab2ops(3, 10, r, 0, 0), SEL_RD);
    }

    #[test]
    fn test_tab2ops_write() {
        let w: FdSet = 1u64 << 7;
        assert_eq!(tab2ops(7, 10, 0, w, 0), SEL_WR);
    }

    #[test]
    fn test_tab2ops_all() {
        let r: FdSet = 1u64 << 1;
        let w: FdSet = 1u64 << 1;
        let e: FdSet = 1u64 << 1;
        assert_eq!(tab2ops(1, 10, r, w, e), SEL_RD | SEL_WR | SEL_EX);
    }

    #[test]
    fn test_tab2ops_out_of_range() {
        assert_eq!(tab2ops(10, 10, !0, !0, !0), 0);
        assert_eq!(tab2ops(-1, 10, !0, 0, 0), 0);
    }

    #[test]
    fn test_ops2tab() {
        let mut rr: FdSet = 0;
        let mut wr: FdSet = 0;
        let mut er: FdSet = 0;
        let n = ops2tab(SEL_RD | SEL_WR, 3, &mut rr, &mut wr, &mut er);
        assert_eq!(n, 2);
        assert!(fd_isset(3, &rr));
        assert!(fd_isset(3, &wr));
        assert!(!fd_isset(3, &er));
    }

    #[test]
    fn poll_event_roundtrip() {
        assert_eq!(poll_events_to_ops(POLLIN), SEL_RD);
        assert_eq!(poll_events_to_ops(POLLOUT), SEL_WR);
        assert_eq!(poll_events_to_ops(POLLPRI), SEL_EX);
        assert_eq!(ops_to_poll(SEL_RD), POLLIN);
        assert_eq!(ops_to_poll(SEL_WR), POLLOUT);
        assert_eq!(ops_to_poll(SEL_NVAL), POLLNVAL);
    }

    #[test]
    fn count_ready_counts_poll_fds_and_select_ops() {
        let mut e = WaitEntry::new();
        e.kind = KIND_POLL;
        e.n = 3;
        e.got[0] = SEL_RD;
        e.got[2] = SEL_NVAL;
        assert_eq!(count_ready(&e), 2);

        let mut s = WaitEntry::new();
        s.kind = KIND_SELECT;
        s.n = 2;
        s.got[0] = SEL_RD | SEL_WR;
        assert_eq!(count_ready(&s), 2);
    }

    #[test]
    fn test_select_driver_reply_stray_returns_enoent() {
        // No entry watches the minor → the reply is stray (ENOENT).
        unsafe {
            assert_eq!(select_driver_reply(5, SEL_RD as i32), ENOENT);
        }
    }

    #[test]
    fn test_select_driver_reply_marks_ready_and_replies() {
        // A blocking entry watching minor 3 on fd 2: a CDEV_SEL2_REPLY with
        // SEL_RD marks fd 2 readable and completes the wait (the host reply
        // seam is a no-op; we assert the entry is freed and the result set).
        unsafe {
            let e = se_slot(0);
            let mut fp = Fproc {
                fp_endpoint: 42,
                ..Default::default()
            };
            e.reset(&mut fp as *mut Fproc, 42, KIND_SELECT, 3);
            e.block = true;
            e.want[2] = SEL_RD;
            e.minor[2] = 3; // fd 2 watches minor 3
            e.vir_readfds = 0; // no user copy on host
            e.vir_writefds = 0;
            e.vir_errorfds = 0;

            let r = select_driver_reply(3, SEL_RD as i32);
            assert_eq!(r, OK);
            assert!(e.requestor.is_null(), "entry freed after reply");
            // The slot is clean for the next test.
            e.reset(core::ptr::null_mut(), 0, KIND_SELECT, 0);
        }
    }

    #[test]
    fn expired_deadline_frees_the_slot() {
        // A suspended wait whose deadline has passed is completed with whatever
        // is ready (nothing) and its slot released.
        unsafe {
            let e = se_slot(0);
            let mut fp = Fproc {
                fp_endpoint: 7,
                ..Default::default()
            };
            e.reset(&mut fp as *mut Fproc, 7, KIND_POLL, 1);
            e.block = true;
            e.deadline = 5;
            e.vir_pollfds = 0;
            expire_at(5);
            assert!(e.requestor.is_null(), "expired wait freed");
            e.reset(core::ptr::null_mut(), 0, KIND_SELECT, 0);
        }
    }

    #[test]
    fn a_future_deadline_is_not_expired() {
        unsafe {
            let e = se_slot(0);
            let mut fp = Fproc {
                fp_endpoint: 8,
                ..Default::default()
            };
            e.reset(&mut fp as *mut Fproc, 8, KIND_POLL, 1);
            e.block = true;
            e.deadline = 100;
            expire_at(99);
            assert!(!e.requestor.is_null(), "wait with time left stays");
            e.reset(core::ptr::null_mut(), 0, KIND_SELECT, 0);
        }
    }
}
