//! Pipe creation and special file creation — adapted from `minix/fs/pfs/open.c`

use crate::pfs::buffer::*;
use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::*;

/// Create a new pipe inode.
///
/// Allocates an inode of type `I_NAMED_PIPE` on the given device.
/// Returns the inode number and metadata through the VFS message.
// Reference: open.c fs_newnode()
pub fn fs_newnode() -> i32 {
    todo!("fs_newnode: not yet wired — requires IPC message parsing")
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
    #[should_panic(expected = "not yet wired")]
    fn test_fs_newnode_panics() {
        fs_newnode();
    }
}
