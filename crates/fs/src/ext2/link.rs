//! Link, unlink, rename, rdlink — adapted from `minix/fs/ext2/link.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::path::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_link — create a hard link.
pub unsafe fn fs_link() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message, create hard link
    let _ = ext2;
    EINVAL
}

/// fs_unlink — remove a link.
pub unsafe fn fs_unlink() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message, unlink file
    let _ = ext2;
    EINVAL
}

/// fs_rename — rename a file.
pub unsafe fn fs_rename() -> i32 {
    // TODO: implement rename
    EINVAL
}

/// fs_rdlink — read a symbolic link.
pub unsafe fn fs_rdlink() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: read symlink target
    let _ = ext2;
    EINVAL
}

/// fs_ftrunc — truncate a file.
pub unsafe fn fs_ftrunc() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: truncate file from message
    let _ = ext2;
    EINVAL
}
