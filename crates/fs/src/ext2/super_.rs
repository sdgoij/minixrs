//! Super block management — adapted from `minix/fs/ext2/super.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// Get the super block for a device (single superblock for ext2).
pub fn get_super(dev: u32) -> *mut SuperBlock {
    if dev == NO_DEV {
        return core::ptr::null_mut();
    }
    unsafe {
        let sp = glo::SUPERBLOCK;
        if !sp.is_null() && (*sp).s_dev == dev {
            return sp;
        }
    }
    core::ptr::null_mut()
}

/// Read and validate a super block from disk.
pub fn read_super(sp: &mut SuperBlock) -> i32 {
    let dev = sp.s_dev;
    if dev == NO_DEV {
        return EINVAL;
    }

    // TODO: Read super block from disk via bdev_read
    // For now validate magic only
    if sp.s_magic != SUPER_MAGIC {
        return EINVAL;
    }

    sp.s_block_size = 1024 * (1u16 << (sp.s_log_block_size as u16));

    if sp.s_block_size < 4096 {
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

    // TODO: Read group descriptors from disk

    if sp.s_inodes_count < 1 || sp.s_blocks_count < 1 {
        return EINVAL;
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

    // TODO: Write super block and group descriptors to disk

    unsafe {
        glo::GROUP_DESCRIPTORS_DIRTY = 0;
    }
}

/// Get group descriptor for a given block group.
pub fn get_group_desc(sp: &SuperBlock, bnum: u32) -> *mut GroupDesc {
    if bnum >= sp.s_groups_count {
        return core::ptr::null_mut();
    }
    // In the real implementation, s_group_desc would be a pointer to an array.
    // For now return null.
    core::ptr::null_mut()
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
