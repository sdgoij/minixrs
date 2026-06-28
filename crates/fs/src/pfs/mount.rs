//! Mount / unmount operations — adapted from `minix/fs/pfs/mount.c` and `super.c`

use crate::pfs::buffer::*;
use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::*;

/// Mount the Pipe File System.
///
/// Initializes the inode table and buffer pool for a fresh PFS instance.
/// PFS has no on-disk super block, so this is purely in-memory setup.
// Reference: mount.c fs_readsuper() + main.c sef_cb_init_fresh()
pub fn fs_readsuper() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();

        // The device will be set by VFS in the message; for now use a default
        (*pfs).fs_dev = 1; // Will be replaced by actual device from message

        // Initialize inode table (if not already done)
        init_inode_cache();
        init_buffer_pool();

        OK
    }
}

/// Unmount the Pipe File System.
///
/// Checks that the filesystem is not busy (no inodes in use) before
/// allowing unmount.
// Reference: mount.c fs_unmount()
pub fn fs_unmount() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();

        // Check if any inodes are still in use
        let mut in_use = 0;
        for i in 0..PFS_NR_INODES {
            let inode = &*glo::get_inode_ptr(i);
            if (*inode).i_count > 0 && (*inode).i_dev == (*pfs).fs_dev {
                in_use += (*inode).i_count;
            }
        }

        // Root inode is always allocated; expect only 1 reference
        if in_use > 1 {
            return EBUSY;
        }

        (*pfs).unmountdone = TRUE;
        OK
    }
}

/// Check if a path is a mountpoint.
///
/// Stub — PFS does not support nested mounts.
// Reference: mount.c fs_mountpoint() via VFS protocol
pub fn fs_mountpoint() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();
        // Find inode by number from message
        let _inode_nr: u32 = 0; // Will come from message
        let rip = match get_inode((*pfs).fs_dev, _inode_nr) {
            Some(idx) => idx,
            None => return EINVAL,
        };
        put_inode(Some(rip));
        OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
        }
    }

    #[test]
    fn test_fs_readsuper() {
        init();
        let r = fs_readsuper();
        assert_eq!(r, OK);
    }

    #[test]
    fn test_fs_unmount_not_busy() {
        init();
        fs_readsuper();
        // No extra inodes in use, so unmount should succeed
        let r = fs_unmount();
        assert_eq!(r, OK);
    }

    #[test]
    fn test_fs_mountpoint() {
        init();
        // With init done, get_inode can create a new entry, so mountpoint
        // succeeds (returns OK) rather than failing with EINVAL.
        let r = fs_mountpoint();
        assert_eq!(r, OK);
    }
}
