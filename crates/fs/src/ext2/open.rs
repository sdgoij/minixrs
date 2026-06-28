//! File/dir/symlink creation — adapted from `minix/fs/ext2/open.c`

use crate::ext2::balloc::*;
use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::inode::*;
use crate::ext2::path::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_create — create a regular file.
pub unsafe fn fs_create() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message
    let _ = ext2;
    EINVAL
}

/// fs_mkdir — create a directory.
pub unsafe fn fs_mkdir() -> i32 {
    // TODO: implement
    EINVAL
}

/// fs_mknod — create a special file (device node).
pub unsafe fn fs_mknod() -> i32 {
    // TODO: implement
    EINVAL
}

/// fs_slink — create a symbolic link.
pub unsafe fn fs_slink() -> i32 {
    // TODO: implement
    EINVAL
}

/// fs_inhibread — inhibit read ahead.
pub unsafe fn fs_inhibread() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: find inode, set i_seek = ISEEK
    let _ = ext2;
    OK
}
