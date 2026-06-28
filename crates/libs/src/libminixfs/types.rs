//! Core types for the libminixfs block cache.

use crate::libminixfs::constants::*;

/// A buffer in the block cache.
///
/// Each `Buf` holds one block of data read from (or to be written to) a device.
/// Buffers are linked together in both a hash chain (for fast lookup by
/// `(dev, blocknr)`) and an LRU chain (for eviction ordering).
#[derive(Debug)]
#[repr(C)]
pub struct Buf {
    /// Allocated data buffer (raw bytes).
    pub data_ptr: *mut u8,
    /// Number of bytes of data in the buffer.
    pub lmfs_bytes: u32,
    /// Block number on device.
    pub lmfs_blocknr: u64,
    /// Device number.
    pub lmfs_dev: u32,
    /// Reference count (0 = on LRU free list).
    pub lmfs_count: i32,
    /// Flags (see `VMMC_*` constants).
    pub lmfs_flags: u32,
    /// Inode number (for VM secondary cache).
    pub lmfs_inode: u64,
    /// Offset within inode (for VM secondary cache).
    pub lmfs_inode_offset: u64,
    /// Flag indicating a `vm_set_cacheblock` call is needed.
    pub lmfs_needsetcache: i32,
    /// Next buffer on hash chain (collision chain).
    pub lmfs_hash: *mut Buf,
    /// Previous buffer on LRU chain.
    pub lmfs_prev: *mut Buf,
    /// Next buffer on LRU chain.
    pub lmfs_next: *mut Buf,
}

/// Null pointer constant for `Buf`.
pub const NO_BUF: *mut Buf = core::ptr::null_mut();

impl Buf {
    /// Create a zero-initialised buffer.
    pub const fn zeroed() -> Self {
        Buf {
            data_ptr: core::ptr::null_mut(),
            lmfs_bytes: 0,
            lmfs_blocknr: 0,
            lmfs_dev: NO_DEV,
            lmfs_count: 0,
            lmfs_flags: 0,
            lmfs_inode: VMC_NO_INODE,
            lmfs_inode_offset: 0,
            lmfs_needsetcache: 0,
            lmfs_hash: core::ptr::null_mut(),
            lmfs_prev: core::ptr::null_mut(),
            lmfs_next: core::ptr::null_mut(),
        }
    }

    /// Check whether this buffer is clean (not dirty).
    pub fn is_clean(&self) -> bool {
        self.lmfs_flags & VMMC_DIRTY == 0
    }

    /// Check whether this buffer is locked (in use).
    pub fn is_locked(&self) -> bool {
        self.lmfs_flags & VMMC_BLOCK_LOCKED != 0
    }
}
