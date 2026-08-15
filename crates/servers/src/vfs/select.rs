//! VFS `select()` implementation — ported from `minix/servers/vfs/select.c`.
//!
//! `select(nfds, readfds, writefds, errorfds, timeout)`:
//! - copies the fd sets from the caller (via `sys_vircopy` — VFS is a
//!   separate address space and never dereferences user pointers),
//! - checks each fd: regular files are always ready, pipes via
//!   `pipe_check`, character devices via a `CDEV_SELECT` round-trip (the
//!   driver replies the currently-ready ops and may register a late watch),
//! - if anything is ready (or the caller asked to poll), the ready sets are
//!   copied back and the count returned;
//! - otherwise the caller is suspended (`SUSPEND`, no reply) until a
//!   driver's `CDEV_SEL2_REPLY` reports readiness, at which point
//!   `select_driver_reply` sends the result to the caller.
//!
//! Timeouts: `timeout == NULL` blocks forever; `{0,0}` polls. A bounded
//! timeout currently behaves as forever — the kernel has no server timer
//! API (`SYS_SETALARM` gap, OPEN_ITEMS A1 deferral), so no wakeup can
//! fire.

use crate::vfs::consts::*;
use crate::vfs::types::*;

/// Maximum number of concurrent `select()` calls.
const MAX_SELECTS: usize = 16;

/// fd_set: 64-bit bitmask, representing fds 0..63.
pub type FdSet = u64;
pub const FD_SETSIZE: usize = 64;

/// Operations that `select()` watches for (match the drivers' `CDEV_OP_*`).
pub const SEL_RD: u32 = 0x01;
pub const SEL_WR: u32 = 0x02;
pub const SEL_EX: u32 = 0x04;

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

struct SelectEntry {
    /// Owning fproc (NULL = free slot).
    requestor: *mut Fproc,
    /// Endpoint to reply to when the select completes.
    req_endpt: i32,
    /// Requested fd sets (as received from user).
    readfds: FdSet,
    writefds: FdSet,
    errorfds: FdSet,
    /// Accumulated ready fd sets.
    ready_readfds: FdSet,
    ready_writefds: FdSet,
    ready_errorfds: FdSet,
    /// User-space pointers to fd_set buffers.
    vir_readfds: u64,
    vir_writefds: u64,
    vir_errorfds: u64,
    /// Number of fds checked (nfds argument).
    nfds: i32,
    /// Number of ready fds.
    nreadyfds: i32,
    /// Accumulated error.
    error: i32,
    /// TRUE = select should block (timeout != {0,0} or NULL).
    block: bool,
    /// Character-device minor watched per fd (`u32::MAX` = none) — matches
    /// a later `CDEV_SEL2_REPLY` to this fd.
    char_minor: [u32; FD_SETSIZE],
}

impl SelectEntry {
    const fn new() -> Self {
        Self {
            requestor: core::ptr::null_mut(),
            req_endpt: 0,
            readfds: 0,
            writefds: 0,
            errorfds: 0,
            ready_readfds: 0,
            ready_writefds: 0,
            ready_errorfds: 0,
            vir_readfds: 0,
            vir_writefds: 0,
            vir_errorfds: 0,
            nfds: 0,
            nreadyfds: 0,
            error: 0,
            block: false,
            char_minor: [u32::MAX; FD_SETSIZE],
        }
    }
}

use core::cell::UnsafeCell;

struct SelectTable(UnsafeCell<[SelectEntry; MAX_SELECTS]>);
unsafe impl Sync for SelectTable {}
impl SelectTable {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { SelectEntry::new() }; MAX_SELECTS]))
    }
    fn get(&self) -> *mut [SelectEntry; MAX_SELECTS] {
        self.0.get()
    }
}

static SELECT_TABLE: SelectTable = SelectTable::new();

unsafe fn se_slot(i: usize) -> &'static mut SelectEntry {
    unsafe { &mut *(*SELECT_TABLE.get()).as_mut_ptr().add(i) }
}

unsafe fn se_slot_ref(i: usize) -> &'static SelectEntry {
    unsafe { &*(*SELECT_TABLE.get()).as_ptr().add(i) }
}

/// Check whether a pipe fd is ready for the given ops.
fn select_request_pipe(filp: &Filp, ops: u32) -> u32 {
    use crate::vfs::pipe;
    if !pipe::is_pipe_filp(filp.filp_pipe_ino) {
        return 0;
    }
    let pipe_idx = pipe::pipe_index_from_filp(filp.filp_pipe_ino);
    let mut ready = 0u32;
    if let Some(p) = pipe::get_pipe(pipe_idx) {
        let (readers, writers) = pipe::pipe_refcounts(pipe_idx);
        if ops & SEL_RD != 0 && (!p.is_empty() || writers == 0) {
            ready |= SEL_RD;
        }
        if ops & SEL_WR != 0 && (!p.is_full() && readers > 0) {
            ready |= SEL_WR;
        }
    }
    ready
}

/// Check a character device fd: `CDEV_SELECT` round-trip with the driver.
/// The driver replies the currently-ready ops (`SEL_* == CDEV_OP_*`). If
/// nothing is ready, the driver has registered a late watch (`CDEV_NOTIFY`)
/// and the minor is recorded so a later `CDEV_SEL2_REPLY` can complete the
/// select.
fn select_request_char(filp: &Filp, ops: u32, se: &mut SelectEntry, fd: i32) -> u32 {
    let vp = filp.filp_vno;
    if vp.is_null() {
        return 0;
    }
    let dev = unsafe { (*vp).v_dev };
    let ready = crate::vfs::device::cdev_select(dev, ops as i32) as u32;
    if ready == 0 && ops != 0 {
        se.char_minor[fd as usize] = dev & 0xFFFF;
    }
    ready
}

/// Perform the `select(nfds, readfds, writefds, errorfds, timeout)` system
/// call. Returns the number of ready fds (copied into the caller's sets),
/// a negative errno, or `SUSPEND` when the caller must block until a
/// driver reports readiness (`select_driver_reply` sends the final reply).
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
        Some(fp) => fp,
        None => return EINVAL,
    };
    let caller_ep = fp.fp_endpoint;

    // Timeout: NULL → block forever; {0,0} → poll; bounded → behaves as
    // forever (no server timer API to fire a wakeup).
    let mut block = true;
    if timeout_p != 0 {
        let mut tv = [0u8; 8];
        if user_copy_in(caller_ep, timeout_p, &mut tv) != 0 {
            return EFAULT;
        }
        let sec = i32::from_le_bytes([tv[0], tv[1], tv[2], tv[3]]);
        let usec = i32::from_le_bytes([tv[4], tv[5], tv[6], tv[7]]);
        if sec == 0 && usec == 0 {
            block = false;
        }
    }

    // Copy the fd sets from the caller (NULL pointers = empty set).
    let mut readfds: FdSet = 0;
    let mut writefds: FdSet = 0;
    let mut errorfds: FdSet = 0;
    if rdfds_p != 0 && user_copy_in(caller_ep, rdfds_p, as_bytes_mut(&mut readfds)) != 0 {
        return EFAULT;
    }
    if wrfds_p != 0 && user_copy_in(caller_ep, wrfds_p, as_bytes_mut(&mut writefds)) != 0 {
        return EFAULT;
    }
    if exfds_p != 0 && user_copy_in(caller_ep, exfds_p, as_bytes_mut(&mut errorfds)) != 0 {
        return EFAULT;
    }

    // Find a free select slot.
    let mut slot_idx = MAX_SELECTS;
    for i in 0..MAX_SELECTS {
        if unsafe { se_slot_ref(i) }.requestor.is_null() {
            slot_idx = i;
            break;
        }
    }
    if slot_idx >= MAX_SELECTS {
        return ENOMEM;
    }

    let se = unsafe { se_slot(slot_idx) };
    se.requestor = fp;
    se.req_endpt = caller_ep;
    se.nfds = nfds;
    se.readfds = readfds;
    se.writefds = writefds;
    se.errorfds = errorfds;
    se.vir_readfds = rdfds_p;
    se.vir_writefds = wrfds_p;
    se.vir_errorfds = exfds_p;
    se.ready_readfds = 0;
    se.ready_writefds = 0;
    se.ready_errorfds = 0;
    se.nreadyfds = 0;
    se.error = OK;
    se.block = block;
    se.char_minor = [u32::MAX; FD_SETSIZE];

    // Check each fd for readiness.
    for fd in 0..nfds {
        let ops = tab2ops(fd, nfds, readfds, writefds, errorfds);
        if ops == 0 {
            continue;
        }

        let filp_idx = fp.fp_filp[fd as usize];
        if filp_idx < 0 {
            se.error = EBADF;
            break;
        }
        let filp_arr = core::ptr::addr_of_mut!(glob.filp) as *mut Filp;
        let filp = unsafe { &*filp_arr.add(filp_idx as usize) };
        let vp = filp.filp_vno;
        if vp.is_null() {
            se.error = EBADF;
            break;
        }

        // Fds not opened for the requested direction are immediately ready
        // (an operation would fail instantly — POSIX).
        let mut want = ops;
        if ops & SEL_RD != 0 && (filp.filp_mode & 1) == 0 {
            se.nreadyfds += ops2tab(
                SEL_RD,
                fd,
                &mut se.ready_readfds,
                &mut se.ready_writefds,
                &mut se.ready_errorfds,
            );
            want &= !SEL_RD;
        }
        if ops & SEL_WR != 0 && (filp.filp_mode & 2) == 0 {
            se.nreadyfds += ops2tab(
                SEL_WR,
                fd,
                &mut se.ready_readfds,
                &mut se.ready_writefds,
                &mut se.ready_errorfds,
            );
            want &= !SEL_WR;
        }
        if want == 0 {
            continue;
        }

        let ready_ops = if crate::vfs::pipe::is_pipe_filp(filp.filp_pipe_ino) {
            select_request_pipe(filp, want)
        } else {
            let mode = unsafe { (*vp).v_mode };
            if mode & S_IFCHR != 0 {
                select_request_char(filp, want, se, fd)
            } else {
                want // regular and block devices: always ready
            }
        };

        if ready_ops != 0 {
            se.nreadyfds += ops2tab(
                ready_ops,
                fd,
                &mut se.ready_readfds,
                &mut se.ready_writefds,
                &mut se.ready_errorfds,
            );
        }
    }

    if se.error != OK {
        let e = se.error;
        se.requestor = core::ptr::null_mut();
        return e;
    }
    if se.nreadyfds > 0 || !se.block {
        write_results(se);
        let n = se.nreadyfds;
        se.requestor = core::ptr::null_mut();
        return n;
    }

    // Nothing ready and blocking: suspend the caller. The entry stays; a
    // driver's `CDEV_SEL2_REPLY` (select_driver_reply) sends the result.
    fp.fp_blocked_on = FP_BLOCKED_ON_SELECT;
    SUSPEND
}

/// A driver reports readiness for a watched minor (`CDEV_SEL1_REPLY` /
/// `CDEV_SEL2_REPLY`; `status` = the ops that became ready). Matches the
/// select entry watching that minor, marks the ops ready, and — when the
/// select can complete — copies the results back and replies to the caller.
///
/// Returns `OK` if an entry consumed the reply; `ENOENT` for a stray reply
/// (the select already completed — its driver watch was not cancelled,
/// `cdev_cancel` is not implemented yet).
///
/// # Safety
///
/// Must be called from the VFS main loop (driver reply dispatch).
pub unsafe fn select_driver_reply(minor: u32, status: i32) -> i32 {
    let ops = status as u32;
    for i in 0..MAX_SELECTS {
        let se = se_slot(i);
        if se.requestor.is_null() {
            continue;
        }
        for fd in 0..se.nfds {
            if se.char_minor[fd as usize] == minor {
                let matched = ops & (SEL_RD | SEL_WR | SEL_EX);
                if matched != 0 {
                    se.nreadyfds += ops2tab(
                        matched,
                        fd,
                        &mut se.ready_readfds,
                        &mut se.ready_writefds,
                        &mut se.ready_errorfds,
                    );
                }
                if se.nreadyfds > 0 || !se.block {
                    write_results(se);
                    let n = se.nreadyfds;
                    let endpt = se.req_endpt;
                    se.requestor = core::ptr::null_mut();
                    send_reply(endpt, n);
                }
                return OK;
            }
        }
    }
    ENOENT
}

/// Copy a completed select's results back to the user and free the slot.
unsafe fn write_results(se: &SelectEntry) {
    if se.vir_readfds != 0 {
        let _ = user_copy_out(
            se.req_endpt,
            se.vir_readfds,
            &se.ready_readfds.to_le_bytes(),
        );
    }
    if se.vir_writefds != 0 {
        let _ = user_copy_out(
            se.req_endpt,
            se.vir_writefds,
            &se.ready_writefds.to_le_bytes(),
        );
    }
    if se.vir_errorfds != 0 {
        let _ = user_copy_out(
            se.req_endpt,
            se.vir_errorfds,
            &se.ready_errorfds.to_le_bytes(),
        );
    }
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
    crate::vfs::call::sys_vircopy(
        endpt,
        src,
        crate::vfs::call::SELF,
        dst.as_mut_ptr() as u64,
        dst.len(),
    )
}

#[cfg(target_os = "minix")]
unsafe fn user_copy_out(endpt: i32, dst: u64, src: &[u8]) -> i32 {
    crate::vfs::call::sys_vircopy(
        crate::vfs::call::SELF,
        src.as_ptr() as u64,
        endpt,
        dst,
        src.len(),
    )
}

#[cfg(not(target_os = "minix"))]
unsafe fn user_copy_in(_endpt: i32, _src: u64, _dst: &mut [u8]) -> i32 {
    -1
}

#[cfg(not(target_os = "minix"))]
unsafe fn user_copy_out(_endpt: i32, _dst: u64, _src: &[u8]) -> i32 {
    -1
}

/// Send the final select result to the caller (blocked in `sendrec(VFS)`).
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
    fn test_select_driver_reply_stray_returns_enoent() {
        // No entry watches the minor → the reply is stray (ENOENT).
        unsafe {
            assert_eq!(select_driver_reply(5, SEL_RD as i32), ENOENT);
        }
    }

    #[test]
    fn test_select_driver_reply_marks_ready_and_replies() {
        // A blocking entry watching minor 3 on fd 2: a CDEV_SEL2_REPLY
        // with SEL_RD marks fd 2 readable and completes the select (the
        // host reply seam is a no-op; we assert the entry is freed).
        unsafe {
            let se = se_slot(0);
            se.requestor = core::ptr::null_mut(); // ensure clean
            // Fake a caller fproc so the slot is "in use".
            let mut fp = Fproc {
                fp_endpoint: 42,
                ..Default::default()
            };
            se.requestor = &mut fp as *mut Fproc;
            se.req_endpt = 42;
            se.nfds = 3;
            se.block = true;
            se.nreadyfds = 0;
            se.char_minor = [u32::MAX; FD_SETSIZE];
            se.char_minor[2] = 3; // fd 2 watches minor 3
            se.vir_readfds = 0; // no user copy on host
            se.vir_writefds = 0;
            se.vir_errorfds = 0;
            se.ready_readfds = 0;
            se.ready_writefds = 0;
            se.ready_errorfds = 0;

            let r = select_driver_reply(3, SEL_RD as i32);
            assert_eq!(r, OK);
            assert!(fd_isset(2, &se.ready_readfds));
            assert!(se.requestor.is_null(), "entry freed after reply");
        }
    }
}
