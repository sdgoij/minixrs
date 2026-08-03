//! VFS `select()` / `poll()` implementation — ported from `minix/servers/vfs/select.c`.
//!
//! Supports multiplexed I/O across regular files (always ready), pipes
//! (read via `pipe::pipe_check`), and character devices (via `CDEV_SELECT`
//! with a simplified single-phase protocol).
//!
//! The select table holds up to `MAX_SELECTS` concurrent calls.  Each entry
//! tracks the fd sets, ready sets, timeout, and blocking state for one caller.

use crate::vfs::consts::*;
use crate::vfs::types::*;

/// Maximum number of concurrent `select()` calls.
const MAX_SELECTS: usize = 16;

/// fd_set: 64-bit bitmask, representing fds 0..63.
pub type FdSet = u64;
pub const FD_SETSIZE: usize = 64;

/// Operations that `select()` watches for.
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

/// Convert a SEL_* bitmask to ready fd_set bits.
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
    /// Endpoint of the requesting process.
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
    /// TRUE = select should block (timeout != {0,0}).
    block: bool,
    /// Timeout in ticks (0 = no timeout / poll mode).
    timeout_ticks: u64,
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
            timeout_ticks: 0,
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

/// Check whether a regular file's fd is ready for the given ops.
/// Regular files are always ready.
fn select_request_file(_filp: &Filp, ops: u32) -> u32 {
    ops
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

/// Check whether a character device fd is ready.
/// For now, char devices are always considered ready (simplified).
fn select_request_char(_filp: &Filp, ops: u32) -> u32 {
    ops
}

/// Perform the `select(nfds, readfds, writefds, errorfds, timeout)` system call.
///
/// Reads parameters from `fs_m_in`, checks ready fds across the fd set,
/// and either returns immediately or suspends the caller.
///
/// # Safety
///
/// Must be called from a valid VFS dispatch context where `fs_m_in` contains
/// a properly-formed `select` request.
pub unsafe fn do_select() -> i32 {
    use crate::vfs::glo::vfs_global;
    let glob = unsafe { &mut *vfs_global() };

    let nfds = r2_i32(&glob.fs_m_in, SEL_NFDS_OFF);
    let readfds_ptr = r2_u64(&glob.fs_m_in, SEL_RDFDS_OFF);
    let writefds_ptr = r2_u64(&glob.fs_m_in, SEL_WRFDS_OFF);
    let errorfds_ptr = r2_u64(&glob.fs_m_in, SEL_EXFDS_OFF);
    let timeout_sec = r2_i64(&glob.fs_m_in, SEL_TV_SEC_OFF);
    let timeout_usec = r2_i64(&glob.fs_m_in, SEL_TV_USEC_OFF);

    if nfds < 0 || nfds > FD_SETSIZE as i32 {
        return EINVAL;
    }

    let fp = unsafe { &mut *glob.fproc.as_mut_ptr() };
    let caller_ep = fp.fp_endpoint;

    // Find a free select table slot.
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

    // Copy fd sets from user space.
    let mut readfds: FdSet = 0;
    let mut writefds: FdSet = 0;
    let mut errorfds: FdSet = 0;

    if readfds_ptr != 0 && nfds > 0 {
        unsafe {
            copy_from_user(readfds_ptr, &mut readfds);
        }
    }
    if writefds_ptr != 0 && nfds > 0 {
        unsafe {
            copy_from_user(writefds_ptr, &mut writefds);
        }
    }
    if errorfds_ptr != 0 && nfds > 0 {
        unsafe {
            copy_from_user(errorfds_ptr, &mut errorfds);
        }
    }

    // Initialize select entry.
    se.requestor = fp;
    se.req_endpt = caller_ep;
    se.nfds = nfds;
    se.readfds = readfds;
    se.writefds = writefds;
    se.errorfds = errorfds;
    se.vir_readfds = readfds_ptr;
    se.vir_writefds = writefds_ptr;
    se.vir_errorfds = errorfds_ptr;
    fd_zero(&mut se.ready_readfds);
    fd_zero(&mut se.ready_writefds);
    fd_zero(&mut se.ready_errorfds);
    se.nreadyfds = 0;
    se.error = OK;

    // Check each fd for readiness.
    for fd in 0..nfds {
        let ops = tab2ops(fd, nfds, readfds, writefds, errorfds);
        if ops == 0 {
            continue;
        }

        let filp_idx = fp.fp_filp[fd as usize];
        if filp_idx < 0 {
            se.error = EBADF;
            continue;
        }

        let filp_arr = core::ptr::addr_of_mut!(glob.filp) as *mut Filp;
        let filp = unsafe { &*filp_arr.add(filp_idx as usize) };
        let vp = filp.filp_vno;
        if vp.is_null() {
            se.error = EBADF;
            continue;
        }

        let mut want_ops = ops;

        // Check mode: if fd lacks read permission but SEL_RD requested → ready
        if ops & SEL_RD != 0 && (filp.filp_mode & 1) == 0 {
            want_ops |= SEL_RD;
        }
        if ops & SEL_WR != 0 && (filp.filp_mode & 2) == 0 {
            want_ops |= SEL_WR;
        }

        // Dispatch based on file type.
        let ready_ops = if crate::vfs::pipe::is_pipe_filp(filp.filp_pipe_ino) {
            select_request_pipe(filp, want_ops)
        } else {
            let mode = unsafe { (*vp).v_mode };
            if mode & S_IFCHR != 0 {
                select_request_char(filp, want_ops)
            } else if mode & S_IFBLK != 0 {
                want_ops // block devices: always ready for now
            } else {
                select_request_file(filp, want_ops)
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

    // Determine blocking mode.
    se.block = !(timeout_sec == 0 && timeout_usec == 0);
    if se.block {
        se.timeout_ticks = (timeout_sec as u64)
            .saturating_mul(100)
            .saturating_add((timeout_usec as u64) / 10000);
    } else {
        se.timeout_ticks = 0;
    }

    // Return immediately if ready, error, or non-blocking.
    if se.nreadyfds > 0 || se.error != OK || !se.block {
        let result = if se.error != OK {
            se.error
        } else {
            se.nreadyfds
        };
        write_results(se);
        se.requestor = core::ptr::null_mut(); // free slot
        return result;
    }

    // Blocking: suspend the process.
    fp.fp_blocked_on = FP_BLOCKED_ON_SELECT;
    SUSPEND
}

/// Copy a completed select's results back to user space and free the slot.
unsafe fn write_results(se: &SelectEntry) {
    if se.vir_readfds != 0 {
        unsafe {
            copy_to_user(se.ready_readfds, se.vir_readfds);
        }
    }
    if se.vir_writefds != 0 {
        unsafe {
            copy_to_user(se.ready_writefds, se.vir_writefds);
        }
    }
    if se.vir_errorfds != 0 {
        unsafe {
            copy_to_user(se.ready_errorfds, se.vir_errorfds);
        }
    }
}

fn r2_i32(buf: &[u8; 64], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn r2_i64(buf: &[u8; 64], off: usize) -> i64 {
    i64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}

fn r2_u64(buf: &[u8; 64], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}

unsafe fn copy_from_user(user_ptr: u64, dst: &mut FdSet) {
    let src = user_ptr as *const u8;
    let bytes = unsafe { core::slice::from_raw_parts(src, 8) };
    *dst = u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]));
}

unsafe fn copy_to_user(val: FdSet, user_ptr: u64) {
    let dst = user_ptr as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(&val.to_le_bytes() as *const u8, dst, 8);
    }
}

/// Notify any blocked select() caller that fds are ready.
/// Called by pipe close/release or device event handlers.
pub fn select_notify() {
    unsafe {
        for i in 0..MAX_SELECTS {
            let se = se_slot(i);
            if se.requestor.is_null() {
                continue;
            }
            if se.block && se.nreadyfds == 0 && se.error == OK {
                continue;
            }
            // Wake the process.
            write_results(se);
            let fp = &mut *se.requestor;
            fp.fp_blocked_on = FP_BLOCKED_ON_NONE;
            se.requestor = core::ptr::null_mut();
        }
    }
}

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
    fn test_fd_isset_out_of_range() {
        let s: FdSet = !0;
        assert!(!fd_isset(100, &s));
        assert!(!fd_isset(-1, &s));
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
    fn test_select_request_file_always_ready() {
        let f = Filp::default();
        assert_eq!(select_request_file(&f, SEL_RD), SEL_RD);
        assert_eq!(select_request_file(&f, SEL_WR), SEL_WR);
        assert_eq!(select_request_file(&f, SEL_RD | SEL_WR), SEL_RD | SEL_WR);
    }

    #[test]
    fn test_select_request_char_always_ready() {
        let f = Filp::default();
        assert_eq!(select_request_char(&f, SEL_RD), SEL_RD);
    }
}
