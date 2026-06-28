//! File read and block mapping — adapted from `minix/fs/ext2/read.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_readwrite — read/write dispatch.
pub unsafe fn fs_readwrite() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message, dispatch read or write
    // For now, stub
    let _ = ext2;
    OK
}

/// Read map: logical → physical block mapping.
pub unsafe fn read_map(rip: *mut Inode, position: u64, _opportunistic: i32) -> u32 {
    let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
    let block_pos = position / block_size;
    let addr_in_block = (block_size as u32) / BLOCK_ADDRESS_BYTES;

    // Direct blocks
    if (block_pos as usize) < EXT2_NDIR_BLOCKS {
        return (*rip).i_block[block_pos as usize];
    }

    let doub_ind_s = EXT2_NDIR_BLOCKS as u64 + addr_in_block as u64;

    // Single indirect
    if block_pos < doub_ind_s {
        let index = (block_pos - EXT2_NDIR_BLOCKS as u64) as u32;
        let b1 = (*rip).i_block[EXT2_IND_BLOCK];
        if b1 == NO_BLOCK {
            return NO_BLOCK;
        }
        // TODO: read indirect block via get_block + rd_indir
        let _ = index;
        return rd_indir(b1, index as usize);
    }

    // Double or triple indirect
    let addr_in_block2 = (addr_in_block as u64) * (addr_in_block as u64);
    let triple_ind_s = doub_ind_s + addr_in_block2;

    if block_pos >= triple_ind_s + addr_in_block2 * (addr_in_block as u64) {
        return NO_BLOCK; // Beyond max file size
    }

    if block_pos >= triple_ind_s {
        // Triple indirect
        let excess = block_pos - triple_ind_s;
        let index3 = (excess / addr_in_block2) as u32;
        let b3 = (*rip).i_block[EXT2_TIND_BLOCK];
        if b3 == NO_BLOCK {
            return NO_BLOCK;
        }
        // TODO: read triple indirect block
        let _ = index3;
        NO_BLOCK
    } else {
        // Double indirect
        let excess = block_pos - doub_ind_s;
        let index2 = (excess / addr_in_block as u64) as u32;
        let b2 = (*rip).i_block[EXT2_DIND_BLOCK];
        if b2 == NO_BLOCK {
            return NO_BLOCK;
        }
        // TODO: read double indirect block
        let _ = index2;
        NO_BLOCK
    }
}

/// Read indirect block entry.
pub fn rd_indir(block: u32, _index: usize) -> u32 {
    if block == NO_BLOCK {
        return NO_BLOCK;
    }
    // TODO: read block from buffer cache, extract entry at index
    NO_BLOCK
}

/// read_ahead stub.
pub fn read_ahead() {
    // TODO: implement read-ahead
}
