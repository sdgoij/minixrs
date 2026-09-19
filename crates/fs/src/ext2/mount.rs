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
///
/// Reference: mount.c fs_readsuper()
pub unsafe fn fs_readsuper() -> i32 {
    let ext2 = glo::ext2_ptr();

    // VFS req_readsuper writes the device at payload+0 (m1.m1i1), the flags at
    // payload+4, the driver label length at payload+8 (m1.m1i3) and the label
    // grant at payload+24.
    let dev = (*ext2).m_in.m_payload.m1.m1i1 as u32;
    if dev == NO_DEV {
        return EINVAL;
    }

    // Mount flags: a read-only mount never writes the filesystem, and the root
    // mount is where `..` stops instead of leaving the filesystem.
    let req_flags = payload_u32(&(*ext2).m_in.m_payload.raw, 4);
    let readonly = (req_flags & REQ_RDONLY) != 0;
    let isroot = (req_flags & REQ_ISROOT) != 0;

    // Resolve the block driver serving `dev` from the granted label so every
    // block request for this device is routed to that driver; when the named
    // driver has no such device the device is re-registered to the ramdisk
    // driver (libbdev bdev_driver / bdev_driver_root).
    #[cfg(target_os = "minix")]
    {
        let label_len = (*ext2).m_in.m_payload.m1.m1i3 as usize;
        let label_grant = i32::from_ne_bytes(
            (&(*ext2).m_in.m_payload.raw)[24..28]
                .try_into()
                .unwrap_or([0u8; 4]),
        );
        if label_len > 0 && label_len < LABEL_MAX {
            let mut label = [0u8; LABEL_MAX];
            let r = crate::block_io::safecopy_from(
                (*ext2).m_in.m_source,
                label_grant,
                &mut label[..label_len],
            );
            if r == 0 {
                let _ = crate::block_io::bdev_driver_root(dev, &label[..label_len]);
            }
        }
    }

    let sp = glo::SUPERBLOCK.load(Ordering::Relaxed);
    if sp.is_null() {
        // First mount: the ext2 superblock sits at byte 1024, i.e. block 1 of
        // the 1024-byte pre-mount block size set up by init_server().
        let bp = lmfs_get_block(dev, 1);
        if bp.is_null() {
            return EINVAL;
        }

        let sp_data = (*bp).data_ptr;
        let new_sp = allocate_superblock();
        if new_sp.is_null() {
            lmfs_put_block(bp, FULL_DATA_BLOCK);
            return EINVAL;
        }
        // Only the on-disk half of `SuperBlock` is in the block: the struct
        // continues with in-memory fields (`s_dev`, `s_block_size`, ...) that
        // `read_super` fills in, and the buffer holds exactly one block. Copying
        // the whole struct read past the block, and when the destination landed
        // inside that overrun the two ranges overlapped - a
        // `copy_nonoverlapping` precondition violation, not just a wasted copy
        // (it is what failed the Linux host tests, where the two allocations sat
        // closer together than on Windows). Byte pointers, so the buffer's own
        // alignment never matters either.
        core::ptr::copy_nonoverlapping(sp_data, new_sp as *mut u8, SUPER_SIZE_D);
        lmfs_put_block(bp, FULL_DATA_BLOCK);

        (*new_sp).s_dev = dev;
        // read_super validates the on-disk fields, loads the group descriptor
        // table and switches the buffer cache to this filesystem's block size.
        let r = read_super(&mut *new_sp);
        if r != OK {
            return r;
        }

        // A filesystem that was not cleanly unmounted carries EXT2_ERROR_FS
        // until an fsck clears it; mounting it read-write would continue from
        // whatever state the crash left behind.
        if !readonly && (*new_sp).s_state == EXT2_ERROR_FS {
            return EINVAL;
        }

        glo::SUPERBLOCK.store(new_sp, Ordering::Relaxed);
    } else if (*sp).s_dev != dev {
        // Already mounted, but on a different device.
        return EINVAL;
    }

    // `sp` was read before the mount branch; the first mount installs the
    // superblock it just filled in.
    let sp = glo::SUPERBLOCK.load(Ordering::Relaxed);

    (*ext2).fs_dev = dev;

    // The root inode must exist and be a directory.
    let root_ip = get_inode(dev, ROOT_INODE);
    if root_ip.is_null() {
        return EINVAL;
    }
    if (*root_ip).i_mode == 0 || ((*root_ip).i_mode & I_TYPE) != I_DIRECTORY {
        put_inode(root_ip);
        return EINVAL;
    }

    // The flags apply to every successful mount, including a re-mount of an
    // already-read superblock.
    (*sp).s_rd_only = readonly as i32;
    (*sp).s_is_root = isroot as u8;

    if !readonly {
        // While this mount is up the on-disk state says the filesystem is in
        // use: an unclean one makes the next mount refuse (or, for a reader,
        // is visible to fsck).
        (*sp).s_state = EXT2_ERROR_FS;
        (*sp).s_mnt_count = (*sp).s_mnt_count.wrapping_add(1);
        (*sp).s_mtime = clock_time() as u32;
        write_super(&mut *sp);
    }

    // Reply fields VFS req_readsuper reads back:
    //   file_size (i64) at payload+0  → m1i1 (low) + m1i2 (high)
    //   dev       (u32) at payload+8  → m1i3
    //   inode_nr  (u32) at payload+12 → m1i4
    //   flags     (u32) at payload+16 → m1i5
    //   mode      (u16) at payload+20 → m1i6
    let root = &*root_ip;
    let size = root.i_size as i64;
    (*ext2).m_out.m_payload.m1.m1i1 = size as i32;
    (*ext2).m_out.m_payload.m1.m1i2 = (size >> 32) as i32;
    (*ext2).m_out.m_payload.m1.m1i3 = dev as i32;
    (*ext2).m_out.m_payload.m1.m1i4 = ROOT_INODE as i32;
    (*ext2).m_out.m_payload.m1.m1i5 = 0;
    (*ext2).m_out.m_payload.m1.m1i6 = root.i_mode as i32;

    OK
}

/// Allocate a new SuperBlock.
pub(crate) unsafe fn allocate_superblock() -> *mut SuperBlock {
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

/// fs_mountpoint — check and set a mount point.
///
/// Message layout (VFS `req_mountpoint`): inode (u32) at payload[0].
///
pub unsafe fn fs_mountpoint() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;
    let inode_num = payload_u32(&raw, 0);

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
