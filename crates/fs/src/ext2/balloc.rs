//! Block allocation and deallocation — adapted from `minix/fs/ext2/balloc.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::super_::*;
use crate::ext2::types::BitchunkT;
use crate::ext2::types::*;
use crate::ext2::utility::*;
use libs::libminixfs::cache::{lmfs_get_block, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::FULL_DATA_BLOCK;
use libs::libminixfs::types::Buf;

/// Discard preallocated blocks for an inode (or all inodes if rip is None).
pub fn discard_preallocated_blocks(rip: Option<&mut Inode>) {
    if let Some(rip) = rip {
        rip.i_prealloc_count = 0;
        rip.i_prealloc_index = 0;
        for i in 0..EXT2_PREALLOC_BLOCKS {
            if rip.i_prealloc_blocks[i] != NO_BLOCK {
                free_block(rip.i_sp.as_mut().unwrap(), rip.i_prealloc_blocks[i]);
                rip.i_prealloc_blocks[i] = NO_BLOCK;
            }
        }
    } else {
        unsafe {
            for i in 0..NR_INODES {
                let rip = glo::get_inode_ptr(i);
                (*rip).i_prealloc_count = 0;
                (*rip).i_prealloc_index = 0;
                (*rip).i_preallocation = 0;
                for j in 0..EXT2_PREALLOC_BLOCKS {
                    if (*rip).i_prealloc_blocks[j] != NO_BLOCK {
                        let sp = (*rip).i_sp.as_mut().unwrap();
                        free_block(sp as &mut SuperBlock, (*rip).i_prealloc_blocks[j]);
                        (*rip).i_prealloc_blocks[j] = NO_BLOCK;
                    }
                }
            }
        }
    }
}

/// Allocate a block for an inode.
pub fn alloc_block(rip: &mut Inode, block: u32) -> u32 {
    // Use raw pointers to avoid borrow checker conflicts
    let sp_ptr: *mut SuperBlock = match rip.i_sp.as_mut() {
        Some(sp) => *sp as *mut SuperBlock,
        None => return NO_BLOCK,
    };

    unsafe {
        if (*sp_ptr).s_rd_only != 0 {
            return NO_BLOCK;
        }

        // Check for free blocks; discard preallocations if running low
        let opt = glo::OPT.get();
        let low_on_blocks = if (*opt).use_reserved_blocks == 0
            && (*sp_ptr).s_free_blocks_count <= (*sp_ptr).s_r_blocks_count
        {
            true
        } else {
            (*sp_ptr).s_free_blocks_count <= EXT2_PREALLOC_BLOCKS as u32
        };

        if low_on_blocks {
            discard_preallocated_blocks(None);
        }

        if (*opt).use_reserved_blocks == 0
            && (*sp_ptr).s_free_blocks_count <= (*sp_ptr).s_r_blocks_count
        {
            return NO_BLOCK;
        } else if (*sp_ptr).s_free_blocks_count == 0 {
            return NO_BLOCK;
        }
    }

    let goal: u32;
    if block != NO_BLOCK {
        goal = block;
        if rip.i_preallocation != 0 && rip.i_prealloc_count > 0 {
            let b = rip.i_prealloc_blocks[rip.i_prealloc_index as usize];
            if block == b || (block + 1) == b {
                rip.i_prealloc_blocks[rip.i_prealloc_index as usize] = NO_BLOCK;
                rip.i_prealloc_count -= 1;
                rip.i_prealloc_index += 1;
                if rip.i_prealloc_index as usize >= EXT2_PREALLOC_BLOCKS {
                    rip.i_prealloc_index = 0;
                }
                rip.i_bsearch = b;
                return b;
            } else {
                rip.i_preallocation = 0;
                discard_preallocated_blocks(Some(rip));
            }
        }
    } else {
        let sp = rip.i_sp.as_mut().unwrap();
        let group = (rip.i_num - 1) / sp.s_inodes_per_group;
        goal = sp.s_blocks_per_group * group + sp.s_first_data_block;
    }

    let b = alloc_block_bit(rip, goal);
    if b != NO_BLOCK {
        rip.i_bsearch = b;
    }
    b
}

/// Allocate a block bit from the block bitmap.
fn alloc_block_bit(rip: &mut Inode, goal: u32) -> u32 {
    let sp = match rip.i_sp.as_mut() {
        Some(sp) => *sp as *mut SuperBlock,
        None => return NO_BLOCK,
    };

    unsafe {
        let sp = &mut *sp;

        let mut goal = goal;
        if goal >= sp.s_blocks_count || (goal < sp.s_first_data_block && goal != 0) {
            goal = sp.s_bsearch;
        }

        let mut update_bsearch = false;
        if goal <= sp.s_bsearch {
            goal = sp.s_bsearch;
            update_bsearch = true;
        }

        // Figure out where to start the bit search.
        let mut word =
            ((goal - sp.s_first_data_block) % sp.s_blocks_per_group) / FS_BITCHUNK_BITS as u32;
        let mut group = (goal - sp.s_first_data_block) / sp.s_blocks_per_group;

        for _ in 0..=sp.s_groups_count {
            if group >= sp.s_groups_count {
                group = 0;
            }

            let gd = get_group_desc(sp, group);
            if gd.is_null() {
                word = 0;
                group += 1;
                continue;
            }

            if (*gd).free_blocks_count == 0 {
                word = 0;
                group += 1;
                continue;
            }

            let bp = lmfs_get_block((*sp).s_dev, (*gd).block_bitmap as u64);
            if bp.is_null() {
                word = 0;
                group += 1;
                continue;
            }

            // Convert data_ptr to bitmap slice
            let block_size = sp.s_block_size as usize;
            let bitmap_ptr = (*bp).data_ptr as *mut BitchunkT;
            let bitmap_len = block_size / core::mem::size_of::<BitchunkT>();
            let bitmap = core::slice::from_raw_parts_mut(bitmap_ptr, bitmap_len);

            let bit = setbit(bitmap, sp.s_blocks_per_group, word);
            if bit == -1 {
                lmfs_put_block(bp, FULL_DATA_BLOCK);
                if word == 0 {
                    group += 1;
                    continue;
                } else {
                    word = 0;
                    group += 1;
                    continue;
                }
            }

            let block = sp.s_first_data_block + group * sp.s_blocks_per_group + bit as u32;

            lmfs_markdirty(bp);
            lmfs_put_block(bp, FULL_DATA_BLOCK);

            (*gd).free_blocks_count -= 1;
            sp.s_free_blocks_count -= 1;

            if update_bsearch && block != NO_BLOCK {
                sp.s_bsearch = block;
            }

            return block;
        }
    }

    NO_BLOCK
}

/// Free a block by turning off its bitmap bit.
pub fn free_block(sp: &mut SuperBlock, bit_returned: u32) {
    if sp.s_rd_only != 0 {
        return;
    }

    if bit_returned >= sp.s_blocks_count || bit_returned < sp.s_first_data_block {
        return;
    }

    let group = (bit_returned - sp.s_first_data_block) / sp.s_blocks_per_group;
    let bit = (bit_returned - sp.s_first_data_block) % sp.s_blocks_per_group;

    let gd = get_group_desc(sp, group);
    if gd.is_null() {
        return;
    }

    let bp = unsafe { lmfs_get_block(sp.s_dev, (*gd).block_bitmap as u64) };
    if bp.is_null() {
        return;
    }

    unsafe {
        // Convert data_ptr to bitmap slice
        let block_size = sp.s_block_size as usize;
        let bitmap_ptr = (*bp).data_ptr as *mut BitchunkT;
        let bitmap_len = block_size / core::mem::size_of::<BitchunkT>();
        let bitmap = core::slice::from_raw_parts_mut(bitmap_ptr, bitmap_len);

        if unsetbit(bitmap, bit) != 0 {
            // Bit was already free; just put the block back
            lmfs_put_block(bp, FULL_DATA_BLOCK);
            return;
        }

        lmfs_markdirty(bp);
        lmfs_put_block(bp, FULL_DATA_BLOCK);

        (*gd).free_blocks_count += 1;
        sp.s_free_blocks_count += 1;
    }

    if bit_returned < sp.s_bsearch {
        sp.s_bsearch = bit_returned;
    }
}
