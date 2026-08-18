//! File descriptor manipulation — adapted from `minix/servers/vfs/filedes.c`
//!
//! Provides functions for looking up, allocating, and closing file
//! descriptors and their backing filp structures.

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::mount;
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

/// Find a filp slot that refers to the given vnode with matching mode bits.
///
/// Used to determine whether anyone still holds a given end of a pipe
/// (C `find_filp(vp, bits)`). Returns a raw pointer to the filp, or `NULL`
/// if none is found.
///
/// # Safety
///
/// Requires exclusive access to the global filp table.
pub unsafe fn find_filp_vp(vp: *mut Vnode, mode: u32) -> *mut Filp {
    let glob = vfs_global();
    let filp_array = unsafe { core::ptr::addr_of_mut!((*glob).filp) as *mut Filp };
    for i in 0..NR_FILPS {
        let f = unsafe { &mut *filp_array.add(i) };
        if f.filp_count > 0 && f.filp_vno == vp && (f.filp_mode & mode) != 0 {
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
/// The caller is responsible for clearing the specific fd table entry;
/// this function must NOT touch the fproc's other fd slots, because a
/// filp can be referenced by several fds in the same process (via dup2)
/// and clearing all of them would destroy the aliases.
///
/// # Safety
///
/// Requires exclusive access to the global filp table and vnode table.
pub unsafe fn close_filp(filp_idx: i32) -> i32 {
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

    if f.filp_count == 0 {
        // Last reference: tell the driver about character-device closes.
        // Use the filp's device number so socket clone minors are released.
        let vp = f.filp_vno;
        if f.filp_dev != 0 && !vp.is_null() && ((*vp).v_mode & S_IFMT) == S_IFCHR {
            unsafe {
                crate::vfs::device::cdev_close(f.filp_dev);
            }
        }
        // Release the vnode reference the filp held (C close_filp: the
        // open-time vnode reference is consumed here, sending req_putnode
        // so the FS server can free the inode).
        if !vp.is_null() {
            mount::put_vnode(vp);
        }
        *f = Filp::default();
    }

    OK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closing one fd must not clear the other fds that alias the same filp
    /// (dup2). The caller of close_filp is responsible for clearing the one
    /// fd slot it closed; close_filp only decrements the refcount.
    #[test]
    fn test_close_filp_preserves_dup_aliases() {
        unsafe {
            let glob = vfs_global();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            let idx = alloc_filp();
            assert!(idx >= 0, "alloc_filp should succeed");
            (*filp_arr.add(idx as usize)).filp_count = 2; // two fds (dup2 alias)

            let mut fp = Fproc::default();
            fp.fp_filp[0] = idx;
            fp.fp_filp[1] = idx;

            assert_eq!(close_filp(idx), OK);
            assert_eq!(fp.fp_filp[0], idx, "fd 0 alias must survive");
            assert_eq!(fp.fp_filp[1], idx, "fd 1 alias must survive");
            assert_eq!((*filp_arr.add(idx as usize)).filp_count, 1);

            assert_eq!(close_filp(idx), OK);
            assert_eq!((*filp_arr.add(idx as usize)).filp_count, 0);
        }
    }
}
