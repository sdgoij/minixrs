//! Link, unlink, rename, readlink — adapted from `minix/fs/pfs/link.c`
//!
//! Pipes do not support hard links or directory renames.
//! The only link-related operation is `fs_ftrunc` for pipe truncation.

use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::{find_inode, truncate_inode};

/// Truncate a pipe inode (REQ_FTRUNC).
///
/// The request carries the inode number (u32 at payload[0]) and the new
/// size as `trc_start` (i64 at payload[8]).  Only truncation to size 0 is
/// supported (pipes cannot grow via truncate).
// Reference: link.c fs_ftrunc(), truncate_inode()
pub fn fs_ftrunc() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();
        let data_ptr = core::ptr::addr_of_mut!((*pfs).m_in_data) as *const u8;
        let inum = core::ptr::read_unaligned(data_ptr.add(0) as *const u32);
        let new_size = core::ptr::read_unaligned(data_ptr.add(8) as *const i64);
        let rip_idx = match find_inode(inum) {
            Some(i) => i,
            None => return -EINVAL,
        };
        let r = truncate_inode(rip_idx, new_size);
        if r == OK { OK } else { -r }
    }
}

/// Create a hard link — not supported for pipes.
pub fn fs_link() -> i32 {
    ENOSYS
}

/// Unlink a pipe — not supported for pipes.
pub fn fs_unlink() -> i32 {
    ENOSYS
}

/// Rename — not supported for pipes.
pub fn fs_rename() -> i32 {
    ENOSYS
}

/// Read a symbolic link — not supported for pipes.
pub fn fs_rdlink() -> i32 {
    ENOSYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_link_returns_enosys() {
        assert_eq!(fs_link(), ENOSYS);
    }

    #[test]
    fn test_fs_unlink_returns_enosys() {
        assert_eq!(fs_unlink(), ENOSYS);
    }

    #[test]
    fn test_fs_rename_returns_enosys() {
        assert_eq!(fs_rename(), ENOSYS);
    }

    #[test]
    fn test_fs_rdlink_returns_enosys() {
        assert_eq!(fs_rdlink(), ENOSYS);
    }

    #[test]
    fn test_fs_ftrunc_truncates_to_zero() {
        unsafe {
            glo::pfs_init_globals();
            crate::pfs::inode::init_inode_cache();
        }
        let ip = crate::pfs::inode::get_inode(1, 70).unwrap();
        unsafe {
            (*glo::get_inode_ptr(ip as usize)).i_size = 100;
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, 70);
            core::ptr::write_unaligned(data.add(8) as *mut i64, 0); // trc_start = 0
        }
        assert_eq!(fs_ftrunc(), OK);
        unsafe {
            assert_eq!((*glo::get_inode_ptr(ip as usize)).i_size, 0);
        }
    }

    #[test]
    fn test_fs_ftrunc_rejects_nonzero() {
        unsafe {
            glo::pfs_init_globals();
            crate::pfs::inode::init_inode_cache();
        }
        crate::pfs::inode::get_inode(1, 71).unwrap();
        unsafe {
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, 71);
            core::ptr::write_unaligned(data.add(8) as *mut i64, 100); // trc_start = 100
        }
        assert_eq!(fs_ftrunc(), -EINVAL);
    }

    #[test]
    fn test_fs_ftrunc_unknown_inode() {
        unsafe {
            glo::pfs_init_globals();
            crate::pfs::inode::init_inode_cache();
        }
        unsafe {
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, 999);
            core::ptr::write_unaligned(data.add(8) as *mut i64, 0);
        }
        assert_eq!(fs_ftrunc(), -EINVAL);
    }
}
