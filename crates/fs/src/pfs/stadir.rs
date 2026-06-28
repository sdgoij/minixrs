//! Stat / statvfs operations — adapted from `minix/fs/pfs/stadir.c`

use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::inode::*;

/// Stat a pipe inode.
///
/// Returns file metadata including size, mode, timestamps, etc.
// Reference: stadir.c fs_stat()
pub fn fs_stat() -> i32 {
    todo!("fs_stat: not yet wired — requires IPC message parsing")
}

/// Statvfs — not meaningful for PFS (no block storage).
///
/// Returns a minimal statvfs indicating no space information.
// Reference: VFS protocol
pub fn fs_statvfs() -> i32 {
    OK
}

/// Internal helper to stat an inode and fill the stat buffer.
///
/// Returns 0 on success, or a negative errno.
// Reference: stadir.c stat_inode()
pub fn stat_inode(rip_idx: u16) -> i32 {
    unsafe {
        let inode = &*glo::get_inode_ptr(rip_idx as usize);

        // Update times if needed
        if (*inode).i_update != 0 {
            update_times(rip_idx);
        }

        // In a real implementation, this would copy a `struct stat`
        // to user space via IPC. For now, just return OK.
        let _mode = (*inode).i_mode;
        let _size = (*inode).i_size;
        let _uid = (*inode).i_uid;
        let _gid = (*inode).i_gid;
        let _nlinks = (*inode).i_nlinks;

        OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            init_inode_cache();
        }
    }

    #[test]
    fn test_stat_inode() {
        init();
        let ip = crate::pfs::inode::get_inode(1, 1).unwrap();
        let r = stat_inode(ip);
        assert_eq!(r, OK);
    }

    #[test]
    fn test_fs_statvfs_returns_ok() {
        assert_eq!(fs_statvfs(), OK);
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_stat_panics() {
        fs_stat();
    }
}
