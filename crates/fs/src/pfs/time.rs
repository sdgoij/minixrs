//! File timestamp operations — adapted from `minix/fs/mfs/time.c`

use crate::pfs::consts::*;
use crate::pfs::glo;

/// Update access and modification times of a pipe inode.
///
/// In PFS, `fs_utime` updates the atime and mtime fields of an inode.
// Reference: time.c fs_utime()
pub fn fs_utime() -> i32 {
    todo!("fs_utime: not yet wired — requires IPC message parsing")
}

/// Set access time on an inode.
pub fn pfs_set_atime(inode_idx: u16, time_val: i64) {
    unsafe {
        let inode = &mut *glo::get_inode_ptr(inode_idx as usize);
        (*inode).i_atime = time_val;
        (*inode).i_update = ((*inode).i_update as u32 & !ATIME) as u8;
    }
}

/// Set modification time on an inode.
pub fn pfs_set_mtime(inode_idx: u16, time_val: i64) {
    unsafe {
        let inode = &mut *glo::get_inode_ptr(inode_idx as usize);
        (*inode).i_mtime = time_val;
        (*inode).i_update = ((*inode).i_update as u32 & !MTIME) as u8;
    }
}

/// Set change time on an inode.
pub fn pfs_set_ctime(inode_idx: u16, time_val: i64) {
    unsafe {
        let inode = &mut *glo::get_inode_ptr(inode_idx as usize);
        (*inode).i_ctime = time_val;
        (*inode).i_update = ((*inode).i_update as u32 & !CTIME) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            crate::pfs::inode::init_inode_cache();
        }
    }

    #[test]
    fn test_pfs_set_atime() {
        init();
        let ip = crate::pfs::inode::get_inode(1, 1).unwrap();
        pfs_set_atime(ip, 12345);
        unsafe {
            assert_eq!((*glo::get_inode_ptr(ip as usize)).i_atime, 12345);
        }
    }

    #[test]
    fn test_pfs_set_mtime() {
        init();
        let ip = crate::pfs::inode::get_inode(1, 2).unwrap();
        pfs_set_mtime(ip, 67890);
        unsafe {
            assert_eq!((*glo::get_inode_ptr(ip as usize)).i_mtime, 67890);
        }
    }

    #[test]
    fn test_pfs_set_ctime() {
        init();
        let ip = crate::pfs::inode::get_inode(1, 3).unwrap();
        pfs_set_ctime(ip, 99999);
        unsafe {
            assert_eq!((*glo::get_inode_ptr(ip as usize)).i_ctime, 99999);
        }
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_utime_panics() {
        fs_utime();
    }
}
