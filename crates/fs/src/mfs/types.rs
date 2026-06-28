//! MFS core types — adapted from `minix/fs/mfs/type.h`, `mfsdir.h`, `inode.h`, `super.h`

use crate::mfs::consts::*;

/// V2.x disk inode (on-disk format).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct D2Inode {
    pub d2_mode: u16,
    pub d2_nlinks: u16,
    pub d2_uid: i16,
    pub d2_gid: u16,
    pub d2_size: i32,
    pub d2_atime: i32,
    pub d2_mtime: i32,
    pub d2_ctime: i32,
    pub d2_zone: [u32; V2_NR_TZONES],
}

pub const V2_INODE_SIZE: usize = core::mem::size_of::<D2Inode>();

/// On-disk directory entry.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Direct {
    pub mfs_d_ino: u32,
    pub mfs_d_name: [u8; MFS_NAME_MAX],
}

pub const DIR_ENTRY_SIZE: usize = core::mem::size_of::<Direct>();
pub const MFS_DIRSIZ: usize = MFS_NAME_MAX;

/// Super block (in-memory + on-disk).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct SuperBlock {
    // On-disk fields (V2/V3)
    pub s_ninodes: u32,
    pub s_nzones: u32, // zone1_t on disk
    pub s_imap_blocks: i16,
    pub s_zmap_blocks: i16,
    pub s_firstdatazone_old: u32, // zone1_t
    pub s_log_zone_size: i16,
    pub s_flags: u16,
    pub s_max_size: i32,
    pub s_zones: u32, // zone_t (V2+)
    pub s_magic: i16,

    // V3+ on-disk fields
    pub s_pad2: i16,
    pub s_block_size: u16,
    pub s_disk_version: u8,

    // In-memory only fields
    pub s_inodes_per_block: u32,
    pub s_firstdatazone: u32, // zone_t (big)
    pub s_dev: u32,           // dev_t
    pub s_rd_only: i32,
    pub s_native: i32,
    pub s_version: i32,
    pub s_ndzones: i32,
    pub s_nindirs: i32,
    pub s_isearch: u32, // bit_t
    pub s_zsearch: u32, // bit_t
    pub s_is_root: u8,
}

impl Default for SuperBlock {
    fn default() -> Self {
        Self {
            s_ninodes: 0,
            s_nzones: 0,
            s_imap_blocks: 0,
            s_zmap_blocks: 0,
            s_firstdatazone_old: 0,
            s_log_zone_size: 0,
            s_flags: 0,
            s_max_size: 0,
            s_zones: 0,
            s_magic: 0,
            s_pad2: 0,
            s_block_size: 0,
            s_disk_version: 0,
            s_inodes_per_block: 0,
            s_firstdatazone: 0,
            s_dev: NO_DEV,
            s_rd_only: 0,
            s_native: 0,
            s_version: 0,
            s_ndzones: 0,
            s_nindirs: 0,
            s_isearch: 0,
            s_zsearch: 0,
            s_is_root: 0,
        }
    }
}

pub const SUPER_SIZE: usize = core::mem::size_of::<SuperBlock>();

/// In-memory inode cache entry.
#[derive(Debug)]
pub struct Inode {
    // On-disk fields
    pub i_mode: u16,
    pub i_nlinks: u16,
    pub i_uid: u16,
    pub i_gid: u16,
    pub i_size: i32,
    pub i_atime: u32,
    pub i_mtime: u32,
    pub i_ctime: u32,
    pub i_zone: [u32; V2_NR_TZONES],

    // In-memory only
    pub i_dev: u32,
    pub i_num: u32,
    pub i_count: i32,
    pub i_ndzones: u32,
    pub i_nindirs: u32,
    pub i_sp: Option<&'static mut SuperBlock>,
    pub i_dirt: u8,
    pub i_zsearch: u32,
    pub i_last_dpos: i64,
    pub i_mountpoint: i32,
    pub i_seek: u8,
    pub i_update: u32,
    pub i_hash_next: Option<u16>, // index into inode table for hash chain
    pub i_hash_prev: Option<u16>, // prev index for hash chain
    pub i_unused_next: Option<u16>, // index for free list
    pub i_unused_prev: Option<u16>, // prev index for free list
}

// Manual Clone: skip the `&'static mut SuperBlock` field since &mut is not Clone.
impl Clone for Inode {
    fn clone(&self) -> Self {
        Inode {
            i_mode: self.i_mode,
            i_nlinks: self.i_nlinks,
            i_uid: self.i_uid,
            i_gid: self.i_gid,
            i_size: self.i_size,
            i_atime: self.i_atime,
            i_mtime: self.i_mtime,
            i_ctime: self.i_ctime,
            i_zone: self.i_zone,
            i_dev: self.i_dev,
            i_num: self.i_num,
            i_count: self.i_count,
            i_ndzones: self.i_ndzones,
            i_nindirs: self.i_nindirs,
            i_sp: None, // don't clone the mutable reference
            i_dirt: self.i_dirt,
            i_zsearch: self.i_zsearch,
            i_last_dpos: self.i_last_dpos,
            i_mountpoint: self.i_mountpoint,
            i_seek: self.i_seek,
            i_update: self.i_update,
            i_hash_next: self.i_hash_next,
            i_hash_prev: self.i_hash_prev,
            i_unused_next: self.i_unused_next,
            i_unused_prev: self.i_unused_prev,
        }
    }
}

impl Inode {
    /// Mark inode as clean.
    pub fn mark_clean(&mut self) {
        self.i_dirt = IN_CLEAN;
    }

    /// Mark inode as dirty (only if FS is not read-only).
    pub fn mark_dirty(&mut self) {
        if let Some(ref sp) = self.i_sp {
            if sp.s_rd_only != 0 {
                // Would print warning in C code
                return;
            }
        }
        self.i_dirt = IN_DIRTY;
    }

    pub fn is_clean(&self) -> bool {
        self.i_dirt == IN_CLEAN
    }

    pub fn is_dirty(&self) -> bool {
        self.i_dirt == IN_DIRTY
    }
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
            i_zone: [0; V2_NR_TZONES],
            i_dev: NO_DEV,
            i_num: 0,
            i_count: 0,
            i_ndzones: V2_NR_DZONES as u32,
            i_nindirs: 0,
            i_sp: None,
            i_dirt: IN_CLEAN,
            i_zsearch: 0,
            i_last_dpos: 0,
            i_mountpoint: FALSE as i32,
            i_seek: NO_SEEK,
            i_update: 0,
            i_hash_next: None,
            i_hash_prev: None,
            i_unused_next: None,
            i_unused_prev: None,
        }
    }
}

// Bitmap types
pub type BitT = u32;
pub type BitchunkT = u32;
pub const FS_BITCHUNK_BITS: usize = core::mem::size_of::<BitchunkT>() * 8;

/// Block number / zone number type.
pub type BlockT = u32;
pub type ZoneT = u32;

/// File system bitmap operations (inode map = 0, zone map = 1).
pub const IMAP: i32 = 0;
pub const ZMAP: i32 = 1;

/// Super block flags.
pub const MFSFLAG_CLEAN: u16 = 1 << 0;
pub const MFSFLAG_MANDATORY_MASK: u16 = 0xFF00;

/// Derived sizes.
pub fn v2_indirects(block_size: usize) -> usize {
    block_size / core::mem::size_of::<ZoneT>()
}

pub fn v2_inodes_per_block(block_size: usize) -> usize {
    block_size / V2_INODE_SIZE
}

pub fn nr_dir_entries(block_size: usize) -> usize {
    block_size / DIR_ENTRY_SIZE
}

pub fn fs_bitmap_chunks(block_size: usize) -> usize {
    block_size / core::mem::size_of::<BitchunkT>()
}

pub fn fs_bits_per_block(block_size: usize) -> usize {
    fs_bitmap_chunks(block_size) * FS_BITCHUNK_BITS
}
