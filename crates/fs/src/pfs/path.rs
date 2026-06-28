//! Path lookup — stub for Pipe File System
//!
//! PFS does not support traditional path-based lookup because pipes are
//! accessed by file descriptor, not by path.  The only way to "open" a
//! pipe is via `fs_newnode` (called from VFS when a pipe is created).

use crate::pfs::consts::*;

/// Path lookup — not supported for pipes.
///
/// Returns ENOSYS because PFS has no directory structure.
// Reference: path.c in other FS implementations
pub fn fs_lookup() -> i32 {
    ENOSYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_lookup_returns_enosys() {
        assert_eq!(fs_lookup(), ENOSYS);
    }
}
