//! Miscellaneous operations — adapted from `minix/fs/ext2/misc.c`

use core::sync::atomic::Ordering;

use libs::libminixfs::cache::{lmfs_do_bpeek, lmfs_flushall, lmfs_invalidate};

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::inode::*;
use crate::ext2::super_::*;
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

/// fs_flush — drop a device's blocks from the cache, after writing the dirty
/// ones out.
///
/// Message layout (VFS `req_flush`): device (u32) at payload[0].
///
/// Reference: misc.c fs_flush()
pub unsafe fn fs_flush() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;
    let dev = payload_u32(&raw, 0);

    if dev == (*ext2).fs_dev {
        return EBUSY;
    }

    lmfs_flushall();
    lmfs_invalidate(dev);

    OK
}

/// fs_new_driver — point a device at a (new) block driver endpoint.
///
/// Message layout (VFS `req_newdriver`): device (u32) at payload[0], label
/// grant (i32) at payload[8], label length (u64) at payload[16]. The label
/// lives in the caller's address space and reaches the server through that
/// grant; a host build has no kernel to copy it through and says so.
///
/// Reference: misc.c fs_new_driver()
pub unsafe fn fs_new_driver() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;

    let dev = payload_u32(&raw, 0);
    let len = payload_u64(&raw, 16) as usize;

    if len > LABEL_MAX {
        return EINVAL;
    }

    #[cfg(target_os = "minix")]
    {
        let mut label = [0u8; LABEL_MAX];
        let r = crate::ext2::read::safecopy_from_grant(
            payload_i32(&raw, 8),
            0,
            label.as_mut_ptr(),
            len,
        );
        if r != OK {
            return EINVAL;
        }
        crate::block_io::bdev_driver(dev, &label[..len]);
        OK
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (dev, len);
        libs::libminixfs::errors::ENOSYS
    }
}

/// fs_bpeek — fault a device's blocks into the cache without handing them to a
/// caller.
///
/// Message layout (VFS `req_bpeek`): device (u32) at payload[0], seek position
/// (i64) at payload[8], length (u64) at payload[24].
///
/// Reference: misc.c fs_bpeek()
pub unsafe fn fs_bpeek() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;

    lmfs_do_bpeek(
        payload_u32(&raw, 0),
        payload_i64(&raw, 8) as u64,
        payload_u64(&raw, 24),
    )
}
