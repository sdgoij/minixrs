//! Mount/unmount — adapted from `minix/fs/ext2/mount.c`

extern crate alloc;
use core::sync::atomic::Ordering;

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::inode::*;
use crate::ext2::super_::{read_super, write_super};
use crate::ext2::types::*;
use crate::ext2::utility::*;
use libs::libminixfs::cache::{lmfs_flushall, lmfs_get_block, lmfs_invalidate, lmfs_put_block};
use libs::libminixfs::constants::FULL_DATA_BLOCK;
use libs::libminixfs::types::Buf;

/// fs_readsuper — read super block and get root inode.
pub unsafe fn fs_readsuper() -> i32 {
    let ext2 = glo::ext2_ptr();

    // Get device from global message state
    let fs_dev = (*ext2).fs_dev;
    if fs_dev == NO_DEV {
        return EINVAL;
    }

    // Read superblock from block 1 (ext2 superblock location)
    let bp = lmfs_get_block(fs_dev, 1);
    if bp.is_null() {
        return EINVAL;
    }

    // Cast data to SuperBlock and validate
    let sp_data = (*bp).data_ptr as *const SuperBlock;
    let magic = (*sp_data).s_magic;
    if magic != SUPER_MAGIC {
        lmfs_put_block(bp, FULL_DATA_BLOCK);
        return EINVAL;
    }

    // Allocate and initialize superblock
    let sp = allocate_superblock();
    if sp.is_null() {
        lmfs_put_block(bp, FULL_DATA_BLOCK);
        return EINVAL;
    }

    // Copy on-disk fields from the raw block
    core::ptr::copy_nonoverlapping(sp_data, sp, 1);

    // Set device before read_super
    (*sp).s_dev = fs_dev;
    let r = read_super(&mut *sp);
    if r != OK {
        // On failure, put_block the super block buffer and return
        lmfs_put_block(bp, FULL_DATA_BLOCK);
        return r;
    }

    // Release the superblock read buffer (the data is now in our allocated sp)
    lmfs_put_block(bp, FULL_DATA_BLOCK);

    // Store the superblock pointer globally
    glo::SUPERBLOCK.store(sp, Ordering::Relaxed);
    (*ext2).fs_dev = fs_dev;

    // Read the root inode
    let root_ip = get_inode(fs_dev, ROOT_INODE);
    if root_ip.is_null() {
        return EINVAL;
    }

    if (*root_ip).i_mode == 0 || ((*root_ip).i_mode & I_TYPE) != I_DIRECTORY {
        put_inode(root_ip);
        return EINVAL;
    }

    // Initialize reply fields via global state (VFS reply)
    // Root inode properties are set on fs_m_out
    // For now, just mark success

    OK
}

/// Allocate a new SuperBlock.
unsafe fn allocate_superblock() -> *mut SuperBlock {
    // Use the global superblock storage
    let sp = glo::SUPERBLOCK.load(Ordering::Relaxed);
    if !sp.is_null() {
        // Already allocated, zero it
        core::ptr::write_bytes(sp, 0, 1);
        return sp;
    }
    // Fresh allocation
    let layout = core::alloc::Layout::new::<SuperBlock>();
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    ptr as *mut SuperBlock
}

/// fs_unmount — unmount a file system.
pub unsafe fn fs_unmount() -> i32 {
    let sp = glo::SUPERBLOCK.load(Ordering::Relaxed);
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

    // bdev_close(fs_dev) — stub
    lmfs_invalidate((*ext2).fs_dev);

    (*sp).s_dev = NO_DEV;
    (*ext2).unmountdone = TRUE;

    OK
}

/// fs_mountpoint — check mount point.
pub unsafe fn fs_mountpoint() -> i32 {
    let ext2 = glo::ext2_ptr();
    let inode_num = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing

    let rip = get_inode((*ext2).fs_dev, inode_num);
    if rip.is_null() {
        return EINVAL;
    }

    let mut r = OK;
    if (*rip).i_mountpoint != 0 {
        r = EBUSY;
    }

    let bits = (*rip).i_mode & I_TYPE;
    if bits == I_BLOCK_SPECIAL || bits == I_CHAR_SPECIAL {
        r = ENOTDIR;
    }

    put_inode(rip);

    if r == OK {
        // Re-get inode to set mountpoint flag
        let rip2 = get_inode((*ext2).fs_dev, inode_num);
        if !rip2.is_null() {
            (*rip2).i_mountpoint = TRUE;
            put_inode(rip2);
        }
    }

    r
}

unsafe fn fs_sync_impl() {
    for i in 0..NR_INODES {
        let rip = glo::get_inode_ptr(i);
        if (*rip).i_count > 0 && (*rip).i_dirt == IN_DIRTY {
            rw_inode(rip, WRITING);
        }
    }
    lmfs_flushall();

    let sp = glo::SUPERBLOCK.load(Ordering::Relaxed);
    if !sp.is_null() && (*sp).s_dev != NO_DEV {
        (*sp).s_wtime = clock_time() as u32;
        write_super(&mut *sp);
    }
}
