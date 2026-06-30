//! Permission checks, chmod/chown, getdents — adapted from `minix/fs/ext2/protect.c`

use libs::libminixfs::cache::{lmfs_get_block_ino, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::{DIRECTORY_BLOCK, NORMAL, VMC_NO_INODE};

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::read::*;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_chmod — change file mode.
pub unsafe fn fs_chmod() -> i32 {
    let ext2 = glo::ext2_ptr();

    let ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing
    let mode = ((*ext2).caller_uid as u16) as u16; // FIXME: parse mode from message

    let rip = get_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    (*rip).i_mode = ((*rip).i_mode & !ALL_MODES) | (mode & ALL_MODES);
    (*rip).i_update |= CTIME;
    (*rip).i_dirt = IN_DIRTY;

    // FIXME: set reply mode

    put_inode(rip);
    OK
}

/// fs_chown — change file owner.
pub unsafe fn fs_chown() -> i32 {
    let ext2 = glo::ext2_ptr();

    let ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing

    let rip = get_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    let r = read_only(rip);
    if r == OK {
        (*rip).i_uid = (*ext2).caller_uid; // FIXME: parse uid from message
        (*rip).i_gid = (*ext2).caller_gid; // FIXME: parse gid from message
        (*rip).i_mode &= !(I_SET_UID_BIT | I_SET_GID_BIT);
        (*rip).i_update |= CTIME;
        (*rip).i_dirt = IN_DIRTY;
    }

    // FIXME: set reply mode = rip->i_mode

    put_inode(rip);
    r
}

/// fs_getdents — get directory entries.
pub unsafe fn fs_getdents() -> i32 {
    let ext2 = glo::ext2_ptr();

    let ino = (*ext2).fs_m_in_type as u32; // FIXME: proper message parsing
    let _size: usize = 0; // FIXME: parse mem_size from message
    let _pos: u64 = 0; // FIXME: parse seek_pos from message

    let rip = get_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    let block_size = (*(*rip).i_sp.as_ref().unwrap()).s_block_size;
    let file_size = (*rip).i_size as u64;

    let mut block_pos: u64 = 0;

    // Iterate directory blocks
    while block_pos < file_size {
        let b = read_map(rip, block_pos, 0);
        if b == NO_BLOCK {
            block_pos += block_size as u64;
            continue;
        }

        let bp = lmfs_get_block_ino(
            (*rip).i_dev,
            b as u64,
            NORMAL,
            (*rip).i_num as u64,
            block_pos,
        );
        if bp.is_null() {
            block_pos += block_size as u64;
            continue;
        }

        let data = b_data(bp);
        let data_end = data.wrapping_add(block_size as usize);
        let mut dp = data as *mut Ext2DiskDirDesc;

        while (dp as usize) < (data_end as usize) {
            let d_ino = core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_ino));
            let d_rec_len = core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_rec_len)) as usize;
            let d_name_len = core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_name_len)) as usize;

            if d_rec_len == 0 || (dp as usize) + d_rec_len > (data_end as usize) {
                break;
            }

            if d_ino != 0 && d_name_len <= EXT2_NAME_MAX {
                // FIXME: copy entry to user buffer via grant
                // Each entry: d_ino (u32), d_rec_len (u16), d_name_len (u8),
                // d_file_type (u8), d_name[d_name_len]
                let _ = &(*dp).d_name;
            }

            dp = (dp as *mut u8).wrapping_add(d_rec_len) as *mut Ext2DiskDirDesc;
        }

        lmfs_put_block(bp, DIRECTORY_BLOCK);
        block_pos += block_size as u64;
    }

    (*rip).i_update |= ATIME;
    (*rip).i_dirt = IN_DIRTY;

    put_inode(rip);
    OK
}

/// forbidden — check if access is allowed.
pub unsafe fn forbidden(rip: *mut Inode, access_desired: u16) -> i32 {
    let ext2 = glo::ext2_ptr();
    let bits = (*rip).i_mode;
    let caller_uid = (*ext2).caller_uid;
    let caller_gid = (*ext2).caller_gid;

    let perm_bits: u16;
    if caller_uid as u32 == SU_UID {
        if (bits & I_TYPE) == I_DIRECTORY || (bits & ((X_BIT << 6) | (X_BIT << 3) | X_BIT)) != 0 {
            perm_bits = R_BIT | W_BIT | X_BIT;
        } else {
            perm_bits = R_BIT | W_BIT;
        }
    } else {
        let shift = if caller_uid == (*rip).i_uid {
            6
        } else if caller_gid == (*rip).i_gid {
            3
        } else {
            // Check supplementary groups
            let mut in_grp = false;
            // TODO: check credentials.vu_sgroups
            if in_grp { 3 } else { 0 }
        };
        perm_bits = (bits >> shift) & (R_BIT | W_BIT | X_BIT);
    }

    let mut r = OK;
    if (perm_bits | access_desired) != perm_bits {
        r = EACCES;
    }

    if r == OK && (access_desired & W_BIT) != 0 {
        r = read_only(rip);
    }

    r
}

/// read_only — check if file system is read-only.
pub unsafe fn read_only(ip: *mut Inode) -> i32 {
    if let Some(ref sp) = (*ip).i_sp {
        if sp.s_rd_only != 0 {
            return EROFS;
        }
    }
    OK
}
