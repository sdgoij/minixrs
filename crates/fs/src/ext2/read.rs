//! File read and block mapping — adapted from `minix/fs/ext2/read.c`

use core::sync::atomic::Ordering;

use libs::libminixfs::cache::{lmfs_get_block, lmfs_get_block_ino, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::{
    DIRECTORY_BLOCK, FULL_DATA_BLOCK, NO_READ, NORMAL, PARTIAL_DATA_BLOCK, PREFETCH, VMC_NO_INODE,
};

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::super_::get_block_size;
use crate::ext2::types::*;
use crate::ext2::utility::*;
use crate::ext2::write::*;

/// fs_readwrite — read/write dispatch.
pub unsafe fn fs_readwrite() -> i32 {
    let ext2 = glo::ext2_ptr();

    let ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing
    let rip = find_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    let mode_word = (*rip).i_mode & I_TYPE;
    let regular = mode_word == I_REGULAR || mode_word == I_NAMED_PIPE;
    let block_spec = mode_word == I_BLOCK_SPECIAL;

    let block_size: u64;
    let f_size: u64;
    if block_spec {
        block_size = get_block_size((*rip).i_block[0]) as u64;
        f_size = u64::MAX;
    } else {
        block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
        f_size = (*rip).i_size as u64;
    }

    // FIXME: determine rw_flag from message type
    let rw_flag = READING;
    let _gid: u32 = 0; // FIXME: parse grant from message
    let mut position: u64 = 0; // FIXME: parse seek_pos from message
    let mut nrbytes: usize = 0; // FIXME: parse nbytes from message

    if rw_flag == WRITING && !block_spec {
        if position > (*(*rip).i_sp.as_ref().unwrap()).s_max_size - nrbytes as u64 {
            return EFBIG;
        }
    }

    let mut cum_io: usize = 0;

    while nrbytes != 0 {
        let off = (position % block_size) as u32;
        let mut chunk = core::cmp::min(nrbytes as u64, block_size - off as u64) as u32;

        if rw_flag == READING {
            let bytes_left = f_size - position;
            if position >= f_size {
                break;
            }
            if chunk as u64 > bytes_left {
                chunk = bytes_left as u32;
            }
        }

        // Read or write chunk
        let r = rw_chunk(
            rip,
            position,
            off,
            chunk,
            nrbytes as u32,
            rw_flag,
            _gid,
            cum_io as u32,
            block_size as u32,
        );
        if r != OK {
            break;
        }
        if (*ext2).rdwt_err < 0 {
            break;
        }

        nrbytes -= chunk as usize;
        cum_io += chunk as usize;
        position += chunk as u64;
    }

    // On write, update file size
    if rw_flag == WRITING {
        if (regular || mode_word == I_DIRECTORY) && position > f_size {
            (*rip).i_size = position as u32;
        }
    }

    // Set up read-ahead
    if rw_flag == READING
        && (*rip).i_seek == NO_SEEK
        && position % block_size == 0
        && (regular || mode_word == I_DIRECTORY)
    {
        glo::RDAHED_INODE.store(rip, Ordering::Relaxed);
        glo::RDAHEDPOS.store(position, Ordering::Relaxed);
        read_ahead();
    }

    (*rip).i_seek = NO_SEEK;

    if (*ext2).rdwt_err != OK {
        return (*ext2).rdwt_err;
    }

    if rw_flag == READING {
        (*rip).i_update |= ATIME;
    }
    if rw_flag == WRITING {
        (*rip).i_update |= CTIME | MTIME;
    }
    (*rip).i_dirt = IN_DIRTY;

    // FIXME: set reply nbytes = cum_io, seek_pos = position

    OK
}

/// Read/Write one chunk (partial block).
pub unsafe fn rw_chunk(
    rip: *mut Inode,
    position: u64,
    off: u32,
    chunk: u32,
    _left: u32,
    rw_flag: i32,
    _gid: u32,
    _buf_off: u32,
    block_size: u32,
) -> i32 {
    let mut bp: *mut libs::libminixfs::types::Buf = core::ptr::null_mut();
    let mut r = OK;

    let block_spec = ((*rip).i_mode & I_TYPE) == I_BLOCK_SPECIAL;

    let b: u32;
    let dev: u32;
    if block_spec {
        b = (position / block_size as u64) as u32;
        dev = (*rip).i_block[0];
    } else {
        b = read_map(rip, position, 0);
        dev = (*rip).i_dev;
    }

    let ino_off = position & !(block_size as u64 - 1);

    if !block_spec && b == NO_BLOCK {
        if rw_flag == READING {
            // Reading from a hole — must read as all zeros
            // FIXME: sys_safememset to grant
            return OK;
        } else {
            // Writing to a hole — create and enter in inode
            bp = new_block(rip, position);
            if bp.is_null() {
                return (*glo::ext2_ptr()).err_code;
            }
        }
    } else if rw_flag == READING || rw_flag == PEEKING {
        // Read (with read-ahead via rahead)
        bp = rahead(rip, b, position, block_size);
    } else {
        // WRITING
        let n = if chunk == block_size { NO_READ } else { NORMAL };
        if !block_spec && off == 0 && position >= (*rip).i_size as u64 {
            // Full block write beyond EOF — no need to read
            bp = lmfs_get_block_ino(dev, b as u64, NO_READ, (*rip).i_num as u64, ino_off);
        } else {
            bp = lmfs_get_block_ino(dev, b as u64, n, (*rip).i_num as u64, ino_off);
        }
    }

    if bp.is_null() {
        return EIO;
    }

    if rw_flag == WRITING {
        lmfs_markdirty(bp);
    }

    // FIXME: copy data between grant and bp->data_ptr using sys_safecopyto/from
    let _ = off;
    let _ = chunk;

    let block_type = if off + chunk == block_size {
        FULL_DATA_BLOCK
    } else {
        PARTIAL_DATA_BLOCK
    };
    lmfs_put_block(bp, block_type);

    r
}

/// Read map: logical → physical block mapping.
pub unsafe fn read_map(rip: *mut Inode, position: u64, opportunistic: i32) -> u32 {
    let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
    let block_pos = position / block_size;
    let addr_in_block = (block_size as u32) / BLOCK_ADDRESS_BYTES;
    let addr_in_block_u = addr_in_block as u64;

    // Direct blocks (0-11)
    if (block_pos as usize) < EXT2_NDIR_BLOCKS {
        return (*rip).i_block[block_pos as usize];
    }

    let doub_ind_s = EXT2_NDIR_BLOCKS as u64 + addr_in_block_u;
    let addr_in_block2 = addr_in_block_u * addr_in_block_u;
    let triple_ind_s = doub_ind_s + addr_in_block2;
    let out_range_s = triple_ind_s + addr_in_block2 * addr_in_block_u;

    let iomode = if opportunistic != 0 { PREFETCH } else { NORMAL };

    // Single indirect
    if block_pos < doub_ind_s {
        let mindex = (block_pos - EXT2_NDIR_BLOCKS as u64) as u32;
        let b = (*rip).i_block[EXT2_IND_BLOCK];
        if b == NO_BLOCK {
            return NO_BLOCK;
        }
        let bp = lmfs_get_block_ino((*rip).i_dev, b as u64, iomode, VMC_NO_INODE, 0);
        if opportunistic != 0 && (*bp).lmfs_dev == libs::libminixfs::constants::NO_DEV {
            lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
            return NO_BLOCK;
        }
        let result = rd_indir(bp, mindex as usize);
        lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
        return result;
    }

    if block_pos >= out_range_s {
        return NO_BLOCK;
    }

    // Double or triple indirect
    let mut excess = block_pos - doub_ind_s;
    let mut b = (*rip).i_block[EXT2_DIND_BLOCK];

    if block_pos >= triple_ind_s {
        // Triple indirect
        b = (*rip).i_block[EXT2_TIND_BLOCK];
        if b == NO_BLOCK {
            return NO_BLOCK;
        }
        let bp = lmfs_get_block_ino((*rip).i_dev, b as u64, NORMAL, VMC_NO_INODE, 0);
        excess = block_pos - triple_ind_s;
        let mindex = (excess / addr_in_block2) as u32;
        b = rd_indir(bp, mindex as usize);
        lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
        excess = excess % addr_in_block2;
    }

    if b == NO_BLOCK {
        return NO_BLOCK;
    }

    // Double indirect
    {
        let bp = lmfs_get_block_ino((*rip).i_dev, b as u64, iomode, VMC_NO_INODE, 0);
        if opportunistic != 0 && (*bp).lmfs_dev == libs::libminixfs::constants::NO_DEV {
            lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
            return NO_BLOCK;
        }
        let mindex = (excess / addr_in_block_u) as u32;
        b = rd_indir(bp, mindex as usize);
        lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
        excess = excess % addr_in_block_u;
    }

    if b == NO_BLOCK {
        return NO_BLOCK;
    }

    // Single indirect
    {
        let bp = lmfs_get_block_ino((*rip).i_dev, b as u64, iomode, VMC_NO_INODE, 0);
        if opportunistic != 0 && (*bp).lmfs_dev == libs::libminixfs::constants::NO_DEV {
            lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
            return NO_BLOCK;
        }
        let mindex = excess as u32;
        b = rd_indir(bp, mindex as usize);
        lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
    }

    b
}

/// Read indirect block entry.
pub unsafe fn rd_indir(bp: *mut libs::libminixfs::types::Buf, index: usize) -> u32 {
    if bp.is_null() {
        return NO_BLOCK;
    }
    let ind = b_ind(bp);
    core::ptr::read_unaligned(&(*ind.add(index)))
}

/// read_ahead — read a block into the cache before it is needed.
pub unsafe fn read_ahead() {
    let rip = glo::RDAHED_INODE.load(Ordering::Relaxed);
    if rip.is_null() {
        return;
    }

    let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
    let rdahedpos = glo::RDAHEDPOS.load(Ordering::Relaxed);

    glo::RDAHED_INODE.store(core::ptr::null_mut(), Ordering::Relaxed); // turn off read-ahead

    let b = read_map(rip, rdahedpos, 1); // opportunistic = 1 (PREFETCH)
    if b == NO_BLOCK {
        return; // at EOF
    }

    let bp = lmfs_get_block_ino(
        (*rip).i_dev,
        b as u64,
        PREFETCH,
        (*rip).i_num as u64,
        rdahedpos & !(block_size - 1),
    );
    if !bp.is_null() {
        lmfs_put_block(bp, PARTIAL_DATA_BLOCK);
    }
}

/// rahead — read block with optional read-ahead.
pub unsafe fn rahead(
    rip: *mut Inode,
    baseblock: u32,
    position: u64,
    _bytes_ahead: u32,
) -> *mut libs::libminixfs::types::Buf {
    let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size as u64;
    let mut b = baseblock;

    // Read the current block
    let ino_off = position & !(block_size - 1);
    let bp = lmfs_get_block_ino((*rip).i_dev, b as u64, NORMAL, (*rip).i_num as u64, ino_off);

    if !bp.is_null() {
        // Try to read ahead by one block
        b = read_map(rip, position + block_size, 1); // opportunistic
        if b != NO_BLOCK {
            let ahead_off = (position + block_size) & !(block_size - 1);
            let ahead_bp = lmfs_get_block_ino(
                (*rip).i_dev,
                b as u64,
                PREFETCH,
                (*rip).i_num as u64,
                ahead_off,
            );
            if !ahead_bp.is_null() && (*ahead_bp).lmfs_dev != libs::libminixfs::constants::NO_DEV {
                // Got the read-ahead block — release immediately
                lmfs_put_block(ahead_bp, PARTIAL_DATA_BLOCK);
            } else if !ahead_bp.is_null() {
                lmfs_put_block(ahead_bp, PARTIAL_DATA_BLOCK);
            }
        }
    }

    bp
}
