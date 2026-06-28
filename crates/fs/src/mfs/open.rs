//! File creation ops — adapted from `minix/fs/mfs/open.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;

pub fn fs_create() -> i32 {
    todo!("fs_create: not yet wired")
}
pub fn fs_mkdir() -> i32 {
    todo!("fs_mkdir: not yet wired")
}
pub fn fs_mknod() -> i32 {
    todo!("fs_mknod: not yet wired")
}
pub fn fs_slink() -> i32 {
    todo!("fs_slink: not yet wired")
}

pub fn fs_inhibread() -> i32 {
    unsafe {
        let _inode_nr: u32 = 0;
        if let Some(rip) = find_inode((*glo::mfs_ptr()).fs_dev, _inode_nr) {
            let inode = &mut *glo::get_inode_ptr(rip as usize);
            (*inode).i_seek = ISEEK;
            OK
        } else {
            EINVAL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe { crate::mfs::glo::mfs_init_globals(); }
    }

    #[test]
    fn test_fs_inhibread_returns_einval_when_uninitialized() {
        // After init, fs_dev == NO_DEV and the inode hash table is
        // empty, so find_inode fails → EINVAL.
        init();
        assert_eq!(fs_inhibread(), EINVAL);
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_create_panics() {
        fs_create();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_mkdir_panics() {
        fs_mkdir();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_mknod_panics() {
        fs_mknod();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_slink_panics() {
        fs_slink();
    }
}
