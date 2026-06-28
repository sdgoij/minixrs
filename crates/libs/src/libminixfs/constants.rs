//! Constants for the libminixfs block cache.

/// Maximum filename length.
pub const LMFS_MAXNAME: usize = 60;

/// Label maximum length.
pub const LABEL_MAX: usize = 16;

/// Path maximum length.
pub const PATH_MAX: usize = 255;

// Block flags (VMMC_* — VM cache / block metadata flags).

/// Block is locked (in use, not on LRU list).
pub const VMMC_BLOCK_LOCKED: u32 = 0x01;

/// Block has been modified (needs to be written back).
pub const VMMC_DIRTY: u32 = 0x02;

/// Block was evicted by the VM (contents no longer valid).
pub const VMMC_EVICTED: u32 = 0x04;

/// Block's VM cache association needs to be updated.
pub const VMMC_NEEDSETCACHE: u32 = 0x08;

/// Special value for "no inode" in VM cache operations.
pub const VMC_NO_INODE: u64 = 0;

/// Special value for "no device".
pub const NO_DEV: u32 = u32::MAX;

/// Special value for "no block number".
pub const NO_BLOCK: u64 = 0;

/// VM page size.
pub const PAGE_SIZE: u32 = 4096;

/// File system block size (used in I/O).
pub const VM_BLOCK_SIZE: u32 = 4096;

/// `only_search` parameter: perform normal (read) I/O.
pub const NORMAL: i32 = 0;

/// `only_search` parameter: do not read from disk (block will be overwritten).
pub const NO_READ: i32 = 1;

/// `only_search` parameter: prefetch only; no I/O, mark dev as NO_DEV.
pub const PREFETCH: i32 = 2;

/// Block type constants for `lmfs_put_block`.
pub const FULL_DATA_BLOCK: i32 = 0;
pub const PARTIAL_DATA_BLOCK: i32 = 1;
pub const DIRECTORY_BLOCK: i32 = 2;
pub const INODE_BLOCK: i32 = 3;
pub const ONE_SHOT: i32 = 4;
