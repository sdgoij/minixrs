//! File write and block allocation — adapted from `minix/fs/ext2/write.c`

use libs::libminixfs::cache::{lmfs_get_block, lmfs_get_block_ino, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::{
    FULL_DATA_BLOCK, NO_READ, NORMAL, PARTIAL_DATA_BLOCK, VMC_NO_INODE,
};

use crate::ext2::balloc::*;
use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::read::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// clear_zone — clear a range of blocks on disk.
pub unsafe fn clear_zone(rip: *mut Inode, pos: u64, _flag: i32) -> i32 {
    let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
    let start_block = pos / block_size;
    let end_block = ((*rip).i_size as u64 + block_size - 1) / block_size;

    for b in start_block..end_block {
        let phys = read_map(rip, b * block_size, 0);
        if phys != NO_BLOCK {
            let bp = lmfs_get_block_ino(
                (*rip).i_dev,
                phys as u64,
                NORMAL,
                (*rip).i_num as u64,
                b * block_size,
            );
            if !bp.is_null() {
                core::ptr::write_bytes(b_data(bp), 0, block_size as usize);
                lmfs_markdirty(bp);
                lmfs_put_block(bp, FULL_DATA_BLOCK);
            }
        }
    }
    OK
}

/// new_block — acquire a new block and return a pointer to it.
pub unsafe fn new_block(rip: *mut Inode, position: u64) -> *mut libs::libminixfs::types::Buf {
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
            let ext2 = glo::ext2_ptr();
            (*ext2).err_code = ENOSPC;
            return core::ptr::null_mut();
        }

        let r = write_map(rip, position, block, 0);
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

    // Get the block and zero it
    let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
    let ino_off = position & !(block_size - 1);
    let bp = lmfs_get_block_ino(
        (*rip).i_dev,
        block as u64,
        NO_READ,
        (*rip).i_num as u64,
        ino_off,
    );
    if !bp.is_null() {
        core::ptr::write_bytes(b_data(bp), 0, block_size as usize);
        lmfs_markdirty(bp);
    }
    bp
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
