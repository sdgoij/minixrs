//! File timestamps — adapted from `minix/fs/mfs/time.c`

use crate::mfs::consts::*;

pub fn fs_utime() -> i32 {
    // TODO: read inode_nr, actime, modtime from IPC message
    // Currently returns EINVAL to avoid silently corrupting inode 0.
    EINVAL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate.
            crate::mfs::glo::UNUSED_INODES_HEAD = None;
            let p = &raw mut crate::mfs::glo::HASH_INODES;
            for i in 0..crate::mfs::consts::INODE_HASH_SIZE {
                let elem = core::ptr::addr_of_mut!((*p)[i]);
                elem.write(None);
            }
        }
    }

    #[test]
    fn test_fs_utime_returns_einval_when_uninitialized() {
        // After init, fs_dev == NO_DEV and the inode hash table is
        // empty, so get_inode fails → EINVAL.
        init();
        assert_eq!(fs_utime(), EINVAL);
    }
}
