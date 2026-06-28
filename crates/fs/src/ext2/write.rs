//! File write and block allocation — adapted from `minix/fs/ext2/write.c`

use crate::ext2::balloc::*;
use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::read::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// clear_zone — clear a zone on disk (stub).
pub fn clear_zone(_rip: *mut Inode, _pos: u64, _flag: i32) -> i32 {
    // TODO: implement
    OK
}

/// new_block — acquire a new block and return a pointer to it.
pub unsafe fn new_block(rip: *mut Inode, position: u64) -> *mut u8 {
    let b = read_map(rip, position, 0);
    let block: u32;
    if b == NO_BLOCK {
        let mut goal = NO_BLOCK;
        if (*rip).i_last_pos_bl_alloc != 0 {
            let position_diff = position - (*rip).i_last_pos_bl_alloc;
            let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
            if position_diff <= block_size {
                if (*rip).i_bsearch != 0 {
                    goal = (*rip).i_bsearch + 1;
                }
            } else {
                (*rip).i_preallocation = 0;
                discard_preallocated_blocks(Some(&mut *rip));
            }
        }

        block = alloc_block(&mut *rip, goal);
        if block == NO_BLOCK {
            unsafe {
                let ext2 = glo::ext2_ptr();
                (*ext2).err_code = ENOSPC;
            }
            return core::ptr::null_mut();
        }

        let r = write_map(rip, position as u64, block, 0);
        if r != OK {
            if let Some(ref mut sp) = (*rip).i_sp {
                free_block(sp as &mut SuperBlock, block);
            }
            return core::ptr::null_mut();
        }

        (*rip).i_last_pos_bl_alloc = position;
        if position == 0 {
            (*rip).i_last_pos_bl_alloc += 1;
        }
    } else {
        block = b;
    }

    // TODO: get_block and zero it
    let _ = block;
    core::ptr::null_mut()
}

/// write_map — write a new block into an inode.
pub unsafe fn write_map(rip: *mut Inode, position: u64, new_wblock: u32, op: i32) -> i32 {
    let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
    let block_pos = position / block_size;

    (*rip).i_dirt = IN_DIRTY;

    if (block_pos as usize) < EXT2_NDIR_BLOCKS {
        if (*rip).i_block[block_pos as usize] != NO_BLOCK && (op as u32 & WMAP_FREE) != 0 {
            if let Some(ref mut sp) = (*rip).i_sp {
                free_block(sp as &mut SuperBlock, (*rip).i_block[block_pos as usize]);
            }
            (*rip).i_block[block_pos as usize] = NO_BLOCK;
            if let Some(ref sp) = (*rip).i_sp {
                (*rip).i_blocks -= sp.s_sectors_in_block as u32;
            }
        } else {
            (*rip).i_block[block_pos as usize] = new_wblock;
            if let Some(ref sp) = (*rip).i_sp {
                (*rip).i_blocks += sp.s_sectors_in_block as u32;
            }
        }
        return OK;
    }

    // Indirect blocks — TODO: implement full indirect/double/triple support
    EINVAL
}
