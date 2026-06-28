//! utime — adapted from `minix/fs/ext2/time.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::types::*;

/// fs_utime — update access/modification times.
pub unsafe fn fs_utime() -> i32 {
    let ext2 = glo::ext2_ptr();

    // TODO: parse message, get inode, set times
    let _ = ext2;
    EINVAL
}
