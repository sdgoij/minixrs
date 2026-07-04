//! PFS constants — adapted from `minix/fs/pfs/const.h`

/// Number of slots in the in-core inode table.
pub const PFS_NR_INODES: usize = 512;

pub const INODE_HASH_LOG2: usize = 7;
pub const INODE_HASH_SIZE: usize = 1 << INODE_HASH_LOG2;
pub const INODE_HASH_MASK: usize = INODE_HASH_SIZE - 1;

/// Standard pipe buffer size.
pub const PIPE_BUF: usize = 4096;

/// Number of pipe data buffers in the static pool.
pub const PIPE_NR_BUFS: usize = 64;

/// Returned by alloc_bit() to signal failure.
pub const NO_BIT: u32 = 0;

/// Time update flags.
pub const ATIME: u32 = 0o002;
pub const CTIME: u32 = 0o004;
pub const MTIME: u32 = 0o010;

/// Bitmap chunk calculations (from const.h macros).
pub const FS_BITCHUNK_BITS: usize = core::mem::size_of::<u32>() * 8;

pub const fn fs_bitmap_chunks(nr_inodes: usize) -> usize {
    nr_inodes / core::mem::size_of::<u32>()
}

pub const INODEMAP_CHUNKS: usize = fs_bitmap_chunks(PFS_NR_INODES);

pub const FS_BASE: i32 = 0xA00;
pub const FS_CALL_VEC_SIZE: usize = 33;

pub const REQ_READ: i32 = FS_BASE + 19;
pub const REQ_WRITE: i32 = FS_BASE + 20;

pub const OK: i32 = 0;
pub const EINVAL: i32 = 22;
pub const EPERM: i32 = 1;
pub const ENOSPC: i32 = 28;
pub const EFBIG: i32 = 27;
pub const ENFILE: i32 = 23;
pub const EIO: i32 = 5;
pub const EBUSY: i32 = 16;
pub const ENOSYS: i32 = 78;

pub const I_TYPE: u16 = 0o170000;
pub const I_NAMED_PIPE: u16 = 0o010000;
pub const I_NOT_ALLOC: u16 = 0o000000;
pub const I_CHAR_SPECIAL: u16 = 0o020000;
pub const I_BLOCK_SPECIAL: u16 = 0o060000;
pub const I_REGULAR: u16 = 0o100000;
pub const ALL_MODES: u16 = 0o7777;

pub const S_IFMT: u16 = 0o170000;
pub const S_IFIFO: u16 = 0o010000;
pub const S_IFBLK: u16 = 0o060000;
pub const S_IFCHR: u16 = 0o020000;

pub const NO_DEV: u32 = 0xFFFF;
pub const NO_ENTRY: u32 = 0;
pub const NO_LINK: u16 = 0;

pub const READING: i32 = 0;
pub const WRITING: i32 = 1;

pub const FALSE: i32 = 0;
pub const TRUE: i32 = 1;
pub const S_BLKSIZE: u32 = 512;
