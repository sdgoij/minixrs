//! Super block management — adapted from `minix/fs/ext2/super.c`

use core::sync::atomic::Ordering;

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::types::*;
use crate::ext2::utility::*;
use libs::libminixfs::cache::{lmfs_get_block, lmfs_put_block};
use libs::libminixfs::types::Buf;

/// Get the block size for a device.
pub fn get_block_size(dev: u32) -> u16 {
    unsafe {
        let sp = get_super(dev);
        if sp.is_null() {
            return 4096;
        }
        (*sp).s_block_size
    }
}

/// Get the super block for a device (single superblock for ext2).
pub fn get_super(dev: u32) -> *mut SuperBlock {
    if dev == NO_DEV {
        return core::ptr::null_mut();
    }
    unsafe {
        let sp = glo::SUPERBLOCK.load(Ordering::Relaxed);
        if !sp.is_null() && (*sp).s_dev == dev {
            return sp;
        }
    }
    core::ptr::null_mut()
}

/// Read and validate a super block from disk.
///
/// `sp` must already have its on-disk fields filled in (from buffer cache).
/// This function validates the superblock and loads the group descriptor table.
pub fn read_super(sp: &mut SuperBlock) -> i32 {
    let dev = sp.s_dev;
    if dev == NO_DEV {
        return EINVAL;
    }

    // Validate magic
    if sp.s_magic != SUPER_MAGIC {
        return EINVAL;
    }

    sp.s_block_size = 1024 * (1u16 << (sp.s_log_block_size as u16));

    if sp.s_block_size < 1024  {
        return EINVAL;
    }

    if (sp.s_block_size % 512) != 0 {
        return EINVAL;
    }

    if SUPER_SIZE_D > sp.s_block_size as usize {
        return EINVAL;
    }

    sp.s_sectors_in_block = sp.s_block_size / 512;

    // Validate inode size
    let inode_size = ext2_inode_size(sp);
    if (inode_size & (inode_size - 1)) != 0 || inode_size > sp.s_block_size as u32 {
        return EINVAL;
    }

    sp.s_blocksize_bits = (sp.s_log_block_size + 10) as u8;
    sp.s_max_size = ext2_max_size(sp.s_block_size);
    sp.s_inodes_per_block = sp.s_block_size as u32 / inode_size;
    if sp.s_inodes_per_block == 0 || sp.s_inodes_per_group == 0 {
        return EINVAL;
    }

    sp.s_itb_per_group = sp.s_inodes_per_group / sp.s_inodes_per_block;
    sp.s_desc_per_block = sp.s_block_size as u32 / core::mem::size_of::<GroupDesc>() as u32;

    sp.s_groups_count =
        ((sp.s_blocks_count - sp.s_first_data_block - 1) / sp.s_blocks_per_group) + 1;

    sp.s_gdb_count = (sp.s_groups_count + sp.s_desc_per_block - 1) / sp.s_desc_per_block;

    if sp.s_inodes_count < 1 || sp.s_blocks_count < 1 {
        return EINVAL;
    }

    // Load group descriptor table blocks into cache
    let gdt_start_block = sp.s_first_data_block + 1;
    let gdb_count = sp.s_gdb_count;
    for i in 0..gdb_count.min(4) {
        unsafe {
            let bp = lmfs_get_block(dev, (gdt_start_block + i) as u64);
            if bp.is_null() {
                return EINVAL;
            }
            sp.s_gdt_bufs[i as usize] = bp as usize;
            sp.s_gdt_data[i as usize] = (*bp).data_ptr as usize;
        }
    }

    sp.s_dirs_counter = ext2_count_dirs(sp);

    // Start block search
    sp.s_bsearch = sp.s_first_data_block + 1 + sp.s_gdb_count + 2 + sp.s_itb_per_group;
    sp.s_igsearch = 0;
    sp.s_dev = dev;

    OK
}

/// Write super block and GDT back to disk.
pub fn write_super(sp: &mut SuperBlock) {
    if sp.s_rd_only != 0 {
        return;
    }
    if sp.s_dev == NO_DEV {
        return;
    }

    // Mark any loaded GDT buffer as dirty so it gets written back
    for i in 0..sp.s_gdb_count.min(4) as usize {
        let buf_ptr = sp.s_gdt_bufs[i];
        if buf_ptr != 0 {
            unsafe {
                let bp = buf_ptr as *mut Buf;
                if !bp.is_null() {
                    libs::libminixfs::cache::lmfs_markdirty(bp);
                }
            }
        }
    }

    glo::GROUP_DESCRIPTORS_DIRTY.store(0, Ordering::Relaxed);
}

/// Get group descriptor for a given block group.
pub fn get_group_desc(sp: &SuperBlock, bnum: u32) -> *mut GroupDesc {
    if bnum >= sp.s_groups_count {
        return core::ptr::null_mut();
    }

    let descs_per_block = sp.s_desc_per_block;
    let block_index = (bnum / descs_per_block) as usize;
    let offset_in_block = (bnum % descs_per_block) as usize;

    if block_index >= 4 {
        return core::ptr::null_mut();
    }

    let data_ptr = sp.s_gdt_data[block_index];
    if data_ptr == 0 {
        return core::ptr::null_mut();
    }

    unsafe {
        let gd_ptr = (data_ptr as *mut u8).add(offset_in_block * core::mem::size_of::<GroupDesc>());
        gd_ptr as *mut GroupDesc
    }
}

fn ext2_max_size(block_size: u16) -> u64 {
    match block_size {
        1024 => 0x7FFFFFFF, // LONG_MAX
        2048 => 0x7FFFFFFF,
        4096 => 0x7FFFFFFF,
        _ => 67383296,
    }
}

fn ext2_count_dirs(sp: &SuperBlock) -> u32 {
    let mut count = 0u32;
    for i in 0..sp.s_groups_count {
        let desc = get_group_desc(sp, i);
        if desc.is_null() {
            continue;
        }
        unsafe {
            count += (*desc).used_dirs_count as u32;
        }
    }
    count
}

/// Compute EXT2_INODE_SIZE based on revision level.
pub fn ext2_inode_size(sp: &SuperBlock) -> u32 {
    if sp.s_rev_level == EXT2_GOOD_OLD_REV {
        EXT2_GOOD_OLD_INODE_SIZE
    } else {
        sp.s_inode_size as u32
    }
}

/// Compute EXT2_FIRST_INO based on revision level.
pub fn ext2_first_ino(sp: &SuperBlock) -> u32 {
    if sp.s_rev_level == EXT2_GOOD_OLD_REV {
        EXT2_GOOD_OLD_FIRST_INO
    } else {
        sp.s_first_ino
    }
}
