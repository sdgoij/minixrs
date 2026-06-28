//! stat/statvfs — adapted from `minix/fs/ext2/stadir.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_stat — stat a file.
pub unsafe fn fs_stat() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message, get inode, fill stat struct
    let _ = ext2;
    EINVAL
}

/// fs_statvfs — stat the file system.
pub unsafe fn fs_statvfs() -> i32 {
    let ext2 = glo::ext2_ptr();
    let sp = get_super((*ext2).fs_dev);
    if sp.is_null() {
        return EINVAL;
    }

    // TODO: fill statvfs struct and copy to user
    let _ = sp;
    EINVAL
}

/// fs_blockstats — get block statistics.
pub unsafe fn fs_blockstats(blocks: &mut u64, free: &mut u64, used: &mut u64) {
    let ext2 = glo::ext2_ptr();
    let sp = get_super((*ext2).fs_dev);
    if sp.is_null() {
        return;
    }
    *blocks = (*sp).s_blocks_count as u64;
    *free = (*sp).s_free_blocks_count as u64;
    *used = *blocks - *free;
}
