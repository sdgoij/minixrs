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
//! Blocking is not ported: C's `SUSPEND`/`revive` machinery is absent, so an
//! empty pipe with writers returns `EAGAIN` and a full pipe returns `EAGAIN`
//! instead of suspending the caller.

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
/// or a negative errno (`EAGAIN` when the operation would block — this port
/// has no suspension — or `EPIPE` when writing with no readers).
///
/// `notouch` matches C's select path (check readiness only).
// Reference: pipe.c pipe_check()
pub fn pipe_check(filp: &Filp, rw_flag: i32, _oflags: i32, bytes: i32, _notouch: bool) -> i32 {
    unsafe {
        let vp = filp.filp_vno;
        if vp.is_null() {
            return EBADF;
        }
        if rw_flag == READING {
            if (*vp).v_size == 0 {
                // Empty pipe: EOF once the last writer closes; EAGAIN while
                // a writer is still open (C would SUSPEND here).
                if filedes::find_filp_vp(vp, crate::vfs::protect::W_BIT).is_null() {
                    0
                } else {
                    EAGAIN
                }
            } else {
                bytes
            }
        } else {
            // Writing: EPIPE without a reader; otherwise clamp to space.
            if filedes::find_filp_vp(vp, crate::vfs::protect::R_BIT).is_null() {
                return EPIPE;
            }
            let space = PIPE_BUF_SIZE as i64 - (*vp).v_size;
            if space <= 0 {
                return EAGAIN;
            }
            (bytes as i64).min(space) as i32
        }
    }
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
        let (r2, new_pos) = if rw_flag == READING {
            crate::vfs::request::req_read(
                (*vp).v_fs_e,
                (*vp).v_inode_nr,
                buf as *mut u8,
                0,
                size as u32,
                user_e,
                0,
            )
        } else {
            crate::vfs::request::req_write(
                (*vp).v_fs_e,
                (*vp).v_inode_nr,
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
        r2
    }
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

    #[test]
    fn test_pipe_check_read_empty_with_writer_returns_eagain() {
        init();
        unsafe {
            let vp = make_pipe_vnode(0);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            assert_eq!(pipe_check(&*filp_arr.add(0), READING, 0, 16, false), EAGAIN);
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

    #[test]
    fn test_pipe_check_write_full_returns_eagain() {
        init();
        unsafe {
            let vp = make_pipe_vnode(crate::vfs::consts::PIPE_BUF_SIZE as i64);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            assert_eq!(pipe_check(&*filp_arr.add(1), WRITING, 0, 16, false), EAGAIN);
        }
    }

    #[test]
    fn test_pipe_check_write_clamps_to_space() {
        init();
        unsafe {
            // 100 bytes buffered; space = PIPE_BUF - 100.
            let vp = make_pipe_vnode(100);
            set_pipe_filp(0, vp, R_BIT);
            set_pipe_filp(1, vp, W_BIT);
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            let space = crate::vfs::consts::PIPE_BUF_SIZE as i64 - 100;
            assert_eq!(
                pipe_check(&*filp_arr.add(1), WRITING, 0, (space + 50) as i32, false),
                space as i32
            );
        }
    }
}
