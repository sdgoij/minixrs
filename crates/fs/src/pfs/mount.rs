//! Unmount operation — adapted from `minix/fs/pfs/mount.c`

use crate::pfs::consts::*;
use crate::pfs::glo;

/// Unmount the Pipe File System (REQ_UNMOUNT).
///
/// Refuses to unmount while pipe inodes are still in use.
// Reference: mount.c fs_unmount()
pub fn fs_unmount() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();

        // Check if any inodes are still in use.
        let mut in_use = 0;
        for i in 0..PFS_NR_INODES {
            let inode = &*glo::get_inode_ptr(i);
            if (*inode).i_count > 0 && (*inode).i_dev == (*pfs).fs_dev {
                in_use += (*inode).i_count;
            }
        }

        // Root inode is always allocated; expect only 1 reference.
        if in_use > 1 {
            return EBUSY;
        }

        (*pfs).unmountdone = TRUE;
        OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_unmount_not_busy() {
        unsafe {
            glo::pfs_init_globals();
        }
        // No inodes in use, so unmount should succeed.
        let r = fs_unmount();
        assert_eq!(r, OK);
    }

    #[test]
    fn test_fs_unmount_busy() {
        unsafe {
            glo::pfs_init_globals();
            crate::pfs::inode::init_inode_cache();
            let pfs = glo::pfs_ptr();
            (*pfs).fs_dev = 1;
        }
        // A pipe inode with two references on the device makes the FS busy.
        crate::pfs::inode::get_inode(1, 42);
        crate::pfs::inode::get_inode(1, 42);
        let r = fs_unmount();
        assert_eq!(r, EBUSY);
    }
}
