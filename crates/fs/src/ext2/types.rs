//! Ext2 core types — adapted from `minix/fs/ext2/type.h`, `inode.h`, `super.h`

use crate::ext2::consts::*;

/// On-disk ext2 inode (little-endian on disk).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DInode {
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size: u32,
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks: u32,
    pub i_flags: u32,
    pub osd1: [u32; 1], // actually a union, but we treat as u32
    pub i_block: [u32; EXT2_N_BLOCKS],
    pub i_generation: u32,
    pub i_file_acl: u32,
    pub i_dir_acl: u32,
    pub i_faddr: u32,
    pub osd2: [u32; 2], // actually a union, but we treat as [u32; 2]
}

pub const EXT2_INODE_SIZE: usize = core::mem::size_of::<DInode>();

/// On-disk directory entry.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ext2DiskDirDesc {
    pub d_ino: u32,
    pub d_rec_len: u16,
    pub d_name_len: u8,
    pub d_file_type: u8,
    pub d_name: [u8; 1],
}

/// Super block (in-memory + on-disk).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct SuperBlock {
    // On-disk fields (as per ext2_fs.h)
    pub s_inodes_count: u32,
    pub s_blocks_count: u32,
    pub s_r_blocks_count: u32,
    pub s_free_blocks_count: u32,
    pub s_free_inodes_count: u32,
    pub s_first_data_block: u32,
    pub s_log_block_size: u32,
    pub s_log_frag_size: u32,
    pub s_blocks_per_group: u32,
    pub s_frags_per_group: u32,
    pub s_inodes_per_group: u32,
    pub s_mtime: u32,
    pub s_wtime: u32,
    pub s_mnt_count: u16,
    pub s_max_mnt_count: u16,
    pub s_magic: u16,
    pub s_state: u16,
    pub s_errors: u16,
    pub s_minor_rev_level: u16,
    pub s_lastcheck: u32,
    pub s_checkinterval: u32,
    pub s_creator_os: u32,
    pub s_rev_level: u32,
    pub s_def_resuid: u16,
    pub s_def_resgid: u16,
    // Dynamic rev fields
    pub s_first_ino: u32,
    pub s_inode_size: u16,
    pub s_block_group_nr: u16,
    pub s_feature_compat: u32,
    pub s_feature_incompat: u32,
    pub s_feature_ro_compat: u32,
    pub s_uuid: [u8; 16],
    pub s_volume_name: [i8; 16],
    pub s_last_mounted: [i8; 64],
    pub s_algorithm_usage_bitmap: u32,
    pub s_prealloc_blocks: u8,
    pub s_prealloc_dir_blocks: u8,
    pub s_padding1: u16,
    pub s_journal_uuid: [u8; 16],
    pub s_journal_inum: u32,
    pub s_journal_dev: u32,
    pub s_last_orphan: u32,
    pub s_hash_seed: [u32; 4],
    pub s_def_hash_version: u8,
    pub s_reserved_char_pad: u8,
    pub s_reserved_word_pad: u16,
    pub s_default_mount_opts: u32,
    pub s_first_meta_bg: u32,
    pub s_reserved: [u32; 190],

    // In-memory only fields
    pub s_inodes_per_block: u32,
    pub s_itb_per_group: u32,
    pub s_gdb_count: u32,
    pub s_desc_per_block: u32,
    pub s_groups_count: u32,
    pub s_blocksize_bits: u8,
    pub s_block_size: u16,
    pub s_sectors_in_block: u16,
    pub s_max_size: u64,
    pub s_dev: u32,
    pub s_rd_only: i32,
    pub s_bsearch: u32,
    pub s_igsearch: i32,
    pub s_is_root: u8,
    pub s_dirs_counter: u32,
    /// Cached group descriptor table buffer pointers (as *mut Buf cast to usize).
    pub s_gdt_bufs: [usize; 4],
    /// Cached group descriptor table data pointers (as *mut u8 cast to usize).
    pub s_gdt_data: [usize; 4],
}

impl Default for SuperBlock {
    fn default() -> Self {
        Self {
            s_inodes_count: 0,
            s_blocks_count: 0,
            s_r_blocks_count: 0,
            s_free_blocks_count: 0,
            s_free_inodes_count: 0,
            s_first_data_block: 0,
            s_log_block_size: 0,
            s_log_frag_size: 0,
            s_blocks_per_group: 0,
            s_frags_per_group: 0,
            s_inodes_per_group: 0,
            s_mtime: 0,
            s_wtime: 0,
            s_mnt_count: 0,
            s_max_mnt_count: 0,
            s_magic: 0,
            s_state: 0,
            s_errors: 0,
            s_minor_rev_level: 0,
            s_lastcheck: 0,
            s_checkinterval: 0,
            s_creator_os: 0,
            s_rev_level: 0,
            s_def_resuid: 0,
            s_def_resgid: 0,
            s_first_ino: 0,
            s_inode_size: 0,
            s_block_group_nr: 0,
            s_feature_compat: 0,
            s_feature_incompat: 0,
            s_feature_ro_compat: 0,
            s_uuid: [0; 16],
            s_volume_name: [0; 16],
            s_last_mounted: [0; 64],
            s_algorithm_usage_bitmap: 0,
            s_prealloc_blocks: 0,
            s_prealloc_dir_blocks: 0,
            s_padding1: 0,
            s_journal_uuid: [0; 16],
            s_journal_inum: 0,
            s_journal_dev: 0,
            s_last_orphan: 0,
            s_hash_seed: [0; 4],
            s_def_hash_version: 0,
            s_reserved_char_pad: 0,
            s_reserved_word_pad: 0,
            s_default_mount_opts: 0,
            s_first_meta_bg: 0,
            s_reserved: [0; 190],
            s_inodes_per_block: 0,
            s_itb_per_group: 0,
            s_gdb_count: 0,
            s_desc_per_block: 0,
            s_groups_count: 0,
            s_blocksize_bits: 0,
            s_block_size: 0,
            s_sectors_in_block: 0,
            s_max_size: 0,
            s_dev: NO_DEV,
            s_rd_only: 0,
            s_bsearch: 0,
            s_igsearch: 0,
            s_is_root: 0,
            s_dirs_counter: 0,
            s_gdt_bufs: [0; 4],
            s_gdt_data: [0; 4],
        }
    }
}

/// Group descriptor (on-disk + in-memory).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GroupDesc {
    pub block_bitmap: u32,
    pub inode_bitmap: u32,
    pub inode_table: u32,
    pub free_blocks_count: u16,
    pub free_inodes_count: u16,
    pub used_dirs_count: u16,
    pub pad: u16,
    pub reserved: [u32; 3],
}

/// In-memory inode cache entry.
#[derive(Debug)]
pub struct Inode {
    // On-disk fields
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size: u32,
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks: u32,
    pub i_flags: u32,
    pub osd1: [u32; 1],
    pub i_block: [u32; EXT2_N_BLOCKS],
    pub i_generation: u32,
    pub i_file_acl: u32,
    pub i_dir_acl: u32,
    pub i_faddr: u32,
    pub osd2: [u32; 2],

    // In-memory only
    pub i_dev: u32,
    pub i_num: u32,
    pub i_count: i32,
    pub i_sp: Option<&'static mut SuperBlock>,
    pub i_dirt: u8,
    pub i_bsearch: u32,
    pub i_last_pos_bl_alloc: u64,
    pub i_last_dpos: u64,
    pub i_last_dentry_size: i32,
    pub i_mountpoint: i32,
    pub i_seek: u8,
    pub i_update: u32,
    pub i_prealloc_blocks: [u32; EXT2_PREALLOC_BLOCKS],
    pub i_prealloc_count: i32,
    pub i_prealloc_index: i32,
    pub i_preallocation: i32,
    pub i_hash_next: Option<u16>,
    pub i_hash_prev: Option<u16>,
    pub i_unused_next: Option<u16>,
    pub i_unused_prev: Option<u16>,
}

impl Clone for Inode {
    fn clone(&self) -> Self {
        Inode {
            i_mode: self.i_mode,
            i_uid: self.i_uid,
            i_size: self.i_size,
            i_atime: self.i_atime,
            i_ctime: self.i_ctime,
            i_mtime: self.i_mtime,
            i_dtime: self.i_dtime,
            i_gid: self.i_gid,
            i_links_count: self.i_links_count,
            i_blocks: self.i_blocks,
            i_flags: self.i_flags,
            osd1: self.osd1,
            i_block: self.i_block,
            i_generation: self.i_generation,
            i_file_acl: self.i_file_acl,
            i_dir_acl: self.i_dir_acl,
            i_faddr: self.i_faddr,
            osd2: self.osd2,
            i_dev: self.i_dev,
            i_num: self.i_num,
            i_count: self.i_count,
            i_sp: None,
            i_dirt: self.i_dirt,
            i_bsearch: self.i_bsearch,
            i_last_pos_bl_alloc: self.i_last_pos_bl_alloc,
            i_last_dpos: self.i_last_dpos,
            i_last_dentry_size: self.i_last_dentry_size,
            i_mountpoint: self.i_mountpoint,
            i_seek: self.i_seek,
            i_update: self.i_update,
            i_prealloc_blocks: self.i_prealloc_blocks,
            i_prealloc_count: self.i_prealloc_count,
            i_prealloc_index: self.i_prealloc_index,
            i_preallocation: self.i_preallocation,
            i_hash_next: self.i_hash_next,
            i_hash_prev: self.i_hash_prev,
            i_unused_next: self.i_unused_next,
            i_unused_prev: self.i_unused_prev,
        }
    }
}

impl Inode {
    pub fn mark_clean(&mut self) {
        self.i_dirt = IN_CLEAN;
    }

    pub fn mark_dirty(&mut self) {
        if let Some(ref sp) = self.i_sp {
            if sp.s_rd_only != 0 {
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
            i_uid: 0,
            i_size: 0,
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: 0,
            i_blocks: 0,
            i_flags: 0,
            osd1: [0; 1],
            i_block: [NO_BLOCK; EXT2_N_BLOCKS],
            i_generation: 0,
            i_file_acl: 0,
            i_dir_acl: 0,
            i_faddr: 0,
            osd2: [0; 2],
            i_dev: NO_DEV,
            i_num: 0,
            i_count: 0,
            i_sp: None,
            i_dirt: IN_CLEAN,
            i_bsearch: 0,
            i_last_pos_bl_alloc: 0,
            i_last_dpos: 0,
            i_last_dentry_size: 0,
            i_mountpoint: FALSE as i32,
            i_seek: NO_SEEK,
            i_update: 0,
            i_prealloc_blocks: [NO_BLOCK; EXT2_PREALLOC_BLOCKS],
            i_prealloc_count: 0,
            i_prealloc_index: 0,
            i_preallocation: 0,
            i_hash_next: None,
            i_hash_prev: None,
            i_unused_next: None,
            i_unused_prev: None,
        }
    }
}

/// Options struct.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Opt {
    pub use_orlov: i32,
    pub mfsalloc: i32,
    pub use_reserved_blocks: i32,
    pub block_with_super: u32,
    pub use_prealloc: i32,
}

impl Default for Opt {
    fn default() -> Self {
        Self {
            use_orlov: TRUE,
            mfsalloc: FALSE,
            use_reserved_blocks: FALSE,
            block_with_super: 0,
            use_prealloc: FALSE,
        }
    }
}

// Bitmap types
pub type BitT = u32;
pub type BitchunkT = u32;
pub const FS_BITCHUNK_BITS: usize = core::mem::size_of::<BitchunkT>() * 8;

/// Block number type.
pub type BlockT = u32;

pub const IMAP: i32 = 0;
pub const BMAP: i32 = 1;
pub const IMAPD: i32 = 2;

pub fn fs_bitmap_chunks(block_size: usize) -> usize {
    block_size / core::mem::size_of::<BitchunkT>()
}

pub fn fs_bits_per_block(block_size: usize) -> usize {
    fs_bitmap_chunks(block_size) * FS_BITCHUNK_BITS
}
