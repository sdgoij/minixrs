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

    // FIXME: parse inode from message
    let ino = (*ext2).fs_m_in_type as u32;
    let rip = get_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    // Update times if needed
    if (*rip).i_update != 0 {
        update_times(rip);
    }

    let s = {
        let mo = (*rip).i_mode & I_TYPE;
        mo == I_CHAR_SPECIAL || mo == I_BLOCK_SPECIAL
    };

    // FIXME: copy stat struct to user via grant
    // Fields that would be filled:
    let _dev = (*rip).i_dev;
    let _ino = (*rip).i_num;
    let _mode = (*rip).i_mode;
    let _nlink = (*rip).i_links_count;
    let _uid = (*rip).i_uid;
    let _gid = (*rip).i_gid;
    let _rdev = if s { (*rip).i_block[0] } else { NO_DEV };
    let _size = (*rip).i_size;
    let _atime = (*rip).i_atime;
    let _mtime = (*rip).i_mtime;
    let _ctime = (*rip).i_ctime;
    let _blksize = (*(*rip).i_sp.as_ref().unwrap()).s_block_size;
    let _blocks = (*rip).i_blocks;

    put_inode(rip);
    OK
}

/// fs_statvfs — stat the file system.
pub unsafe fn fs_statvfs() -> i32 {
    let ext2 = glo::ext2_ptr();
    let sp = get_super((*ext2).fs_dev);
    if sp.is_null() {
        return EINVAL;
    }

    // FIXME: fill statvfs struct and copy to user via grant
    // Fields:
    let _f_flag = 0; // ST_NOTRUNC
    let _f_bsize = (*sp).s_block_size;
    let _f_frsize = (*sp).s_block_size;
    let _f_iosize = (*sp).s_block_size;
    let _f_blocks = (*sp).s_blocks_count;
    let _f_bfree = (*sp).s_free_blocks_count;
    let _f_bavail = (*sp).s_free_blocks_count - (*sp).s_r_blocks_count;
    let _f_files = (*sp).s_inodes_count;
    let _f_ffree = (*sp).s_free_inodes_count;
    let _f_favail = (*sp).s_free_inodes_count;
    let _f_namemax = EXT2_NAME_MAX as u64;

    OK
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
