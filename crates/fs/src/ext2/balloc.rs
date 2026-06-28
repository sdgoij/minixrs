//! Block allocation and deallocation — adapted from `minix/fs/ext2/balloc.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

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
        let opt = core::ptr::addr_of_mut!(glo::OPT);
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

fn alloc_block_bit(rip: &mut Inode, goal: u32) -> u32 {
    let sp = match rip.i_sp.as_mut() {
        Some(sp) => *sp as *mut SuperBlock,
        None => return NO_BLOCK,
    };

    // TODO: Implement actual block bitmap allocation via buffer cache
    // In C balloc.c, `goal` seeds the bitmap search position:
    //   1. Compute block group from goal via group = (goal - s_first_data_block) / s_blocks_per_group
    //   2. Load the group's block bitmap via get_block
    //   3. Search forward from bit = (goal - s_first_data_block) % s_blocks_per_group
    //   4. Fall back to linear search across all groups if not found in goal group
    //   5. Update sp.s_bsearch on success, set bitmap bit, mark buffer dirty
    //   6. Update s_free_blocks_count and group's free_blocks_count
    // When implementing, DO NOT use _goal prefix — goal MUST be used for search.
    let _ = (sp, goal);
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

    let _gd_ref = unsafe { &mut *gd };
    let _bit_val = bit;
    // TODO: Implement actual bitmap free via buffer cache

    if bit_returned < sp.s_bsearch {
        sp.s_bsearch = bit_returned;
    }
}
