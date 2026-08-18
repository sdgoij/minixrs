//! Miscellaneous operations — adapted from `minix/fs/pfs/misc.c`

use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::{find_inode, get_inode, put_inode};

/// Sync: no-op for PFS (all data is in-memory).
// Reference: misc.c fs_sync()
pub fn fs_sync() -> i32 {
    OK
}

/// Change mode of a pipe inode (REQ_CHMOD).
///
/// Replaces the permission bits, keeping the type bits (C: mode & ALL_MODES).
/// Replies with the new mode (VFS req_chmod reads it to refresh v_mode).
// Reference: misc.c fs_chmod()
pub fn fs_chmod() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();
        let data_ptr = core::ptr::addr_of_mut!((*pfs).m_in_data) as *const u8;
        let inum = core::ptr::read_unaligned(data_ptr.add(0) as *const u32);
        let mode = core::ptr::read_unaligned(data_ptr.add(4) as *const u16);
        let rip_idx = match find_inode(inum) {
            Some(i) => i,
            None => return -EINVAL,
        };
        get_inode((*glo::get_inode_ptr(rip_idx as usize)).i_dev, inum);
        let inode = glo::get_inode_ptr(rip_idx as usize);
        (*inode).i_mode = ((*inode).i_mode & !ALL_MODES) | (mode & ALL_MODES);
        let new_mode = (*inode).i_mode;
        let out = core::ptr::addr_of_mut!((*pfs).m_out_data) as *mut u8;
        core::ptr::write_unaligned(out.add(0) as *mut u16, new_mode);
        put_inode(Some(rip_idx));
        OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_sync_returns_ok() {
        assert_eq!(fs_sync(), OK);
    }

    #[test]
    fn test_fs_chmod_updates_mode_and_replies() {
        unsafe {
            glo::pfs_init_globals();
            crate::pfs::inode::init_inode_cache();
        }
        let ip = crate::pfs::inode::get_inode(1, 80).unwrap();
        unsafe {
            (*glo::get_inode_ptr(ip as usize)).i_mode = I_NAMED_PIPE | 0o600;
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, 80);
            core::ptr::write_unaligned(data.add(4) as *mut u16, 0o640);
        }
        assert_eq!(fs_chmod(), OK);
        unsafe {
            let inode = &*glo::get_inode_ptr(ip as usize);
            assert_eq!((*inode).i_mode, I_NAMED_PIPE | 0o640);
            let pfs = glo::pfs_ptr();
            let out = core::ptr::addr_of!((*pfs).m_out_data) as *const u8;
            let replied = core::ptr::read_unaligned(out.add(0) as *const u16);
            assert_eq!(replied, I_NAMED_PIPE | 0o640);
        }
    }

    #[test]
    fn test_fs_chmod_unknown_inode() {
        unsafe {
            glo::pfs_init_globals();
            crate::pfs::inode::init_inode_cache();
        }
        unsafe {
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, 999);
            core::ptr::write_unaligned(data.add(4) as *mut u16, 0o600);
        }
        assert_eq!(fs_chmod(), -EINVAL);
    }
}
