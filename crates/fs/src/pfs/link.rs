//! Link, unlink, rename, readlink — adapted from `minix/fs/pfs/link.c`
//!
//! Pipes do not support hard links or directory renames.
//! The only link-related operation is `fs_ftrunc` for pipe truncation.

use crate::pfs::consts::*;

/// Truncate a pipe inode.
///
/// Unwired in this port: pipe data lives in VFS ring buffers, so VFS never
/// sends PFS requests (see PORTING_PLAN.md Phase 9.6 pfs-wiring).
// Reference: link.c fs_ftrunc(), truncate_inode()
pub fn fs_ftrunc() -> i32 {
    ENOSYS
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
    fn test_fs_ftrunc_returns_enosys() {
        assert_eq!(fs_ftrunc(), ENOSYS);
    }
}
