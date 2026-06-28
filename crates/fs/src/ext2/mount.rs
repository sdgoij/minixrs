//! Mount/unmount — adapted from `minix/fs/ext2/mount.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_readsuper — read super block and get root inode.
pub unsafe fn fs_readsuper() -> i32 {
    let ext2 = glo::ext2_ptr();

    // TODO: parse message for device, label, flags
    // For now, stub
    let _ = ext2;
    EINVAL
}

/// fs_unmount — unmount a file system.
pub unsafe fn fs_unmount() -> i32 {
    let sp = glo::SUPERBLOCK;
    if sp.is_null() {
        return EINVAL;
    }

    let ext2 = glo::ext2_ptr();

    if (*sp).s_dev != (*ext2).fs_dev {
        return EINVAL;
    }

    // Count open inodes on this device
    let mut count = 0;
    for i in 0..NR_INODES {
        let rip = glo::get_inode_ptr(i);
        if (*rip).i_count > 0 && (*rip).i_dev == (*ext2).fs_dev {
            count += (*rip).i_count;
        }
    }

    let root_ip = find_inode((*ext2).fs_dev, ROOT_INODE);
    if root_ip.is_null() {
        return EINVAL;
    }

    // Sync before checking count
    if (*sp).s_rd_only == 0 {
        fs_sync_impl();
    }

    if count > 1 {
        return EBUSY;
    }

    put_inode(root_ip);

    if (*sp).s_rd_only == 0 {
        (*sp).s_wtime = clock_time() as u32;
        (*sp).s_state = EXT2_VALID_FS;
        write_super(&mut *sp);
    }

    // TODO: bdev_close(fs_dev);
    // TODO: lmfs_invalidate(fs_dev);

    (*sp).s_dev = NO_DEV;
    (*ext2).unmountdone = TRUE;

    OK
}

/// fs_mountpoint — check mount point.
pub unsafe fn fs_mountpoint() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message for inode
    let _ = ext2;
    EINVAL
}

unsafe fn fs_sync_impl() {
    for i in 0..NR_INODES {
        let rip = glo::get_inode_ptr(i);
        if (*rip).i_count > 0 && (*rip).i_dirt == IN_DIRTY {
            rw_inode(rip, WRITING);
        }
    }
    // TODO: lmfs_flushall();

    let sp = glo::SUPERBLOCK;
    if !sp.is_null() && (*sp).s_dev != NO_DEV {
        (*sp).s_wtime = clock_time() as u32;
        write_super(&mut *sp);
    }
}
