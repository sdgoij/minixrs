//! MFS constants — adapted from `minix/fs/mfs/const.h`

/// # direct zone numbers in a V2 inode.
pub const V2_NR_DZONES: usize = 7;
/// Total # zone numbers in a V2 inode.
pub const V2_NR_TZONES: usize = 10;

/// # slots in "in core" inode table (matches NR_VNODES in VFS).
pub const NR_INODES: usize = 512;

pub const INODE_HASH_LOG2: usize = 7;
pub const INODE_HASH_SIZE: usize = 1 << INODE_HASH_LOG2;
pub const INODE_HASH_MASK: usize = INODE_HASH_SIZE - 1;

/// Max filename length (matches MFS_DIRSIZ).
pub const MFS_NAME_MAX: usize = 60;

pub const SUPER_MAGIC: u16 = 0x137F;
pub const SUPER_REV: u16 = 0x7F13;
pub const SUPER_V2: u16 = 0x2468;
pub const SUPER_V2_REV: u16 = 0x6824;
pub const SUPER_V3: u16 = 0x4D5A;

pub const V2: i32 = 2;
pub const V3: i32 = 3;

/// Super user's uid.
pub const SU_UID: i32 = 0;

/// Returned by alloc_bit() to signal failure.
pub const NO_BIT: u32 = 0;

pub const LOOK_UP: i32 = 0;
pub const ENTER: i32 = 1;
pub const DELETE: i32 = 2;
pub const IS_EMPTY: i32 = 3;

// write_map() args
pub const WMAP_FREE: u32 = 1 << 0;

pub const IGN_PERM: i32 = 0;
pub const CHK_PERM: i32 = 1;

pub const IN_CLEAN: u8 = 0;
pub const IN_DIRTY: u8 = 1;
pub const ATIME: u32 = 0o002;
pub const CTIME: u32 = 0o004;
pub const MTIME: u32 = 0o010;

pub const BYTE_SWAP: i32 = 0;

pub const END_OF_FILE: i32 = -104;

pub const ROOT_INODE: u32 = 1;
pub const BOOT_BLOCK: u32 = 0;
pub const SUPER_BLOCK_BYTES: u32 = 1024;
pub const START_BLOCK: u32 = 2;

/// No device constant.
pub const NO_DEV: u32 = 0xFFFF;

/// Default number of buffer cache blocks.
pub const DEFAULT_NR_BUFS: usize = 512;

pub const NO_SEEK: u8 = 0;
pub const ISEEK: u8 = 1;

/// PATH_MAX for user_path.
pub const PATH_MAX: usize = 1024;

/// Number of FS call vector entries.
pub const NREQS: usize = 34;

/// Invalid UID/GID sentinel.
pub const INVAL_UID: u16 = 0xFFFF;
pub const INVAL_GID: u16 = 0xFFFF;

pub const OK: i32 = 0;
pub const EINVAL: i32 = 22;
pub const EPERM: i32 = 1;
pub const ENOSPC: i32 = 28;
pub const EROFS: i32 = 30;
pub const ENOENT: i32 = 2;
pub const EFBIG: i32 = 27;
pub const EMLINK: i32 = 31;
pub const EACCES: i32 = 13;
pub const ENOTDIR: i32 = 20;
pub const EISDIR: i32 = 21;
pub const EBUSY: i32 = 16;
pub const EEXIST: i32 = 17;
pub const ENFILE: i32 = 23;
pub const E2BIG: i32 = 7;
pub const EIO: i32 = 5;
pub const ELOOP: i32 = 40;
pub const ENAMETOOLONG: i32 = 36;
pub const ENOTEMPTY: i32 = 39;
pub const EXDEV: i32 = 18;
pub const ESYMLINK: i32 = 100;
pub const EENTERMOUNT: i32 = 101;
pub const ELEAVEMOUNT: i32 = 102;

pub const NO_ZONE: u32 = 0xFFFFFFFF;
pub const NO_BLOCK: u32 = 0xFFFFFFFF;
pub const NO_ENTRY: u32 = 0;
pub const NO_LINK: u16 = 0;
pub const LINK_MAX: u16 = 32767;
pub const NORMAL: i32 = 0;
pub const NO_READ: i32 = 1;
pub const PREFETCH: i32 = 2;
pub const READING: i32 = 0;
pub const WRITING: i32 = 1;
pub const PEEKING: i32 = 2;
pub const MAP_BLOCK: i32 = 0;
pub const DIRECTORY_BLOCK: i32 = 1;
pub const INDIRECT_BLOCK: i32 = 2;
pub const INODE_BLOCK: i32 = 3;
pub const FULL_DATA_BLOCK: i32 = 4;
pub const PARTIAL_DATA_BLOCK: i32 = 5;
pub const NR_IOREQS: u32 = 32;

// Mode / permission bits
pub const I_TYPE: u16 = 0o170000;
pub const I_REGULAR: u16 = 0o100000;
pub const I_DIRECTORY: u16 = 0o040000;
pub const I_BLOCK_SPECIAL: u16 = 0o060000;
pub const I_CHAR_SPECIAL: u16 = 0o020000;
pub const I_NAMED_PIPE: u16 = 0o010000;
pub const I_SYMBOLIC_LINK: u16 = 0o120000;
pub const I_NOT_ALLOC: u16 = 0o000000;
pub const I_SET_UID_BIT: u16 = 0o4000;
pub const I_SET_GID_BIT: u16 = 0o2000;
pub const ALL_MODES: u16 = 0o7777;
pub const R_BIT: u16 = 4;
pub const W_BIT: u16 = 2;
pub const X_BIT: u16 = 1;
pub const RWX_MODES: u16 = 0o0777;

// VFS request types (FS_BASE + n)
pub const FS_BASE: i32 = 0xA00;
pub const REQ_READ: i32 = FS_BASE + 19;
pub const REQ_WRITE: i32 = FS_BASE + 20;
pub const REQ_PEEK: i32 = FS_BASE + 32;
pub const REQ_BREAD: i32 = FS_BASE + 11;
pub const REQ_BWRITE: i32 = FS_BASE + 12;
pub const REQ_UNLINK: i32 = FS_BASE + 13;
pub const REQ_RMDIR: i32 = FS_BASE + 14;

pub const UTIME_NOW: i64 = -1;
pub const UTIME_OMIT: i64 = -2;

pub const MAX_FILE_POS: i64 = 0x7FFFFFFF; // LONG_MAX

pub const PATH_GET_UCRED: i32 = 0x01;
pub const PATH_RET_SYMLINK: i32 = 0x02;
pub const REQ_RDONLY: i32 = 0x01;
pub const REQ_ISROOT: i32 = 0x02;
pub const RES_HASPEEK: i32 = 0x01;
pub const _POSIX_SYMLOOP_MAX: u32 = 8;

pub const FALSE: i32 = 0;
pub const TRUE: i32 = 1;

pub const DOT1: [u8; 2] = [b'.', 0];
pub const DOT2: [u8; 3] = [b'.', b'.', 0];
pub const UMAX_FILE_POS: u64 = 0x7FFFFFFF;
