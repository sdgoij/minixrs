//! Miscellaneous operations — adapted from `minix/fs/ext2/misc.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_sync — flush all tables to disk.
pub unsafe fn fs_sync() -> i32 {
    let sp = glo::SUPERBLOCK;
    if sp.is_null() || (*sp).s_rd_only != 0 {
        return OK;
    }

    // Write all dirty inodes
    for i in 0..NR_INODES {
        let rip = glo::get_inode_ptr(i);
        if (*rip).i_count > 0 && (*rip).i_dirt == IN_DIRTY {
            rw_inode(rip, WRITING);
        }
    }

    // TODO: lmfs_flushall();

    if (*sp).s_dev != NO_DEV {
        (*sp).s_wtime = clock_time() as u32;
        write_super(&mut *sp);
    }

    OK
}

/// fs_flush — flush blocks of a device.
pub unsafe fn fs_flush() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message for device
    let dev = (*ext2).fs_dev;

    if dev == (*ext2).fs_dev {
        return EBUSY;
    }

    // TODO: lmfs_flushall();
    // TODO: lmfs_invalidate(dev);

    OK
}

/// fs_new_driver — set a new driver endpoint.
pub unsafe fn fs_new_driver() -> i32 {
    // TODO: implement
    OK
}

/// fs_bpeek — block peek.
pub fn fs_bpeek() -> i32 {
    // TODO: lmfs_do_bpeek
    EINVAL
}
