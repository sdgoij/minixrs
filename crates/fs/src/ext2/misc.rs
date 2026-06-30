//! Miscellaneous operations — adapted from `minix/fs/ext2/misc.c`

use core::sync::atomic::Ordering;

use libs::libminixfs::cache::{lmfs_flushall, lmfs_get_block_ino, lmfs_invalidate, lmfs_put_block};
use libs::libminixfs::constants::{FULL_DATA_BLOCK, NORMAL};

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_sync — flush all tables to disk.
pub unsafe fn fs_sync() -> i32 {
    let sp = glo::SUPERBLOCK.load(Ordering::Relaxed);
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

    lmfs_flushall();

    if (*sp).s_dev != NO_DEV {
        (*sp).s_wtime = clock_time() as u32;
        write_super(&mut *sp);
    }

    OK
}

/// fs_flush — flush blocks of all devices.
pub unsafe fn fs_flush() -> i32 {
    let ext2 = glo::ext2_ptr();
    let dev = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing

    if dev == (*ext2).fs_dev {
        return EBUSY;
    }

    lmfs_flushall();
    lmfs_invalidate(dev);

    OK
}

/// fs_new_driver — set a new driver endpoint.
pub unsafe fn fs_new_driver() -> i32 {
    // TODO: implement
    OK
}

/// fs_bpeek — block peek (read without consuming).
pub unsafe fn fs_bpeek() -> i32 {
    let ext2 = glo::ext2_ptr();
    // FIXME: parse (dev, start, len) from fs_m_in message
    let dev = (*ext2).fs_dev;
    let start: u64 = 0;
    let len: u64 = 0;

    if len == 0 {
        return EINVAL;
    }

    let block_size = get_block_size(dev) as u64;
    let start_block = start / block_size;
    let num_blocks = (len + block_size - 1) / block_size;

    for b in start_block..start_block + num_blocks {
        let bp = lmfs_get_block_ino(dev, b, NORMAL, libs::libminixfs::constants::VMC_NO_INODE, 0);
        if !bp.is_null() {
            lmfs_put_block(bp, FULL_DATA_BLOCK);
        }
    }

    OK
}
