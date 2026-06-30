//! Miscellaneous operations — adapted from `minix/fs/mfs/misc.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;

pub fn fs_flush() -> i32 {
    OK
}

pub fn fs_sync() -> i32 {
    unsafe {
        for i in 0..NR_INODES {
            let rip = &*glo::get_inode_ptr(i);
            if (*rip).i_count > 0 && (*rip).i_dirt == IN_DIRTY {
                rw_inode(i as u16, WRITING);
            }
        }
    }
    OK
}

pub fn fs_new_driver() -> i32 {
    // Stub: returns OK; the real implementation reads the device
    // and label from the IPC message and calls bdev_driver(dev, label).
    OK
}

pub fn fs_bpeek() -> i32 {
    // Block peek stub: the real implementation delegates to
    // lmfs_do_bpeek(&fs_m_in). Return OK for now.
    OK
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_fs_sync() {
        assert_eq!(fs_sync(), OK);
    }
}
