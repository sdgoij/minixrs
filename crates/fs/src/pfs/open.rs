//! Pipe creation and special file creation — adapted from `minix/fs/pfs/open.c`

use crate::pfs::buffer::*;
use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::*;

/// Create a new pipe inode (REQ_NEWNODE).
///
/// Allocates an inode of the requested type (uid/gid/mode/device from the
/// message payload) plus a pipe data buffer for FIFOs, then replies with
/// the node details: file_size (i64@0), device (u32@8), inode (u32@12),
/// mode (u16@16), uid (u16@18), gid (u16@20).
// Reference: open.c fs_newnode()
pub fn fs_newnode() -> i32 {
    unsafe {
        let pfs = glo::pfs_ptr();
        let data_ptr = core::ptr::addr_of_mut!((*pfs).m_in_data) as *const u8;
        let dev = core::ptr::read_unaligned(data_ptr.add(0) as *const u32);
        let bits = core::ptr::read_unaligned(data_ptr.add(4) as *const u16);
        let uid = core::ptr::read_unaligned(data_ptr.add(6) as *const u16);
        let gid = core::ptr::read_unaligned(data_ptr.add(8) as *const u16);

        let mut r = OK;
        let rip = match alloc_inode(dev, bits, uid, gid) {
            Some(i) => i,
            None => return -(*pfs).err_code,
        };
        match bits & S_IFMT {
            S_IFBLK | S_IFCHR => {
                (*glo::get_inode_ptr(rip as usize)).i_rdev = dev;
            }
            S_IFIFO => {
                let inum = (*glo::get_inode_ptr(rip as usize)).i_num;
                if get_block(dev, inum).is_none() {
                    r = EIO;
                }
            }
            _ => r = EIO,
        }
        if r != OK {
            free_inode(rip);
            -r
        } else {
            let inode = &*glo::get_inode_ptr(rip as usize);
            let out = core::ptr::addr_of_mut!((*pfs).m_out_data) as *mut u8;
            core::ptr::write_unaligned(out.add(0) as *mut i64, (*inode).i_size);
            core::ptr::write_unaligned(out.add(8) as *mut u32, dev);
            core::ptr::write_unaligned(out.add(12) as *mut u32, (*inode).i_num);
            core::ptr::write_unaligned(out.add(16) as *mut u16, (*inode).i_mode);
            core::ptr::write_unaligned(out.add(18) as *mut u16, (*inode).i_uid);
            core::ptr::write_unaligned(out.add(20) as *mut u16, (*inode).i_gid);
            r
        }
    }
}

/// Create a pipe inode.
///
/// Actually allocates the inode. Used internally by PFS.
pub fn pfs_create_pipe(dev: u32, uid: u16, gid: u16) -> Option<u16> {
    let rip = alloc_inode(dev, I_NAMED_PIPE, uid, gid)?;

    // Allocate a buffer for the pipe data
    unsafe {
        let inum = (*glo::get_inode_ptr(rip as usize)).i_num;
        if get_block(dev, inum).is_none() {
            // Buffer allocation failed — clean up
            free_inode(rip);
            put_inode(Some(rip));
            return None;
        }
    }

    Some(rip)
}

/// Create a special file node (block or character device).
///
/// Stub — PFS only supports pipes.
pub fn fs_mknod() -> i32 {
    ENOSYS
}

/// Create a symbolic link.
///
/// Stub — PFS does not support symlinks.
pub fn fs_slink() -> i32 {
    ENOSYS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pfs::buffer::init_buffer_pool;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            init_inode_cache();
            init_buffer_pool();
        }
    }

    #[test]
    fn test_pfs_create_pipe() {
        init();
        let ip = pfs_create_pipe(1, 100, 200);
        assert!(ip.is_some());
        let idx = ip.unwrap();
        unsafe {
            let inode = &*glo::get_inode_ptr(idx as usize);
            assert_eq!((*inode).i_mode, I_NAMED_PIPE);
            assert_eq!((*inode).i_uid, 100);
            assert_eq!((*inode).i_gid, 200);
        }
    }

    #[test]
    fn test_fs_mknod_returns_enosys() {
        assert_eq!(fs_mknod(), ENOSYS);
    }

    #[test]
    fn test_fs_slink_returns_enosys() {
        assert_eq!(fs_slink(), ENOSYS);
    }

    #[test]
    fn test_fs_newnode_creates_pipe() {
        init();
        unsafe {
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, NO_DEV);
            core::ptr::write_unaligned(data.add(4) as *mut u16, I_NAMED_PIPE);
            core::ptr::write_unaligned(data.add(6) as *mut u16, 100);
            core::ptr::write_unaligned(data.add(8) as *mut u16, 200);
        }
        assert_eq!(fs_newnode(), OK);
        unsafe {
            let pfs = glo::pfs_ptr();
            let out = core::ptr::addr_of!((*pfs).m_out_data) as *const u8;
            let inode_nr = core::ptr::read_unaligned(out.add(12) as *const u32);
            assert!(inode_nr > 0);
            let mode = core::ptr::read_unaligned(out.add(16) as *const u16);
            assert_eq!(mode, I_NAMED_PIPE);
        }
    }

    #[test]
    fn test_fs_newnode_rejects_unsupported_mode() {
        init();
        unsafe {
            let pfs = glo::pfs_ptr();
            let data = core::ptr::addr_of_mut!((*pfs).m_in_data) as *mut u8;
            core::ptr::write_unaligned(data.add(0) as *mut u32, NO_DEV);
            core::ptr::write_unaligned(data.add(4) as *mut u16, I_REGULAR);
            core::ptr::write_unaligned(data.add(6) as *mut u16, 0);
            core::ptr::write_unaligned(data.add(8) as *mut u16, 0);
        }
        assert_eq!(fs_newnode(), -EIO);
    }
}
