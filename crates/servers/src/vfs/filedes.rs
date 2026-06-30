//! File descriptor manipulation — adapted from `minix/servers/vfs/filedes.c`
//!
//! Provides functions for looking up, allocating, and closing file
//! descriptors and their backing filp structures.

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::*;

// Debug: lock checking (stubs)

/// Check whether any filp locks are held by any thread.
///
/// # Safety
///
/// Requires exclusive access to the global filp table.
pub unsafe fn check_filp_locks() {}

/// Check whether the current thread still holds filp locks.
///
/// # Safety
///
/// Requires exclusive access to the global filp table.
pub unsafe fn check_filp_locks_by_me() {}

// Initialization

/// Initialize all filp structures.
///
/// # Safety
///
/// Must be called once during VFS initialization.
pub unsafe fn init_filps() {
    let glob = vfs_global();
    let filp_array = unsafe { core::ptr::addr_of_mut!((*glob).filp) as *mut Filp };
    for i in 0..NR_FILPS {
        let f = unsafe { &mut *filp_array.add(i) };
        *f = Filp::default();
    }
}

// get_fd

/// Look for a free file descriptor and a free filp slot.
///
/// On success, writes the fd into `*k` and returns `OK`.
///
/// # Safety
///
/// Requires exclusive access to the global filp table.
pub unsafe fn get_fd(rfp: &mut Fproc, start: i32, k: &mut i32) -> i32 {
    let mut i = start;
    while (i as usize) < OPEN_MAX {
        if rfp.fp_filp[i as usize] < 0 {
            *k = i;
            break;
        }
        i += 1;
    }
    if (i as usize) >= OPEN_MAX {
        return EMFILE;
    }

    let glob = vfs_global();
    let filp_array = unsafe { core::ptr::addr_of_mut!((*glob).filp) as *mut Filp };
    for j in 0..NR_FILPS {
        let f = unsafe { &mut *filp_array.add(j) };
        if f.filp_count == 0 {
            f.filp_mode = 0;
            f.filp_pos = 0;
            f.filp_selectors = 0;
            f.filp_select_ops = 0;
            f.filp_select_flags = 0;
            f.filp_pipe_select_ops = 0;
            f.filp_pipe_select_ep = [-1; 2];
            f.filp_pipe_ino = 0;
            f.filp_flags = 0;
            f.filp_ino = 0;
            f.filp_vno = core::ptr::null_mut();
            f.filp_state = 0;
            f.filp_select_ep = -1;
            rfp.fp_filp[i as usize] = j as i32;
            return OK;
        }
    }

    ENFILE
}

// get_filp

/// Look up the filp entry for a given file descriptor in the current
/// process. Returns the filp index (>= 0) on success, or a negative errno.
///
/// # Safety
///
/// Requires exclusive access to the calling process's fproc and filp table.
pub unsafe fn get_filp(fd: i32, fp: &Fproc) -> i32 {
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let idx = fp.fp_filp[fd as usize];
    if idx < 0 {
        return EBADF;
    }
    if (idx as usize) >= NR_FILPS {
        return EBADF;
    }
    idx
}

/// Find a filp slot that refers to the given vnode inode number
/// with matching mode bits. Returns a raw pointer to the filp, or
/// `NULL` if none is found.
///
/// # Safety
///
/// Requires exclusive access to the global filp table.
pub unsafe fn find_filp(ino: u32, mode: u32) -> *mut Filp {
    let glob = vfs_global();
    let filp_array = unsafe { core::ptr::addr_of_mut!((*glob).filp) as *mut Filp };
    for i in 0..NR_FILPS {
        let f = unsafe { &mut *filp_array.add(i) };
        if f.filp_count > 0 && f.filp_ino == ino && (f.filp_mode & mode) != 0 {
            return f;
        }
    }
    core::ptr::null_mut()
}

// alloc_filp

/// Allocate a free filp slot. Returns the index into the filp table,
/// or `ENFILE` if the table is full.
///
/// # Safety
///
/// Requires exclusive access to the global filp table.
pub unsafe fn alloc_filp() -> i32 {
    let glob = vfs_global();
    let filp_array = unsafe { core::ptr::addr_of_mut!((*glob).filp) as *mut Filp };
    for i in 0..NR_FILPS {
        let f = unsafe { &mut *filp_array.add(i) };
        if f.filp_count == 0 {
            *f = Filp::default();
            f.filp_count = 1;
            return i as i32;
        }
    }
    ENFILE
}

// close_filp

/// Close a filp by index. Decrements the reference count and frees the
/// slot if it reaches zero.
///
/// # Safety
///
/// Requires exclusive access to the global filp table and vnode table.
pub unsafe fn close_filp(fp: &mut Fproc, filp_idx: i32) -> i32 {
    if filp_idx < 0 || (filp_idx as usize) >= NR_FILPS {
        return EBADF;
    }

    let glob = vfs_global();
    let filp_array = unsafe { core::ptr::addr_of_mut!((*glob).filp) as *mut Filp };
    let f = unsafe { &mut *filp_array.add(filp_idx as usize) };

    if f.filp_count <= 0 {
        return OK;
    }

    f.filp_count -= 1;

    for fd in 0..OPEN_MAX {
        if fp.fp_filp[fd] == filp_idx {
            fp.fp_filp[fd] = -1;
        }
    }

    if f.filp_count == 0 {
        *f = Filp::default();
    }

    OK
}
