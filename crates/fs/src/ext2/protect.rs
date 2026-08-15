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
///
/// Message layout (VFS `req_chmod`): inode (u32) at payload[0], mode
/// (u16) at payload[4]. Reply: mode (u16) at payload[0].
pub unsafe fn fs_chmod() -> i32 {
    let ext2 = glo::ext2_ptr();
    let payload = &(*ext2).m_in.m_payload.raw;
    let ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
    let mode = u16::from_ne_bytes(payload[4..6].try_into().unwrap_or([0u8; 2]));

    let rip = get_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    let r = read_only(rip);
    if r != OK {
        put_inode(rip);
        return r;
    }

    // Replace the permission bits, keep the type bits (C: mode & ALL_MODES).
    (*rip).i_mode = ((*rip).i_mode & !ALL_MODES) | (mode & ALL_MODES);
    (*rip).i_update |= CTIME;
    (*rip).i_dirt = IN_DIRTY;

    let new_mode = (*rip).i_mode;
    let raw_ref = &mut (*ext2).m_out.m_payload.raw;
    raw_ref[0..2].copy_from_slice(&new_mode.to_le_bytes());

    put_inode(rip);
    OK
}

/// fs_chown — change file owner.
///
/// Message layout (VFS `req_chown`): inode (u32) at payload[0], uid
/// (u16) at payload[4], gid (u16) at payload[6]. Reply: mode (u16) at
/// payload[0].
pub unsafe fn fs_chown() -> i32 {
    let ext2 = glo::ext2_ptr();
    let payload = &(*ext2).m_in.m_payload.raw;
    let ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
    let uid = u16::from_ne_bytes(payload[4..6].try_into().unwrap_or([0u8; 2]));
    let gid = u16::from_ne_bytes(payload[6..8].try_into().unwrap_or([0u8; 2]));

    let rip = get_inode((*ext2).fs_dev, ino);
    if rip.is_null() {
        return EINVAL;
    }

    let r = read_only(rip);
    if r == OK {
        (*rip).i_uid = uid;
        (*rip).i_gid = gid;
        (*rip).i_mode &= !(I_SET_UID_BIT | I_SET_GID_BIT);
        (*rip).i_update |= CTIME;
        (*rip).i_dirt = IN_DIRTY;
    }

    // Reply mode — C always reports it, changed or not.
    let raw_ref = &mut (*ext2).m_out.m_payload.raw;
    raw_ref[0..2].copy_from_slice(&(*rip).i_mode.to_le_bytes());

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
            let d_rec_len =
                core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_rec_len)) as usize;
            let d_name_len =
                core::ptr::read_unaligned(core::ptr::addr_of!((*dp).d_name_len)) as usize;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::ext2_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate.
            crate::ext2::inode::init_inode_cache();
        }
    }

    fn set_chown_req(ino: u32, uid: u16, gid: u16) {
        unsafe {
            let ext2 = glo::ext2_ptr();
            let raw = &mut (*ext2).m_in.m_payload.raw;
            raw[0..4].copy_from_slice(&ino.to_le_bytes());
            raw[4..6].copy_from_slice(&uid.to_le_bytes());
            raw[6..8].copy_from_slice(&gid.to_le_bytes());
        }
    }

    fn set_chmod_req(ino: u32, mode: u16) {
        unsafe {
            let ext2 = glo::ext2_ptr();
            let raw = &mut (*ext2).m_in.m_payload.raw;
            raw[0..4].copy_from_slice(&ino.to_le_bytes());
            raw[4..6].copy_from_slice(&mode.to_le_bytes());
        }
    }

    #[test]
    fn test_fs_chmod_sets_mode_and_keeps_type_bits() {
        init();
        unsafe {
            let ext2 = glo::ext2_ptr();
            let ino = get_inode((*ext2).fs_dev, 7);
            assert!(!ino.is_null(), "inode alloc");
            (*ino).i_mode = I_DIRECTORY | 0o755;

            set_chmod_req(7, 0o600);
            let r = fs_chmod();
            assert_eq!(r, OK);
            assert_eq!(
                (*ino).i_mode,
                I_DIRECTORY | 0o600,
                "type bits survive, permission bits replaced"
            );
            assert_ne!((*ino).i_update & CTIME, 0, "ctime update flagged");
            assert_eq!((*ino).i_dirt, IN_DIRTY, "inode marked dirty");
            let reply_raw = (*ext2).m_out.m_payload.raw;
            let reply_mode = u16::from_ne_bytes(reply_raw[0..2].try_into().unwrap_or([0; 2]));
            assert_eq!(reply_mode, (*ino).i_mode, "reply carries new mode");
        }
    }

    #[test]
    fn test_fs_chown_sets_owner_and_clears_setuid_bits() {
        init();
        unsafe {
            let ext2 = glo::ext2_ptr();
            let ino = get_inode((*ext2).fs_dev, 7);
            assert!(!ino.is_null(), "inode alloc");
            (*ino).i_mode = 0o4755; // setuid + rwxr-xr-x
            (*ino).i_uid = 100;
            (*ino).i_gid = 50;

            set_chown_req(7, 200, 60);
            let r = fs_chown();
            assert_eq!(r, OK);
            assert_eq!((*ino).i_uid, 200, "owner uid changes");
            assert_eq!((*ino).i_gid, 60, "owner gid changes");
            assert_eq!(
                (*ino).i_mode & (I_SET_UID_BIT | I_SET_GID_BIT),
                0,
                "setuid/setgid bits cleared by chown (C)"
            );
            assert_eq!((*ino).i_mode & 0o777, 0o755, "perm bits untouched");
            assert_ne!((*ino).i_update & CTIME, 0, "ctime update flagged");
            assert_eq!((*ino).i_dirt, IN_DIRTY, "inode marked dirty");
            let reply_raw = (*ext2).m_out.m_payload.raw;
            let reply_mode = u16::from_ne_bytes(reply_raw[0..2].try_into().unwrap_or([0; 2]));
            assert_eq!(reply_mode, (*ino).i_mode, "reply carries new mode");
        }
    }
}
