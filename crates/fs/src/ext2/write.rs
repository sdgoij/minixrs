//! File write and block allocation — adapted from `minix/fs/ext2/write.c`

use libs::libminixfs::cache::{
    lmfs_get_block, lmfs_get_block_ino, lmfs_markclean, lmfs_markdirty, lmfs_put_block,
};
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
            // The difference is an `off_t` in the C original and is compared
            // signed: a position behind the last allocation takes the same arm
            // as a nearby one.
            let position_diff = position as i64 - (*rip).i_last_pos_bl_alloc as i64;
            let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
            if position_diff <= block_size as i64 {
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

/// Write a block number into an indirect block.
///
/// Reference: write.c wr_indir()
unsafe fn wr_indir(bp: *mut libs::libminixfs::types::Buf, index: usize, block: u32) {
    if bp.is_null() {
        return;
    }
    let ind = b_ind(bp);
    // On disk an unused entry is 0; `NO_BLOCK` is the in-memory sentinel, so
    // freeing an entry writes 0 rather than the sentinel. No byte swap:
    // rd_indir() reads the entries back the same way, and ext2 on disk is
    // little-endian like the hosts this runs on.
    let on_disk = if block == NO_BLOCK { 0 } else { block };
    core::ptr::write_unaligned(ind.add(index), on_disk);
}

/// True if the indirect block holds no entries.
///
/// Reference: write.c empty_indir()
unsafe fn empty_indir(bp: *mut libs::libminixfs::types::Buf, block_size: u64) -> bool {
    if bp.is_null() {
        return true;
    }
    let addr_in_block = (block_size / BLOCK_ADDRESS_BYTES as u64) as usize;
    let ind = b_ind(bp);
    for i in 0..addr_in_block {
        // 0 is how an unused entry is stored on disk; NO_BLOCK is the
        // in-memory sentinel a buffer built here may still hold.
        let b = core::ptr::read_unaligned(ind.add(i));
        if b != 0 && b != NO_BLOCK {
            return false;
        }
    }
    true
}

/// Free `block` on the inode's filesystem, if the inode has a superblock.
unsafe fn free_block_of(rip: *mut Inode, block: u32) {
    if block == NO_BLOCK {
        return;
    }
    if let Some(ref mut sp) = (*rip).i_sp {
        free_block(sp as &mut SuperBlock, block);
    }
}

/// Adjust `i_blocks` by `delta` blocks of `s_sectors_in_block` sectors.
///
/// Reference: write_map()'s `rip->i_blocks` bookkeeping
unsafe fn add_sectors(rip: *mut Inode, delta: i32) {
    if let Some(ref sp) = (*rip).i_sp {
        let n = sp.s_sectors_in_block as u32;
        if delta >= 0 {
            (*rip).i_blocks = (*rip).i_blocks.wrapping_add(n * delta as u32);
        } else {
            (*rip).i_blocks = (*rip).i_blocks.wrapping_sub(n * (-delta) as u32);
        }
    }
}

/// write_map — write a block number into an inode, or free the block at
/// `position` when `WMAP_FREE` is set.
///
/// Handles the direct blocks plus single, double and triple indirection, and
/// drops the indirect blocks that become empty. It is the only function that
/// maintains `i_blocks`.
///
/// Reference: write.c write_map()
pub unsafe fn write_map(rip: *mut Inode, position: u64, new_wblock: u32, op: i32) -> i32 {
    let block_size = match (*rip).i_sp {
        Some(ref sp) => sp.s_block_size as u64,
        None => return EIO,
    };
    let addr_in_block = (block_size / BLOCK_ADDRESS_BYTES as u64) as usize;
    let addr_in_block2 = addr_in_block * addr_in_block;
    let doub_ind_s = EXT2_NDIR_BLOCKS as u64 + addr_in_block as u64;
    let triple_ind_s = doub_ind_s + addr_in_block2 as u64;
    let out_range_s = triple_ind_s + (addr_in_block2 * addr_in_block) as u64;

    // Relative block number in the file.
    let block_pos = position / block_size;
    let free = (op as u32 & WMAP_FREE) != 0;
    (*rip).i_dirt = IN_DIRTY;

    // In the inode itself?
    if block_pos < EXT2_NDIR_BLOCKS as u64 {
        let idx = block_pos as usize;
        if (*rip).i_block[idx] != NO_BLOCK && free {
            free_block_of(rip, (*rip).i_block[idx]);
            (*rip).i_block[idx] = NO_BLOCK;
            add_sectors(rip, -1);
        } else {
            (*rip).i_block[idx] = new_wblock;
            add_sectors(rip, 1);
        }
        return OK;
    }

    let mut index1: usize = 0;
    let mut index2: usize = 0;
    let mut index3: usize = 0;
    let mut new_ind = false;
    let mut new_dbl = false;
    let mut new_triple = false;
    let mut single = false;
    let mut triple = false;
    let mut b1: u32 = NO_BLOCK;
    let mut b2: u32 = NO_BLOCK;
    let mut b3: u32 = NO_BLOCK;
    let mut bp: *mut libs::libminixfs::types::Buf = core::ptr::null_mut();
    let mut bp_dindir: *mut libs::libminixfs::types::Buf = core::ptr::null_mut();
    let mut bp_tindir: *mut libs::libminixfs::types::Buf = core::ptr::null_mut();

    let iomode = |new: bool| if new { NO_READ } else { NORMAL };

    if block_pos < doub_ind_s {
        // Single indirect.
        b1 = (*rip).i_block[EXT2_IND_BLOCK];
        index1 = (block_pos - EXT2_NDIR_BLOCKS as u64) as usize;
        single = true;
    } else if block_pos >= out_range_s {
        return EFBIG;
    } else {
        // Double or triple indirect: find the double indirect block first.
        let mut excess = (block_pos - doub_ind_s) as usize;
        b2 = (*rip).i_block[EXT2_DIND_BLOCK];

        if block_pos >= triple_ind_s {
            b3 = (*rip).i_block[EXT2_TIND_BLOCK];
            if b3 == NO_BLOCK && !free {
                b3 = alloc_block(&mut *rip, (*rip).i_bsearch);
                if b3 == NO_BLOCK {
                    return ENOSPC;
                }
                (*rip).i_block[EXT2_TIND_BLOCK] = b3;
                add_sectors(rip, 1);
                new_triple = true;
            }
            if b3 == NO_BLOCK {
                // Freeing and there is no triple indirect block: then there
                // is no double or single indirect block either.
                b1 = NO_BLOCK;
                b2 = NO_BLOCK;
            } else {
                bp_tindir = lmfs_get_block_ino(
                    (*rip).i_dev,
                    b3 as u64,
                    iomode(new_triple),
                    VMC_NO_INODE,
                    0,
                );
                if bp_tindir.is_null() {
                    return EIO;
                }
                if new_triple {
                    core::ptr::write_bytes(b_data(bp_tindir), 0, block_size as usize);
                    lmfs_markdirty(bp_tindir);
                }
                excess = (block_pos - triple_ind_s) as usize;
                index3 = excess / addr_in_block2;
                b2 = rd_indir(bp_tindir, index3);
                excess %= addr_in_block2;
            }
            triple = true;
        }

        if b2 == NO_BLOCK && !free {
            b2 = alloc_block(&mut *rip, (*rip).i_bsearch);
            if b2 == NO_BLOCK {
                lmfs_put_block(bp_tindir, PARTIAL_DATA_BLOCK);
                return ENOSPC;
            }
            if triple {
                wr_indir(bp_tindir, index3, b2);
                lmfs_markdirty(bp_tindir);
            } else {
                (*rip).i_block[EXT2_DIND_BLOCK] = b2;
            }
            add_sectors(rip, 1);
            new_dbl = true;
        }

        if b2 == NO_BLOCK {
            // Freeing and there is no double indirect block: then there is no
            // single indirect block either.
            b1 = NO_BLOCK;
        } else {
            bp_dindir =
                lmfs_get_block_ino((*rip).i_dev, b2 as u64, iomode(new_dbl), VMC_NO_INODE, 0);
            if bp_dindir.is_null() {
                lmfs_put_block(bp_tindir, PARTIAL_DATA_BLOCK);
                return EIO;
            }
            if new_dbl {
                core::ptr::write_bytes(b_data(bp_dindir), 0, block_size as usize);
                lmfs_markdirty(bp_dindir);
            }
            index2 = excess / addr_in_block;
            b1 = rd_indir(bp_dindir, index2);
            index1 = excess % addr_in_block;
        }
        single = false;
    }

    // The single indirect block, created unless we are freeing.
    if b1 == NO_BLOCK && !free {
        b1 = alloc_block(&mut *rip, (*rip).i_bsearch);
        if b1 == NO_BLOCK {
            lmfs_put_block(bp_dindir, PARTIAL_DATA_BLOCK);
            lmfs_put_block(bp_tindir, PARTIAL_DATA_BLOCK);
            return ENOSPC;
        }
        if single {
            (*rip).i_block[EXT2_IND_BLOCK] = b1;
        } else {
            wr_indir(bp_dindir, index2, b1);
            lmfs_markdirty(bp_dindir);
        }
        add_sectors(rip, 1);
        new_ind = true;
    }

    if b1 != NO_BLOCK {
        bp = lmfs_get_block_ino((*rip).i_dev, b1 as u64, iomode(new_ind), VMC_NO_INODE, 0);
        if bp.is_null() {
            lmfs_put_block(bp_dindir, PARTIAL_DATA_BLOCK);
            lmfs_put_block(bp_tindir, PARTIAL_DATA_BLOCK);
            return EIO;
        }
        if new_ind {
            core::ptr::write_bytes(b_data(bp), 0, block_size as usize);
        }

        if free {
            let old_block = rd_indir(bp, index1);
            if old_block != NO_BLOCK {
                free_block_of(rip, old_block);
                add_sectors(rip, -1);
                wr_indir(bp, index1, NO_BLOCK);
            }

            // Was that the last entry? Then the indirect block goes too.
            if empty_indir(bp, block_size) {
                free_block_of(rip, b1);
                add_sectors(rip, -1);
                b1 = NO_BLOCK;
                if single {
                    (*rip).i_block[EXT2_IND_BLOCK] = NO_BLOCK;
                } else {
                    wr_indir(bp_dindir, index2, NO_BLOCK);
                    lmfs_markdirty(bp_dindir);
                }
            }
        } else {
            wr_indir(bp, index1, new_wblock);
            add_sectors(rip, 1);
        }

        // b1 is NO_BLOCK only here when the block was just freed, in which
        // case the zeroed buffer must not be written back.
        if b1 == NO_BLOCK {
            lmfs_markclean(bp);
        } else {
            lmfs_markdirty(bp);
        }
        lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
    }

    // Keep the double indirect block only while it has entries.
    if b1 == NO_BLOCK && !single && b2 != NO_BLOCK && empty_indir(bp_dindir, block_size) {
        lmfs_markclean(bp_dindir);
        free_block_of(rip, b2);
        add_sectors(rip, -1);
        b2 = NO_BLOCK;
        if triple {
            wr_indir(bp_tindir, index3, NO_BLOCK);
            lmfs_markdirty(bp_tindir);
        } else {
            (*rip).i_block[EXT2_DIND_BLOCK] = NO_BLOCK;
        }
    }

    // ... and the triple indirect block only while it has entries.
    if b2 == NO_BLOCK && triple && b3 != NO_BLOCK && empty_indir(bp_tindir, block_size) {
        lmfs_markclean(bp_tindir);
        free_block_of(rip, b3);
        add_sectors(rip, -1);
        (*rip).i_block[EXT2_TIND_BLOCK] = NO_BLOCK;
    }

    lmfs_put_block(bp_dindir, PARTIAL_DATA_BLOCK);
    lmfs_put_block(bp_tindir, PARTIAL_DATA_BLOCK);

    OK
}
