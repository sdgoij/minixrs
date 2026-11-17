//! VFS pipe operations — adapted from `minix/servers/vfs/pipe.c`
//!
//! Pipe inodes and pipe data live in the PFS server, matching the original
//! MINIX design: `do_pipe2` creates the pipe inode on PFS via `req_newnode`
//! and maps the vnode to PFS (`v_fs_e = PFS_PROC_NR`); read/write route
//! through `req_read`/`req_write` into PFS's block cache. VFS caches the
//! current pipe size in `v_size` (refreshed from each reply's seek_pos) and
//! derives reader/writer presence from its own filp table (`find_filp_vp`),
//! exactly like C.
//!
//! Blocking is ported the way C does it, without C's worker framework: a read of an
//! empty pipe with a writer open (or a write that cannot fit) parks the caller's request
//! in [`Parked`] and returns `SUSPEND`, so the main loop does not reply; `release` re-runs
//! the request when the other end moves. A descriptor opened `O_NONBLOCK` still gets
//! `EAGAIN` at once, as POSIX requires.

use crate::vfs::consts::*;
use crate::vfs::filedes;
use crate::vfs::types::*;

/// Read/write direction flags (C `read.h`).
pub const READING: i32 = 0;
pub const WRITING: i32 = 1;

/// Decide whether a pipe read or write can proceed.
///
/// Returns the number of bytes that may be transferred (clamped to the
/// available space), 0 for EOF (read on an empty pipe with no writers),
/// `SUSPEND` when the caller must block, or a negative errno (`EAGAIN` for the
/// same conditions on a descriptor opened `O_NONBLOCK`, `EPIPE` when writing
/// with no readers).
///
/// `notouch` marks a readiness check. It suppresses nothing here: the side
/// effects C performs inside this function (waking the opposite direction)
/// live in [`rw_pipe`], which the readiness path never calls.
// Reference: pipe.c pipe_check()
pub fn pipe_check(filp: &Filp, rw_flag: i32, oflags: i32, bytes: i32, _notouch: bool) -> i32 {
    let nonblock = oflags & O_NONBLOCK != 0;
    unsafe {
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        if rw_flag == READING {
            if (*vp).v_size == 0 {
                // Empty pipe: EOF once the last writer closes, otherwise the read waits
                // for data (C suspends here; a non-blocking descriptor gets EAGAIN).
                if filedes::find_filp_vp(vp, crate::vfs::protect::W_BIT).is_null() {
                    return 0;
                }
                return if nonblock { EAGAIN } else { SUSPEND };
            }
            return bytes;
        }
        // Writing: EPIPE without a reader; otherwise clamp to space.
        if filedes::find_filp_vp(vp, crate::vfs::protect::R_BIT).is_null() {
            return EPIPE;
        }
        write_allowance((*vp).v_size, bytes, nonblock)
    }
}

/// How much of a `bytes` write may be transferred with `used` bytes in the pipe.
///
/// `PIPE_BUF` is the atomicity bound as well as the capacity: a write that fits is
/// transferred whole, a smaller write that does not fit waits for room rather than being
/// cut (POSIX requires a write of at most `PIPE_BUF` to be all or nothing), and only a
/// write larger than `PIPE_BUF` is split at the space available, which is how a producer
/// larger than the buffer makes progress at all.
fn write_allowance(used: i64, bytes: i32, nonblock: bool) -> i32 {
    let capacity = PIPE_BUF_SIZE as i64;
    let space = capacity - used;
    let len = bytes as i64;
    if space <= 0 {
        return if nonblock { EAGAIN } else { SUSPEND };
    }
    if used + len <= capacity {
        return bytes;
    }
    // It does not fit. An atomic-sized write waits for room; only one larger than the
    // buffer is split, which is how a producer bigger than the pipe makes progress.
    if len > capacity {
        return space as i32;
    }
    if nonblock { EAGAIN } else { SUSPEND }
}

/// Read from or write to a pipe end (C `rw_pipe`).
///
/// Checks the pipe state, then routes the data transfer to PFS through
/// `req_read`/`req_write` and refreshes the cached `v_size` from the reply.
/// Returns the number of bytes transferred, or a negative errno.
// Reference: read.c rw_pipe()
pub fn rw_pipe(filp: &Filp, rw_flag: i32, user_e: i32, buf: u64, req_size: usize) -> i32 {
    unsafe {
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        let oflags = filp.filp_flags as i32;
        let r = pipe_check(filp, rw_flag, oflags, req_size as i32, false);
        if r <= 0 {
            return r;
        }
        let mut size = r as usize;
        if rw_flag == READING && size > (*vp).v_size as usize {
            size = (*vp).v_size as usize;
        }
        // Named pipes (opened through a directory entry) keep the entry on
        // the original FS but route data through the mapped FS (C read.c
        // rw_pipe uses v_mapfs_e/v_mapinode_nr); anonymous pipes set
        // v_fs_e = PFS directly and leave the mapping fields unset.
        let (fs_e, inode_nr) = if (*vp).v_mapfs_e != crate::vfs::consts::NONE_ENDPOINT {
            ((*vp).v_mapfs_e, (*vp).v_mapinode_nr)
        } else {
            ((*vp).v_fs_e, (*vp).v_inode_nr)
        };
        let (r2, new_pos) = if rw_flag == READING {
            crate::vfs::request::req_read(fs_e, inode_nr, buf as *mut u8, 0, size as u32, user_e, 0)
        } else {
            crate::vfs::request::req_write(
                fs_e,
                inode_nr,
                buf as *const u8,
                0,
                size as u32,
                user_e,
                0,
            )
        };
        if r2 < 0 {
            return r2;
        }
        // C caches the pipe size from the reply's seek_pos.
        (*vp).v_size = new_pos;
        // The transfer changed what the other end can do: a read freed space for a writer,
        // a write gave a reader something. Nothing else reports that for a pipe (a
        // character device notifies; a pipe is VFS's own), so the opposite direction's
        // suspended requests are resumed and a suspended wait watching a pipe is
        // completed here or waits for its deadline.
        release(vp, if rw_flag == READING { WRITING } else { READING });
        crate::vfs::select::wake_pipes();
        r2
    }
}

/// Suspend the current caller's pipe request (C `pipe_suspend`).
///
/// Returns `SUSPEND`, which the caller returns upwards so the main loop does not reply;
/// [`release`] resumes the request and sends the answer. A second request from a
/// suspended process cannot arrive, because it is blocked in its `sendrec`.
///
/// # Safety
///
/// VFS dispatch only, for the process named by `fp`.
pub unsafe fn suspend(fp: *mut Fproc, filp_idx: i32, buf: u64, count: usize, op: i32) -> i32 {
    crate::vfs::blocked::park(
        fp,
        crate::vfs::blocked::Parked {
            kind: crate::vfs::blocked::PIPE,
            id: filp_idx,
            op,
            buf,
            count,
        },
    )
}

/// Resume `slot`'s suspended pipe request and answer it, or put it back if it still
/// cannot proceed. Returns true when the request was answered.
///
/// # Safety
///
/// VFS dispatch only.
unsafe fn pending(slot: usize) -> bool {
    let Some(p) = (unsafe { crate::vfs::blocked::take(slot, crate::vfs::blocked::PIPE) }) else {
        return false;
    };
    let filp_arr =
        core::ptr::addr_of_mut!((*crate::vfs::glo::vfs_global()).filp) as *mut Filp;
    let ep = (*unsafe { crate::vfs::blocked::fproc(slot) }).fp_endpoint;
    let r = rw_pipe(&*filp_arr.add(p.id as usize), p.op, ep, p.buf, p.count);
    if r == SUSPEND {
        // Still would block: park exactly as before, so the next release finds it.
        unsafe { crate::vfs::blocked::restore(slot, p) };
        return false;
    }
    unsafe { crate::vfs::misc::reply_endpoint(ep, r) };
    true
}

/// Wake every process suspended on `vp` in direction `op`, now that it may be able to
/// proceed (C `release`). Returns how many requests were answered.
///
/// `op` is the direction of the *suspended* request, which is the opposite of the one
/// that just moved.
///
/// # Safety
///
/// VFS dispatch only. `vp` may be compared but not dereferenced.
pub unsafe fn release(vp: *mut Vnode, op: i32) -> usize {
    let mut answered = 0;
    for slot in 0..crate::vfs::blocked::SLOTS {
        let Some(p) = (unsafe { crate::vfs::blocked::peek(slot) }) else {
            continue;
        };
        if p.kind != crate::vfs::blocked::PIPE || p.op != op || p.id < 0 {
            continue;
        }
        let glob = crate::vfs::glo::vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        if (*filp_arr.add(p.id as usize)).filp_vno != vp {
            continue;
        }
        if unsafe { pending(slot) } {
            answered += 1;
        }
    }
    answered
}

/// Reader/writer pairing for a FIFO open (C `pipe_open` in open.c).
///
/// O_RDWR on a FIFO is rejected (ENXIO); a write end needs a reader already
/// open, a read end is always openable. The suspension branch of C is
/// replaced by EAGAIN, matching the port's no-blocking pipe policy: a
/// blocking open that C would suspend on fails immediately instead.
///
/// # Safety
///
/// `vp` must point to a valid, initialized Vnode.
// Reference: open.c pipe_open()
pub unsafe fn pipe_open(vp: *mut Vnode, bits: u32, oflags: u32) -> i32 {
    use crate::vfs::protect::{R_BIT, W_BIT};
    if (bits & (R_BIT | W_BIT)) == (R_BIT | W_BIT) {
        return ENXIO;
    }
    let other = if bits & W_BIT != 0 { R_BIT } else { W_BIT };
    unsafe {
        if filedes::find_filp_vp(vp, other).is_null() {
            if (oflags as i32) & O_NONBLOCK != 0 {
                // C: a non-blocking write end without a reader fails; a
                // non-blocking read end without a writer opens fine.
                if bits & W_BIT != 0 {
                    return ENXIO;
                }
            } else {
                return EAGAIN;
            }
        }
    }
    OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::glo::vfs_global;
    use crate::vfs::protect::{R_BIT, W_BIT};

    fn init() {
        unsafe {
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            for i in 0..crate::vfs::consts::NR_FILPS {
                (*filp_arr.add(i)) = Filp::default();
            }
        }
    }

    /// Wire filp slot `i` to `vp` as a pipe end with the given mode bits.
    unsafe fn set_pipe_filp(i: usize, vp: *mut Vnode, mode: u32) {
        let glob = vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        let f = &mut *filp_arr.add(i);
        f.filp_count = 1;
        f.filp_mode = mode;
        f.filp_vno = vp;
    }

    unsafe fn make_pipe_vnode(v_size: i64) -> *mut Vnode {
        let glob = vfs_global();
        let vnode_arr = core::ptr::addr_of_mut!((*glob).vnode) as *mut Vnode;
        let vp = &mut *vnode_arr.add(0);
        *vp = Vnode::default();
        vp.v_mode = crate::vfs::consts::S_IFIFO;
        vp.v_size = v_size;
        vp
    }

    /// A read of an empty pipe with a writer open waits for data (C suspends); a
    /// non-blocking descriptor gets the old answer at once.
    #[test]
    fn test_pipe_check_read_empty_with_writer_blocks() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            assert_eq!(pipe_check(&*filp_arr.add(0), READING, 0, 16, false), SUSPEND);
            assert_eq!(
                pipe_check(&*filp_arr.add(0), READING, O_NONBLOCK, 16, false),
                EAGAIN
            );
        }
    }

    #[test]
    fn test_pipe_check_read_empty_no_writer_returns_eof() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            set_pipe_filp(0, vp, R_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            assert_eq!(pipe_check(&*filp_arr.add(0), READING, 0, 16, false), 0);
        }
    }

    #[test]
    fn test_pipe_check_read_with_data_returns_bytes() {
        init();
        unsafe {
            let vp = make_pipe_vnode(100);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            assert_eq!(pipe_check(&*filp_arr.add(0), READING, 0, 16, false), 16);
        }
    }

    #[test]
    fn test_pipe_check_write_no_reader_returns_epipe() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            assert_eq!(pipe_check(&*filp_arr.add(1), WRITING, 0, 16, false), EPIPE);
        }
    }

    /// A full pipe blocks a writer; a non-blocking one is told to come back.
    #[test]
    fn test_pipe_check_write_full_blocks() {
        init();
        unsafe {
            let vp = make_pipe_vnode(crate::vfs::consts::PIPE_BUF_SIZE as i64);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            assert_eq!(pipe_check(&*filp_arr.add(1), WRITING, 0, 16, false), SUSPEND);
            assert_eq!(
                pipe_check(&*filp_arr.add(1), WRITING, O_NONBLOCK, 16, false),
                EAGAIN
            );
        }
    }

    /// A write of at most `PIPE_BUF` is all or nothing: it waits for room rather than
    /// being cut, which is what POSIX requires of a write that small.
    #[test]
    fn test_pipe_check_write_small_that_does_not_fit_blocks() {
        init();
        unsafe {
            let buf = crate::vfs::consts::PIPE_BUF_SIZE;
            let vp = make_pipe_vnode(100);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            let small = buf - 50;
            assert_eq!(
                pipe_check(&*filp_arr.add(1), WRITING, 0, small, false),
                SUSPEND
            );
            assert_eq!(
                pipe_check(&*filp_arr.add(1), WRITING, O_NONBLOCK, small, false),
                EAGAIN
            );
        }
    }

    /// A write larger than the buffer is split at the space available, in both modes:
    /// that is the only way a producer bigger than the pipe makes progress.
    #[test]
    fn test_pipe_check_write_large_is_split_at_the_space() {
        init();
        unsafe {
            let space = crate::vfs::consts::PIPE_BUF_SIZE as i64 - 100;
            let vp = make_pipe_vnode(100);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            let big = crate::vfs::consts::PIPE_BUF_SIZE + 100;
            assert_eq!(
                pipe_check(&*filp_arr.add(1), WRITING, 0, big, false),
                space as i32
            );
            assert_eq!(
                pipe_check(&*filp_arr.add(1), WRITING, O_NONBLOCK, big, false),
                space as i32
            );
        }
    }

    /// A suspended request is forgotten when its process goes away, so the slot cannot be
    /// resumed and answered after it is reused.
    #[test]
    fn test_pipe_suspend_then_cancel_clears_the_slot() {
        init();
        unsafe {
            let glob = vfs_global();
            let fp = (core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc).add(3);
            (*fp).fp_endpoint = 3;
            (*fp).fp_pid = 42;

            assert_eq!(suspend(fp, 1, 0x1000, 16, READING), SUSPEND);
            assert_eq!((*fp).fp_blocked_on, FP_BLOCKED_ON_PIPE);
            let p = crate::vfs::blocked::peek(3).expect("parked");
            assert_eq!((p.id, p.count, p.op), (1, 16, READING));

            crate::vfs::blocked::clear(&mut *fp);
            assert_eq!((*fp).fp_blocked_on, FP_BLOCKED_ON_NONE);
            assert!(
                crate::vfs::blocked::peek(3).is_none(),
                "the entry must not outlive the process"
            );
        }
    }

    /// The whole shape a shell's pipeline relies on: a reader suspends on an empty pipe, and
    /// the writer closing its end resumes it with EOF. This is the guest deadlock in miniature
    /// (`just test-pipeblock`): without the release, neither end can ever finish.
    #[test]
    fn test_close_of_the_write_end_resumes_a_suspended_reader() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;

            let reader = fproc_arr.add(3);
            (*reader).fp_endpoint = 3;
            (*reader).fp_pid = 30;
            (*reader).fp_filp[0] = 0;
            let writer = fproc_arr.add(4);
            (*writer).fp_endpoint = 4;
            (*writer).fp_pid = 40;
            (*writer).fp_filp[1] = 1;

            assert_eq!(pipe_check(&*filp_arr.add(0), READING, 0, 16, false), SUSPEND);
            assert_eq!(suspend(reader, 0, 0x1000, 16, READING), SUSPEND);

            let _ = crate::vfs::stadir::close_fd(&mut *writer, 1);

            assert_eq!(
                (*reader).fp_blocked_on,
                FP_BLOCKED_ON_NONE,
                "the last writer closing must resume the reader"
            );
            assert!(
                crate::vfs::blocked::peek(3).is_none(),
                "and its parked request is gone"
            );
            assert_eq!((*reader).fp_filp[0], 0, "the reader's own fd is untouched");
        }
    }

    #[test]
    fn test_pipe_open_rdwr_rejected() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            assert_eq!(pipe_open(vp, R_BIT | W_BIT, 0), ENXIO);
        }
    }

    #[test]
    fn test_pipe_open_write_no_reader_nonblock_enxio() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            assert_eq!(pipe_open(vp, W_BIT, O_NONBLOCK as u32), ENXIO);
        }
    }

    #[test]
    fn test_pipe_open_write_no_reader_blocking_eagain() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            assert_eq!(pipe_open(vp, W_BIT, 0), EAGAIN);
        }
    }

    #[test]
    fn test_pipe_open_read_no_writer_nonblock_ok() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            assert_eq!(pipe_open(vp, R_BIT, O_NONBLOCK as u32), OK);
        }
    }

    #[test]
    fn test_pipe_open_write_with_reader_ok() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            set_pipe_filp(0, vp, R_BIT);
            assert_eq!(pipe_open(vp, W_BIT, 0), OK);
        }
    }
}
