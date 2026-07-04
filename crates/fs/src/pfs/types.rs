//! PFS core types — adapted from `minix/fs/pfs/inode.h` and `buf.h`

use crate::pfs::consts::*;

/// In-memory inode for the Pipe File System.
///
/// PFS inodes have no on-disk representation — everything lives in memory.
/// Linked lists use `Option<u16>` indices into the global inode table.
#[derive(Debug, Clone)]
pub struct Inode {
    pub i_mode: u16,
    pub i_nlinks: u16,
    pub i_uid: u16,
    pub i_gid: u16,
    pub i_size: i64,
    pub i_atime: i64,
    pub i_mtime: i64,
    pub i_ctime: i64,
    pub i_dev: u32,
    pub i_rdev: u32,
    pub i_num: u32,
    pub i_count: i32,
    pub i_update: u8,
    /// Next inode in hash chain (index into inode_table).
    pub i_hash_next: Option<u16>,
    /// Next inode in unused/free list (index into inode_table).
    pub i_unused_next: Option<u16>,
}

impl Default for Inode {
    fn default() -> Self {
        Self {
            i_mode: 0,
            i_nlinks: 0,
            i_uid: 0,
            i_gid: 0,
            i_size: 0,
            i_atime: 0,
            i_mtime: 0,
            i_ctime: 0,
            i_dev: NO_DEV,
            i_rdev: NO_DEV,
            i_num: NO_ENTRY,
            i_count: 0,
            i_update: 0,
            i_hash_next: None,
            i_unused_next: None,
        }
    }
}

/// Pipe data buffer — from `minix/fs/pfs/buf.h`
#[derive(Debug, Clone)]
pub struct Buf {
    /// Pipe data storage.
    pub b_data: [u8; PIPE_BUF],
    /// Next buffer in free list (index into buf_pool).
    pub b_next: Option<u16>,
    /// Previous buffer in free list (index into buf_pool).
    pub b_prev: Option<u16>,
    /// Inode number on minor device.
    pub b_num: u32,
    /// Device where buffer belongs.
    pub b_dev: u32,
    /// Number of bytes allocated in bp.
    pub b_bytes: i32,
    /// Number of users of this buffer.
    pub b_count: i32,
}

impl Default for Buf {
    fn default() -> Self {
        Self {
            b_data: [0; PIPE_BUF],
            b_next: None,
            b_prev: None,
            b_num: 0,
            b_dev: NO_DEV,
            b_bytes: 0,
            b_count: 0,
        }
    }
}

pub type BitchunkT = u32;
pub type BitT = u32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inode_default() {
        let ino = Inode::default();
        assert_eq!(ino.i_count, 0);
        assert_eq!(ino.i_dev, NO_DEV);
        assert_eq!(ino.i_num, NO_ENTRY);
        assert!(ino.i_hash_next.is_none());
        assert!(ino.i_unused_next.is_none());
    }

    #[test]
    fn test_buf_default() {
        let buf = Buf::default();
        assert_eq!(buf.b_dev, NO_DEV);
        assert_eq!(buf.b_count, 0);
        assert_eq!(buf.b_bytes, 0);
        assert!(buf.b_next.is_none());
        assert!(buf.b_prev.is_none());
    }

    #[test]
    fn test_sizeof_buf() {
        // Just verify it compiles and has sensible size
        assert!(core::mem::size_of::<Buf>() >= PIPE_BUF);
    }

    #[test]
    fn test_sizeof_inode() {
        // Verify struct size is reasonable
        assert!(core::mem::size_of::<Inode>() > 0);
    }
}
