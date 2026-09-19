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

/// Kernel call numbers for the grant-based copies (KERNEL_CALL + n).
#[cfg(target_os = "minix")]
const SAFECOPYTO_CALL: i32 = 32;
#[cfg(target_os = "minix")]
const SAFECOPYFROM_CALL: i32 = 31;

/// Copy `len` bytes from `src` into the grant `gid` created by VFS, `buf_off`
/// bytes into that grant (C: sys_safecopyto).
#[cfg(target_os = "minix")]
pub(crate) unsafe fn safecopy_to_grant(gid: i32, buf_off: u64, src: *const u8, len: usize) -> i32 {
    let mut kmsg = [0u8; 64];
    kmsg[8..12].copy_from_slice(&arch_common::com::VFS_PROC_NR.to_le_bytes());
    kmsg[12..16].copy_from_slice(&gid.to_le_bytes());
    kmsg[16..24].copy_from_slice(&buf_off.to_le_bytes());
    kmsg[24..32].copy_from_slice(&(src as u64).to_le_bytes());
    kmsg[32..40].copy_from_slice(&(len as u64).to_le_bytes());
    minix_rt::kernel_call(SAFECOPYTO_CALL, &mut kmsg)
}

/// Copy `len` bytes from the grant `gid` created by VFS, `buf_off` bytes into
/// that grant, into `dst` (C: sys_safecopyfrom).
#[cfg(target_os = "minix")]
pub(crate) unsafe fn safecopy_from_grant(gid: i32, buf_off: u64, dst: *mut u8, len: usize) -> i32 {
    let mut kmsg = [0u8; 64];
    kmsg[8..12].copy_from_slice(&arch_common::com::VFS_PROC_NR.to_le_bytes());
    kmsg[12..16].copy_from_slice(&gid.to_le_bytes());
    kmsg[16..24].copy_from_slice(&buf_off.to_le_bytes());
    kmsg[24..32].copy_from_slice(&(dst as u64).to_le_bytes());
    kmsg[32..40].copy_from_slice(&(len as u64).to_le_bytes());
    minix_rt::kernel_call(SAFECOPYFROM_CALL, &mut kmsg)
}

/// fs_readwrite — read/write dispatch.
pub unsafe fn fs_readwrite() -> i32 {
    let ext2 = glo::ext2_ptr();

    // VFS req_readwrite payload (raw offsets):
    //   raw[0..4]   = inode (u32)
    //   raw[8..16]  = seek_pos (i64)
    //   raw[16..20] = grant (i32)
    //   raw[24..32] = nbytes (u64)
    let payload = (*ext2).m_in.m_payload.raw;
    let ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
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
        // An inode whose superblock is not mounted cannot be sized or read.
        match (*rip).i_sp {
            Some(ref sp) => block_size = sp.s_block_size as u64,
            None => return EINVAL,
        }
        f_size = (*rip).i_size as u64;
    }

    let rw_flag = match (*ext2).req_nr + FS_BASE {
        REQ_READ => READING,
        REQ_WRITE => WRITING,
        REQ_PEEK => PEEKING,
        _ => return EINVAL,
    };
    let gid = i32::from_ne_bytes(payload[16..20].try_into().unwrap_or([0u8; 4]));
    let mut position = i64::from_ne_bytes(payload[8..16].try_into().unwrap_or([0u8; 8])) as u64;
    let mut nrbytes = u64::from_ne_bytes(payload[24..32].try_into().unwrap_or([0u8; 8])) as usize;

    (*ext2).rdwt_err = OK;

    if rw_flag == WRITING && !block_spec {
        let max_size = match (*rip).i_sp {
            Some(ref sp) => sp.s_max_size,
            None => return EINVAL,
        };
        if position > max_size.saturating_sub(nrbytes as u64) {
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
            gid,
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

    // Reply fields VFS reads back (req_readwrite): seek_pos, then nbytes.
    let raw = &mut (*ext2).m_out.m_payload.raw;
    raw[0..8].copy_from_slice(&(position as i64).to_le_bytes());
    raw[8..12].copy_from_slice(&(cum_io as u32).to_le_bytes());

    OK
}

/// fs_breadwrite — raw block I/O on a device, without an inode.
///
/// VFS routes a block special file's reads and writes here (through the FS
/// that serves the device), because the bytes are addressed on the device, not
/// in a file. Message layout (VFS `req_breadwrite`): device (u32) at payload[0],
/// seek_pos (i64) at payload[8], grant (i32) at payload[16], nbytes (u64) at
/// payload[24]. Reply: seek_pos (i64) at payload[0], nbytes (u64) at payload[8].
///
/// Reference: read.c fs_breadwrite()
pub unsafe fn fs_breadwrite() -> i32 {
    let ext2 = glo::ext2_ptr();
    let payload = (*ext2).m_in.m_payload.raw;

    let rw_flag = if (*ext2).m_in.m_type == REQ_BREAD {
        READING
    } else {
        WRITING
    };
    let dev = payload_u32(&payload, 0);
    let gid = payload_i32(&payload, 16);
    let mut nrbytes = payload_u64(&payload, 24) as usize;
    let mut position = payload_i64(&payload, 8) as u64;

    // The C takes the block size from the cache (`get_block_size()` there is
    // `lmfs_fs_block_size()`), which is also the unit the cache addresses blocks
    // in — and covers a device this server has not mounted, since VFS routes raw
    // I/O to the FS responsible for the device.
    let block_size = libs::libminixfs::cache::lmfs_fs_block_size() as u64;
    if block_size == 0 {
        return EINVAL;
    }

    // A pseudo inode for rw_chunk: the device lives in i_block[0], exactly the
    // way a block special inode stores it, and there is no inode behind it.
    let mut block = [0u32; EXT2_N_BLOCKS];
    block[0] = dev;
    let mut rip = Inode {
        i_block: block,
        i_mode: I_BLOCK_SPECIAL,
        i_dev: NO_DEV,
        i_num: VMC_NO_INODE as u32,
        i_size: 0,
        ..Inode::default()
    };

    (*ext2).rdwt_err = OK;

    let mut r = OK;
    let mut cum_io: u32 = 0;

    while nrbytes > 0 {
        let off = (position % block_size) as u32;
        let chunk = (nrbytes as u64).min(block_size - off as u64) as u32;

        r = rw_chunk(
            &mut rip,
            position,
            off,
            chunk,
            nrbytes as u32,
            rw_flag,
            gid,
            cum_io,
            block_size as u32,
        );
        if r != OK {
            break;
        }
        if (*ext2).rdwt_err < 0 {
            break;
        }

        nrbytes -= chunk as usize;
        cum_io += chunk;
        position += chunk as u64;
    }

    let raw = &mut (*ext2).m_out.m_payload.raw;
    raw[0..8].copy_from_slice(&(position as i64).to_le_bytes());
    raw[8..16].copy_from_slice(&(cum_io as u64).to_le_bytes());

    if (*ext2).rdwt_err != OK {
        r = (*ext2).rdwt_err;
    }
    if (*ext2).rdwt_err == END_OF_FILE {
        r = OK;
    }

    r
}

/// Read/Write one chunk (partial block).
pub unsafe fn rw_chunk(
    rip: *mut Inode,
    position: u64,
    off: u32,
    chunk: u32,
    _left: u32,
    rw_flag: i32,
    gid: i32,
    buf_off: u32,
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
            // Reading a hole reads as zeros (C: sys_safememset).
            #[cfg(target_os = "minix")]
            {
                const ZEROS: [u8; 4096] = [0u8; 4096];
                let mut done = 0usize;
                while done < chunk as usize {
                    let n = (chunk as usize - done).min(ZEROS.len());
                    r = safecopy_to_grant(gid, buf_off as u64 + done as u64, ZEROS.as_ptr(), n);
                    if r != OK {
                        return r;
                    }
                    done += n;
                }
            }
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
        let mut n = if chunk == block_size { NO_READ } else { NORMAL };
        if !block_spec && off == 0 && position >= (*rip).i_size as u64 {
            n = NO_READ;
        }
        bp = if block_spec {
            // A device block is numbered on the device itself and belongs to no
            // inode (C: get_block(dev, b, n)).
            lmfs_get_block_ino(dev, b as u64, n, VMC_NO_INODE, 0)
        } else {
            lmfs_get_block_ino(dev, b as u64, n, (*rip).i_num as u64, ino_off)
        };
    }

    if bp.is_null() {
        return EIO;
    }

    if rw_flag == WRITING {
        // The buffer was acquired without reading the block when the caller
        // covers a whole block, or writes past the end of the file: clear the
        // bytes it is not about to overwrite, so the rest of the block does not
        // expose whatever the buffer held before (C zero_block()).
        if chunk != block_size && !block_spec && off == 0 && position >= (*rip).i_size as u64 {
            core::ptr::write_bytes((*bp).data_ptr, 0, block_size as usize);
        }
        lmfs_markdirty(bp);
    }

    // Move the data between the caller's grant and the cached block.
    #[cfg(target_os = "minix")]
    {
        let data = (*bp).data_ptr;
        match rw_flag {
            READING => {
                r = safecopy_to_grant(gid, buf_off as u64, data.add(off as usize), chunk as usize);
            }
            WRITING => {
                r = safecopy_from_grant(
                    gid,
                    buf_off as u64,
                    data.add(off as usize),
                    chunk as usize,
                );
            }
            _ => {}
        }
    }
    #[cfg(not(target_os = "minix"))]
    let _ = (gid, buf_off);

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
        // Inodes read straight from disk without icopy() can still carry the
        // on-disk encoding of "no block", which is 0.
        let b = (*rip).i_block[block_pos as usize];
        return if b == 0 { NO_BLOCK } else { b };
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
///
/// On disk an unused entry is 0; `NO_BLOCK` is the in-memory sentinel, so the
/// two are translated here (and back in `wr_indir`).
pub unsafe fn rd_indir(bp: *mut libs::libminixfs::types::Buf, index: usize) -> u32 {
    if bp.is_null() {
        return NO_BLOCK;
    }
    let ind = b_ind(bp);
    let b = core::ptr::read_unaligned(&(*ind.add(index)));
    if b == 0 { NO_BLOCK } else { b }
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
///
/// A block special file is addressed directly on its device and carries no
/// superblock, so its block size comes from the device and no inode-based
/// block map is consulted (C rahead's `block_spec` branch; the raw-device
/// prefetch it does there is left out, so a device read fetches one block).
pub unsafe fn rahead(
    rip: *mut Inode,
    baseblock: u32,
    position: u64,
    _bytes_ahead: u32,
) -> *mut libs::libminixfs::types::Buf {
    let block_spec = ((*rip).i_mode & I_TYPE) == I_BLOCK_SPECIAL;
    let dev = if block_spec {
        (*rip).i_block[0]
    } else {
        (*rip).i_dev
    };
    let block_size = get_block_size(dev) as u64;

    let ino_off = position & !(block_size - 1);
    let bp = if block_spec {
        lmfs_get_block_ino(dev, baseblock as u64, NORMAL, VMC_NO_INODE, 0)
    } else {
        lmfs_get_block_ino(dev, baseblock as u64, NORMAL, (*rip).i_num as u64, ino_off)
    };

    if !bp.is_null() && !block_spec {
        // Try to read ahead by one block
        let b = read_map(rip, position + block_size, 1); // opportunistic
        if b != NO_BLOCK {
            let ahead_off = (position + block_size) & !(block_size - 1);
            let ahead_bp =
                lmfs_get_block_ino(dev, b as u64, PREFETCH, (*rip).i_num as u64, ahead_off);
            if !ahead_bp.is_null() {
                lmfs_put_block(ahead_bp, PARTIAL_DATA_BLOCK);
            }
        }
    }

    bp
}
