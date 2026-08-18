//! Miscellaneous operations — adapted from `minix/fs/pfs/misc.c`

use crate::pfs::consts::*;

/// Sync: no-op for PFS (all data is in-memory).
// Reference: misc.c fs_sync()
pub fn fs_sync() -> i32 {
    OK
}

/// Flush device: no-op for PFS.
// Reference: misc.c — fs_flush via VFS protocol
pub fn fs_flush() -> i32 {
    OK
}

/// New driver notification: no-op for PFS.
// Reference: misc.c — fs_new_driver via VFS protocol
pub fn fs_new_driver() -> i32 {
    OK
}

/// Change mode of a pipe inode.
///
/// Unwired in this port: pipe data lives in VFS ring buffers, so VFS never
/// sends PFS requests (see PORTING_PLAN.md Phase 9.6 pfs-wiring).
// Reference: misc.c fs_chmod()
pub fn fs_chmod() -> i32 {
    ENOSYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_sync_returns_ok() {
        assert_eq!(fs_sync(), OK);
    }

    #[test]
    fn test_fs_flush_returns_ok() {
        assert_eq!(fs_flush(), OK);
    }

    #[test]
    fn test_fs_new_driver_returns_ok() {
        assert_eq!(fs_new_driver(), OK);
    }

    #[test]
    fn test_fs_chmod_returns_enosys() {
        assert_eq!(fs_chmod(), ENOSYS);
    }
}
