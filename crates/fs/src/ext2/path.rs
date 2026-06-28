//! Path lookup and directory search — adapted from `minix/fs/ext2/path.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::protect::*;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_lookup — VFS lookup handler.
pub unsafe fn fs_lookup() -> i32 {
    let ext2 = glo::ext2_ptr();

    // TODO: parse message, extract path, parent inode, flags
    // For now, return ENOENT
    (*ext2).err_code = ENOENT;
    ENOENT
}

/// Advance to the next path component.
pub unsafe fn advance(dirp: *mut Inode, string: &[u8], chk_perm: i32) -> *mut Inode {
    if dirp.is_null() {
        return core::ptr::null_mut();
    }

    if ((*dirp).i_mode & I_TYPE) != I_DIRECTORY {
        return core::ptr::null_mut();
    }

    let mut numb = 0u32;
    let r = search_dir(dirp, string, &mut numb, LOOK_UP, chk_perm, 0);
    if r != OK {
        return core::ptr::null_mut();
    }

    if numb == 0 {
        return core::ptr::null_mut();
    }

    let rip = get_inode((*dirp).i_dev, numb);
    rip
}

/// Search a directory for a string, or enter/delete an entry.
pub unsafe fn search_dir(
    ldir_ptr: *mut Inode,
    string: &[u8],
    numb: &mut u32,
    flag: i32,
    check_permissions: i32,
    _ftype: i32,
) -> i32 {
    if ldir_ptr.is_null() {
        return ENOENT;
    }

    // Check if it's a directory
    if ((*ldir_ptr).i_mode & I_TYPE) != I_DIRECTORY {
        return ENOTDIR;
    }

    // Check permissions
    if check_permissions != IGN_PERM {
        let r = forbidden(ldir_ptr, R_BIT | W_BIT | X_BIT);
        if r != OK {
            return r;
        }
    }

    // For LOOK_UP: find the entry
    if flag == LOOK_UP {
        // TODO: iterate through directory blocks, find matching entry
        // For now, return ENOENT
        return ENOENT;
    }

    // For ENTER: create a new entry
    if flag == ENTER {
        // TODO: find free slot and create entry
        return ENOSPC;
    }

    // For DELETE: remove an entry
    if flag == DELETE {
        // TODO: find and delete entry
        return ENOENT;
    }

    // For IS_EMPTY: check if directory is empty
    if flag == IS_EMPTY {
        // TODO: check if directory has entries other than . and ..
        return ENOTEMPTY;
    }

    EINVAL
}
