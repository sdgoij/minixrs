//! Link, unlink, rename, readlink — adapted from `minix/fs/pfs/link.c`
//!
//! Pipes do not support hard links or directory renames.
//! The only link-related operation is `fs_ftrunc` for pipe truncation.

use crate::pfs::consts::*;

/// Truncate a pipe inode.
///
/// Only truncation to size 0 is supported (pipes cannot grow via truncate).
///
/// Returns OK on success, EINVAL if `newsize != 0`.
// Reference: link.c fs_ftrunc(), truncate_inode()
pub fn fs_ftrunc() -> i32 {
    todo!("fs_ftrunc: not yet wired — requires IPC message parsing")
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
    #[should_panic(expected = "not yet wired")]
    fn test_fs_ftrunc_panics() {
        fs_ftrunc();
    }
}
