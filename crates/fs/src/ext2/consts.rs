//! Ext2 constants — adapted from `minix/fs/ext2/const.h`

/// # slots in "in core" inode table (matches NR_VNODES in VFS).
pub const NR_INODES: usize = 512;
pub const GETDENTS_BUFSIZ: usize = 257;

pub const INODE_HASH_LOG2: usize = 7;
pub const INODE_HASH_SIZE: usize = 1 << INODE_HASH_LOG2;
pub const INODE_HASH_MASK: usize = INODE_HASH_SIZE - 1;

pub const SUPER_MAGIC: u16 = 0xEF53;

pub const EXT2_NAME_MAX: usize = 255;

/// Super user's uid.
pub const SU_UID: u32 = 0;

/// Returned by alloc_bit() to signal failure.
pub const NO_BIT: u32 = 0;

pub const NORMAL: i32 = 0;
pub const NO_READ: i32 = 1;
pub const PREFETCH: i32 = 2;

pub const LOOK_UP: i32 = 0;
pub const ENTER: i32 = 1;
pub const DELETE: i32 = 2;
pub const IS_EMPTY: i32 = 3;

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

pub const SUPER_BLOCK_BYTES: u32 = 1024;

pub const ROOT_INODE: u32 = 2;
pub const BOOT_BLOCK: u32 = 0;
pub const START_BLOCK: u32 = 2;
pub const BLOCK_ADDRESS_BYTES: u32 = 4;

pub const SUPER_SIZE_D: usize = 1024;

pub const DIR_ENTRY_ALIGN: u32 = 4;
pub const MIN_DIR_ENTRY_SIZE: usize = 8;

pub const EXT2_NDIR_BLOCKS: usize = 12;
pub const EXT2_IND_BLOCK: usize = EXT2_NDIR_BLOCKS;
pub const EXT2_DIND_BLOCK: usize = EXT2_IND_BLOCK + 1;
pub const EXT2_TIND_BLOCK: usize = EXT2_DIND_BLOCK + 1;
pub const EXT2_N_BLOCKS: usize = EXT2_TIND_BLOCK + 1;

pub const EXT2_GOOD_OLD_INODE_SIZE: u32 = 128;
pub const EXT2_GOOD_OLD_FIRST_INO: u32 = 11;

pub const MAX_FAST_SYMLINK_LENGTH: usize = core::mem::size_of::<u32>() * EXT2_N_BLOCKS;

/// FS states
pub const EXT2_VALID_FS: u16 = 0x0001;
pub const EXT2_ERROR_FS: u16 = 0x0002;

pub const EXT2_GOOD_OLD_REV: u32 = 0;
pub const EXT2_DYNAMIC_REV: u32 = 1;

/// Feature flags
pub const COMPAT_DIR_PREALLOC: u32 = 0x0001;
pub const COMPAT_IMAGIC_INODES: u32 = 0x0002;
pub const COMPAT_HAS_JOURNAL: u32 = 0x0004;
pub const COMPAT_EXT_ATTR: u32 = 0x0008;
pub const COMPAT_RESIZE_INO: u32 = 0x0010;
pub const COMPAT_DIR_INDEX: u32 = 0x0020;
pub const COMPAT_ANY: u32 = 0xffffffff;

pub const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
pub const RO_COMPAT_LARGE_FILE: u32 = 0x0002;
pub const RO_COMPAT_BTREE_DIR: u32 = 0x0004;
pub const RO_COMPAT_ANY: u32 = 0xffffffff;

pub const INCOMPAT_COMPRESSION: u32 = 0x0001;
pub const INCOMPAT_FILETYPE: u32 = 0x0002;
pub const INCOMPAT_RECOVER: u32 = 0x0004;
pub const INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
pub const INCOMPAT_META_BG: u32 = 0x0010;
pub const INCOMPAT_ANY: u32 = 0xffffffff;

/// Supported features
pub const SUPPORTED_INCOMPAT_FEATURES: u32 = INCOMPAT_FILETYPE;
pub const SUPPORTED_RO_COMPAT_FEATURES: u32 = RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE;

/// Ext2 file types (low 3 bits only)
pub const EXT2_FT_UNKNOWN: u8 = 0;
pub const EXT2_FT_REG_FILE: u8 = 1;
pub const EXT2_FT_DIR: u8 = 2;
pub const EXT2_FT_CHRDEV: u8 = 3;
pub const EXT2_FT_BLKDEV: u8 = 4;
pub const EXT2_FT_FIFO: u8 = 5;
pub const EXT2_FT_SOCK: u8 = 6;
pub const EXT2_FT_SYMLINK: u8 = 7;
pub const EXT2_FT_MAX: u8 = 8;

/// Inode flags
pub const EXT2_INDEX_FL: u32 = 0x00001000;
pub const EXT2_TOPDIR_FL: u32 = 0x00020000;
pub const EXT2_PREALLOC_BLOCKS: usize = 8;

/// Hash feature test macros
pub const fn has_compat_feature(sp: &super::types::SuperBlock, mask: u32) -> bool {
    (sp.s_feature_compat & mask) != 0
}

pub const fn has_ro_compat_feature(sp: &super::types::SuperBlock, mask: u32) -> bool {
    (sp.s_feature_ro_compat & mask) != 0
}

pub const fn has_incompat_feature(sp: &super::types::SuperBlock, mask: u32) -> bool {
    (sp.s_feature_incompat & mask) != 0
}

pub const OK: i32 = 0;
pub const EINVAL: i32 = -22;
pub const EPERM: i32 = -1;
pub const ENOSPC: i32 = -28;
pub const EROFS: i32 = -30;
pub const ENOENT: i32 = -2;
pub const EFBIG: i32 = -27;
pub const EMLINK: i32 = -31;
pub const EACCES: i32 = -13;
pub const ENOTDIR: i32 = -20;
pub const EISDIR: i32 = -21;
pub const EBUSY: i32 = -16;
pub const EEXIST: i32 = -17;
pub const ENFILE: i32 = -23;
pub const E2BIG: i32 = -7;
pub const EIO: i32 = -5;
pub const ELOOP: i32 = -40;
pub const ENAMETOOLONG: i32 = -36;
pub const ENOTEMPTY: i32 = -39;
pub const EXDEV: i32 = -18;
pub const ESYMLINK: i32 = -100;
pub const EENTERMOUNT: i32 = -101;
pub const ELEAVEMOUNT: i32 = -102;

pub const NO_DEV: u32 = 0xFFFF;
pub const NO_BLOCK: u32 = 0xFFFFFFFF;
pub const NO_ENTRY: u32 = 0;
pub const NO_LINK: u16 = 0;
pub const LINK_MAX: u16 = 32767;

pub const NO_SEEK: u8 = 0;
pub const ISEEK: u8 = 1;

pub const PATH_MAX: usize = 1024;
pub const NREQS: usize = 34;

pub const INVAL_UID: u16 = 0xFFFF;
pub const INVAL_GID: u16 = 0xFFFF;

pub const READING: i32 = 0;
pub const WRITING: i32 = 1;
pub const PEEKING: i32 = 2;

pub const MAP_BLOCK: i32 = 0;
pub const DIRECTORY_BLOCK: i32 = 1;
pub const INDIRECT_BLOCK: i32 = 2;
pub const INODE_BLOCK: i32 = 3;
pub const FULL_DATA_BLOCK: i32 = 4;
pub const PARTIAL_DATA_BLOCK: i32 = 5;

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

// VFS request numbers
pub const FS_BASE: i32 = 200;
pub const REQ_READ: i32 = FS_BASE + 19;
pub const REQ_WRITE: i32 = FS_BASE + 20;
pub const REQ_PEEK: i32 = FS_BASE + 32;
pub const REQ_UNLINK: i32 = FS_BASE + 13;
pub const REQ_RMDIR: i32 = FS_BASE + 14;

pub const UTIME_NOW: i64 = -1;
pub const UTIME_OMIT: i64 = -2;

pub const TRUE: i32 = 1;
pub const FALSE: i32 = 0;

pub const DOT1: [u8; 2] = [b'.', 0];
pub const DOT2: [u8; 3] = [b'.', b'.', 0];
