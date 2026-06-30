//! ISO 9660 mount/unmount — adapted from `minix/fs/iso9660fs/mount.c`

use libs::libminixfs::cache::lmfs_set_blocksize;

use crate::iso9660::consts::*;
use crate::iso9660::glo;
use crate::iso9660::inode;
use crate::iso9660::super_block;

/// `fs_readsuper()` — called by VFS to mount the filesystem.
///
/// Reads the super block (volume descriptors), validates the standard ID,
/// and returns root inode properties.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_readsuper() -> i32 {
    let isofs = glo::isofs_ptr();
    let v_pri = glo::v_pri_ptr();

    (*isofs).fs_dev = 0; // stub: device number from fs_m_in
    // In the real implementation:
    //   fs_dev = fs_m_in.m_vfs_fs_readsuper.device;
    //   label_gid = fs_m_in.m_vfs_fs_readsuper.grant;
    //   label_len = fs_m_in.m_vfs_fs_readsuper.path_len;
    //   sys_safecopyfrom(...)
    //   bdev_driver(fs_dev, fs_dev_label);
    //   bdev_open(fs_dev, BDEV_R_BIT);

    // Read the volume descriptors
    let r = super_block::read_vds((*isofs).fs_dev);
    if r != OK {
        // bdev_close(fs_dev);
        return r;
    }

    // Validate standard ID
    if &(*v_pri).standard_id != ISO9660_STANDARD_ID {
        return EINVAL;
    }

    // Set block size
    let block_size = (*v_pri).logical_block_size_l;
    if block_size < ISO9660_MIN_BLOCK_SIZE as u16 {
        return EINVAL;
    }
    lmfs_set_blocksize(block_size as u32, 0); // major dev is 0 for ISO

    // Return root inode properties
    // In the real implementation these go to fs_m_out:
    //   fs_m_out.m_fs_vfs_readsuper.inode = ID_DIR_RECORD(v_pri.dir_rec_root);
    //   fs_m_out.m_fs_vfs_readsuper.mode = v_pri.dir_rec_root.d_mode;
    //   fs_m_out.m_fs_vfs_readsuper.file_size = v_pri.dir_rec_root.d_file_size;
    //   fs_m_out.m_fs_vfs_readsuper.uid = SYS_UID;
    //   fs_m_out.m_fs_vfs_readsuper.gid = SYS_GID;
    //   fs_m_out.m_fs_vfs_readsuper.flags = RES_NOFLAGS;

    OK
}

/// `fs_unmount()` — unmount the filesystem.
///
/// Releases the primary volume descriptor and closes the device.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_unmount() -> i32 {
    super_block::release_v_pri();
    // bdev_close(fs_dev);
    (*glo::isofs_ptr()).unmountdone = true;
    OK
}

/// `fs_mountpoint()` — check if the given inode can be a mount point.
///
/// Returns OK if the inode is a directory and not already a mount point.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_mountpoint() -> i32 {
    let mut r = OK;

    // In the real implementation the inode comes from fs_m_in:
    //   ino = fs_m_in.m_vfs_fs_mountpoint.inode;
    let ino: u32 = 1; // stub

    let rip = inode::get_dir_record(ino);
    if rip.is_null() {
        return EINVAL;
    }

    if (*rip).d_mountpoint {
        r = EBUSY;
    }

    if ((*rip).d_mode & I_TYPE) != I_DIRECTORY {
        r = ENOTDIR;
    }

    inode::release_dir_record(rip);

    if r == OK {
        (*rip).d_mountpoint = true;
    }

    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::glo;

    #[test]
    fn test_fs_readsuper_stub() {
        unsafe {
            glo::isofs_init_globals();
            inode::init_inode_cache();
            // Will return EINVAL because block I/O stub returns zeros
            let r = fs_readsuper();
            // The buffer is zero-filled, so standard_id won't match "CD001"
            assert_eq!(r, EINVAL);
        }
    }

    #[test]
    fn test_fs_unmount_stub() {
        unsafe {
            glo::isofs_init_globals();
            inode::init_inode_cache();
            let r = fs_unmount();
            assert_eq!(r, OK);
            assert!((*glo::isofs_ptr()).unmountdone);
        }
    }

    #[test]
    fn test_fs_mountpoint_no_inode() {
        unsafe {
            glo::isofs_init_globals();
            inode::init_inode_cache();
            let r = fs_mountpoint();
            // With no inode in cache, get_dir_record returns null -> EINVAL
            assert_eq!(r, EINVAL);
        }
    }
}
