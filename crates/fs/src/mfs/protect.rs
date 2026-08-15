//! Permission checking and file attribute ops — adapted from `minix/fs/mfs/protect.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::read::*;
use crate::mfs::types::{DIR_ENTRY_SIZE, Direct};
use libs::libminixfs::cache::{lmfs_get_block, lmfs_put_block};

/// SAFECOPYTO kernel call number (KERNEL_CALL + 32).
const SAFECOPYTO_CALL: i32 = 32;

pub fn forbidden(rip_idx: u16, access_desired: u16) -> i32 {
    unsafe {
        let rip = &*glo::get_inode_ptr(rip_idx as usize);
        let bits = (*rip).i_mode;
        let caller_uid = (*glo::mfs_ptr()).caller_uid;
        let caller_gid = (*glo::mfs_ptr()).caller_gid;

        let perm_bits = if caller_uid == SU_UID as u16 {
            let is_dir = (bits & I_TYPE) == I_DIRECTORY;
            let any_x = (bits & ((X_BIT << 6) | (X_BIT << 3) | X_BIT)) != 0;
            if is_dir || any_x {
                R_BIT | W_BIT | X_BIT
            } else {
                R_BIT | W_BIT
            }
        } else {
            let shift = if caller_uid == (*rip).i_uid {
                6
            } else if caller_gid == (*rip).i_gid {
                3
            } else {
                0
            };
            (bits >> shift) & (R_BIT | W_BIT | X_BIT)
        };

        let r = if (perm_bits | access_desired) != perm_bits {
            EACCES
        } else {
            OK
        };
        if r == OK && (access_desired & W_BIT) != 0 {
            let ro = read_only(rip_idx);
            if ro != OK {
                return ro;
            }
        }
        r
    }
}

pub fn read_only(rip_idx: u16) -> i32 {
    unsafe {
        let rip = &*glo::get_inode_ptr(rip_idx as usize);
        match (*rip).i_sp {
            Some(sp) if (*sp).s_rd_only != 0 => EROFS,
            Some(_) => OK,
            None => EROFS,
        }
    }
}

/// Change a file's mode bits (C `mfs/protect.c fs_chmod`).
///
/// Message layout (VFS `req_chmod`): inode (u32) at payload[0], mode
/// (u16) at payload[4]. Reply: mode (u16) at payload[0].
pub fn fs_chmod() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        let payload = &(*mfs).m_in.m_payload.raw;
        let ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
        let mode = u16::from_ne_bytes(payload[4..6].try_into().unwrap_or([0u8; 2]));
        let rip = match get_inode((*mfs).fs_dev, ino) {
            Some(i) => i,
            None => return EINVAL,
        };
        let r = read_only(rip);
        if r != OK {
            put_inode(Some(rip));
            return r;
        }
        let rip_ptr = glo::get_inode_ptr(rip as usize);
        // Replace the permission bits, keep the type bits (C: mode & ALL_MODES).
        (*rip_ptr).i_mode = ((*rip_ptr).i_mode & !ALL_MODES) | (mode & ALL_MODES);
        (*rip_ptr).i_update |= CTIME;
        (*rip_ptr).i_dirt = IN_DIRTY;
        let new_mode = (*rip_ptr).i_mode;
        let raw_ref = &mut (*mfs).m_out.m_payload.raw;
        raw_ref[0..2].copy_from_slice(&new_mode.to_le_bytes());
        put_inode(Some(rip));
        OK
    }
}

/// Change a file's owner (C `mfs/protect.c fs_chown`).
///
/// Message layout (VFS `req_chown`): inode (u32) at payload[0], uid
/// (u16) at payload[4], gid (u16) at payload[6]. Reply: mode (u16) at
/// payload[0].
pub fn fs_chown() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        let payload = &(*mfs).m_in.m_payload.raw;
        let ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
        let uid = u16::from_ne_bytes(payload[4..6].try_into().unwrap_or([0u8; 2]));
        let gid = u16::from_ne_bytes(payload[6..8].try_into().unwrap_or([0u8; 2]));
        let rip = match get_inode((*mfs).fs_dev, ino) {
            Some(i) => i,
            None => return EINVAL,
        };
        let rip_ptr = glo::get_inode_ptr(rip as usize);
        let r = read_only(rip);
        if r == OK {
            (*rip_ptr).i_uid = uid;
            (*rip_ptr).i_gid = gid;
            (*rip_ptr).i_mode &= !(I_SET_UID_BIT | I_SET_GID_BIT);
            (*rip_ptr).i_update |= CTIME;
            (*rip_ptr).i_dirt = IN_DIRTY;
        }
        // Reply mode — C always reports it, changed or not.
        let raw_ref = &mut (*mfs).m_out.m_payload.raw;
        raw_ref[0..2].copy_from_slice(&(*rip_ptr).i_mode.to_le_bytes());
        put_inode(Some(rip));
        r
    }
}

pub fn fs_getdents() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();

        // Read parameters from the incoming message raw payload.
        // VFS req_getdents writes:
        //   msg[8..12]  = inode_nr (u32)
        //   msg[16..24] = seek_pos (i64)
        //   msg[24..28] = grant_id (i32)
        //   msg[32..40] = mem_size (u64)
        // After kernel delivery, these land in m_payload.raw at:
        //   raw[0..4]   = inode_nr
        //   raw[8..16]  = seek_pos
        //   raw[16..20] = grant_id
        //   raw[24..32] = mem_size
        let payload = &(*mfs).m_in.m_payload.raw;
        let ino = u32::from_ne_bytes(payload[0..4].try_into().unwrap_or([0u8; 4]));
        let mut pos = i64::from_ne_bytes(payload[8..16].try_into().unwrap_or([0u8; 8]));
        let grant_id = i32::from_ne_bytes(payload[16..20].try_into().unwrap_or([0u8; 4]));
        let _mem_size = u64::from_ne_bytes(payload[24..32].try_into().unwrap_or([0u8; 8]));
        let dev = (*mfs).fs_dev;

        let rip = match get_inode(dev, ino) {
            Some(r) => r,
            None => return EINVAL,
        };

        // Load inode data from disk if not already loaded.
        let rip_ptr = glo::get_inode_ptr(rip as usize);
        if (*rip_ptr).i_size == 0 && (*rip_ptr).i_mode == 0 {
            let r = rw_inode(rip, READING);
            if r != 0 {
                return r;
            }
        }

        let rip_ref = &*glo::get_inode_ptr(rip as usize);
        let dir_size = (*rip_ref).i_size as i64;
        if pos < 0 || pos >= dir_size {
            return 0;
        }

        let sp = crate::mfs::super_block::get_super(dev);
        if sp.is_null() {
            return EINVAL;
        }
        let block_size = (*sp).s_block_size as i64;
        if block_size == 0 {
            return EINVAL;
        }

        let entries_per_block = block_size as usize / DIR_ENTRY_SIZE;
        let mut buf_offset: usize = 0;
        let user_path = &mut (*mfs).user_path;
        let max_buf = user_path.len();

        while pos < dir_size && buf_offset + 13 <= max_buf {
            let block_num = pos / block_size;
            let block_start = block_num * block_size;

            let b = read_map(rip, block_start, 0);
            if b == NO_BLOCK {
                pos = block_start + block_size;
                continue;
            }

            let bp = lmfs_get_block(dev, b as u64);
            if bp.is_null() {
                return EIO;
            }

            let data = (*bp).data_ptr as *const Direct;
            let offset_in_block = (pos - block_start) as usize;
            let start_entry = offset_in_block / DIR_ENTRY_SIZE;

            for i in start_entry..entries_per_block {
                let entry = &*data.add(i);
                if (*entry).mfs_d_ino == NO_ENTRY {
                    pos = block_start + (i as i64 + 1) * DIR_ENTRY_SIZE as i64;
                    continue;
                }

                // Find name length (up to null terminator)
                let name_slice = &(*entry).mfs_d_name;
                let namlen = name_slice
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(MFS_NAME_MAX - 1)
                    .min(MFS_NAME_MAX - 1);
                if namlen == 0 {
                    pos = block_start + (i as i64 + 1) * DIR_ENTRY_SIZE as i64;
                    continue;
                }

                // Compute struct dirent reclen (padded to 4 bytes):
                // d_fileno(8) + d_reclen(2) + d_namlen(2) + d_type(1) + d_name(namlen) + null
                let raw_size = 13 + namlen + 1;
                let reclen = ((raw_size + 3) & !3) as u16;

                if buf_offset + reclen as usize > max_buf {
                    break;
                }

                // d_fileno: u64 at offset 0
                let fileno = (*entry).mfs_d_ino as u64;
                user_path[buf_offset..buf_offset + 8].copy_from_slice(&fileno.to_le_bytes());
                // d_reclen: u16 at offset 8
                user_path[buf_offset + 8..buf_offset + 10].copy_from_slice(&reclen.to_le_bytes());
                // d_namlen: u16 at offset 10
                let namlen_u16 = namlen as u16;
                user_path[buf_offset + 10..buf_offset + 12]
                    .copy_from_slice(&namlen_u16.to_le_bytes());
                // d_type: u8 at offset 12
                user_path[buf_offset + 12] = 0; // DT_UNKNOWN
                // d_name at offset 13
                user_path[buf_offset + 13..buf_offset + 13 + namlen]
                    .copy_from_slice(&name_slice[..namlen]);

                buf_offset += reclen as usize;
                pos = block_start + (i as i64 + 1) * DIR_ENTRY_SIZE as i64;
            }

            lmfs_put_block(bp, DIRECTORY_BLOCK);

            if pos >= dir_size || (pos / block_size) != block_num {
                continue;
            }
        }

        // Copy directory entries through the grant to the user's buffer.
        if buf_offset > 0 && grant_id >= 0 {
            let mut kmsg = [0u8; 64];
            kmsg[8..12].copy_from_slice(&arch_common::com::VFS_PROC_NR.to_le_bytes());
            kmsg[12..16].copy_from_slice(&grant_id.to_le_bytes());
            kmsg[16..24].copy_from_slice(&0i64.to_le_bytes());
            let local_addr = (*mfs).user_path.as_ptr() as u64;
            kmsg[24..32].copy_from_slice(&local_addr.to_le_bytes());
            kmsg[32..40].copy_from_slice(&(buf_offset as u64).to_le_bytes());
            let r = minix_rt::kernel_call(SAFECOPYTO_CALL, &mut kmsg);
            if r != 0 {
                return r;
            }
        }

        (*mfs).cch[0] = buf_offset as i32;
        // Set reply payload:
        //   raw[0..8]  = new seek_pos
        //   raw[8..12] = buf_offset (bytes written via grant)
        let raw_ref = &mut (*mfs).m_out.m_payload.raw;
        raw_ref[0..8].copy_from_slice(&pos.to_le_bytes());
        raw_ref[8..12].copy_from_slice(&(buf_offset as i32).to_le_bytes());
        OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate
            // (mfs_init_globals only resets MFS_STORAGE, not these
            // separate static mut variables).
            *crate::mfs::glo::UNUSED_INODES_HEAD.get() = Some(0);
            let p = crate::mfs::glo::HASH_INODES.get();
            for i in 0..crate::mfs::consts::INODE_HASH_SIZE {
                let elem = core::ptr::addr_of_mut!((*p)[i]);
                elem.write(None);
            }
        }
    }

    /// Build a chown request message for inode `ino`, owner `uid`/`gid`.
    fn set_chown_req(ino: u32, uid: u16, gid: u16) {
        unsafe {
            let mfs = glo::mfs_ptr();
            let raw = &mut (*mfs).m_in.m_payload.raw;
            raw[0..4].copy_from_slice(&ino.to_le_bytes());
            raw[4..6].copy_from_slice(&uid.to_le_bytes());
            raw[6..8].copy_from_slice(&gid.to_le_bytes());
        }
    }

    /// Build a chmod request message for inode `ino`, mode `mode`.
    fn set_chmod_req(ino: u32, mode: u16) {
        unsafe {
            let mfs = glo::mfs_ptr();
            let raw = &mut (*mfs).m_in.m_payload.raw;
            raw[0..4].copy_from_slice(&ino.to_le_bytes());
            raw[4..6].copy_from_slice(&mode.to_le_bytes());
        }
    }

    #[test]
    fn test_read_only_no_super_returns_erofs() {
        init();
        assert_eq!(read_only(0), EROFS);
    }

    #[test]
    fn test_forbidden_default_inode_returns_ok() {
        init();
        assert_eq!(forbidden(0, 0), OK);
    }

    #[test]
    fn test_fs_chmod_read_only_inode_returns_erofs() {
        // An inode with no super block reference is on a read-only fs
        // (C: rip->i_sp->s_rd_only). The mode is never touched.
        init();
        unsafe {
            let mfs = glo::mfs_ptr();
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_mode = 0o755;
            set_chmod_req(7, 0o600);
            let r = fs_chmod();
            assert_eq!(r, EROFS);
            assert_eq!((*rip).i_mode, 0o755, "mode unchanged on r/o fs");
        }
    }

    #[test]
    fn test_fs_chmod_sets_mode_and_keeps_type_bits() {
        init();
        unsafe {
            let mfs = glo::mfs_ptr();
            let sp = glo::get_super_ptr(0);
            (*sp).s_rd_only = 0;
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_mode = I_DIRECTORY | 0o755;
            (*rip).i_sp = Some(sp);

            set_chmod_req(7, 0o600);
            let r = fs_chmod();
            assert_eq!(r, OK);
            assert_eq!(
                (*rip).i_mode,
                I_DIRECTORY | 0o600,
                "type bits survive, permission bits replaced"
            );
            assert_ne!((*rip).i_update & CTIME, 0, "ctime update flagged");
            assert_eq!((*rip).i_dirt, IN_DIRTY, "inode marked dirty");
            let reply_raw = (*mfs).m_out.m_payload.raw;
            let reply_mode = u16::from_ne_bytes(reply_raw[0..2].try_into().unwrap_or([0; 2]));
            assert_eq!(reply_mode, (*rip).i_mode, "reply carries new mode");
        }
    }

    #[test]
    fn test_fs_chmod_sets_and_clears_setuid_bit() {
        // J5: the setuid/setgid bits live inside ALL_MODES, so chmod can
        // set them (0o4755) and clear them (0o755) — they survive the
        // round-trip exactly as requested.
        init();
        unsafe {
            let mfs = glo::mfs_ptr();
            let sp = glo::get_super_ptr(0);
            (*sp).s_rd_only = 0;
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_mode = I_REGULAR | 0o755;
            (*rip).i_sp = Some(sp);

            set_chmod_req(7, 0o4755);
            assert_eq!(fs_chmod(), OK);
            assert_eq!((*rip).i_mode, I_REGULAR | 0o4755, "setuid bit set");

            set_chmod_req(7, 0o755);
            assert_eq!(fs_chmod(), OK);
            assert_eq!((*rip).i_mode, I_REGULAR | 0o755, "setuid bit cleared");
        }
    }

    #[test]
    fn test_fs_chown_read_only_inode_returns_erofs() {
        // An inode with no super block reference is on a read-only fs;
        // ownership must not change.
        init();
        unsafe {
            let mfs = glo::mfs_ptr();
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_uid = 100;
            (*rip).i_gid = 50;
            set_chown_req(7, 200, 60);
            let r = fs_chown();
            assert_eq!(r, EROFS);
            assert_eq!((*rip).i_uid, 100, "owner unchanged on r/o fs");
            assert_eq!((*rip).i_gid, 50);
        }
    }

    #[test]
    fn test_fs_chown_sets_owner_and_clears_setuid_bits() {
        init();
        unsafe {
            let mfs = glo::mfs_ptr();
            let sp = glo::get_super_ptr(0);
            (*sp).s_rd_only = 0;
            let ino = get_inode((*mfs).fs_dev, 7).expect("inode alloc");
            let rip = glo::get_inode_ptr(ino as usize);
            (*rip).i_mode = 0o4755; // setuid + rwxr-xr-x
            (*rip).i_uid = 100;
            (*rip).i_gid = 50;
            (*rip).i_sp = Some(sp);

            set_chown_req(7, 200, 60);
            let r = fs_chown();
            assert_eq!(r, OK);
            assert_eq!((*rip).i_uid, 200, "owner uid changes");
            assert_eq!((*rip).i_gid, 60, "owner gid changes");
            assert_eq!(
                (*rip).i_mode & (I_SET_UID_BIT | I_SET_GID_BIT),
                0,
                "setuid/setgid bits cleared by chown (C)"
            );
            assert_eq!((*rip).i_mode & 0o777, 0o755, "perm bits untouched");
            assert_ne!((*rip).i_update & CTIME, 0, "ctime update flagged");
            assert_eq!((*rip).i_dirt, IN_DIRTY, "inode marked dirty");
            let reply_raw = (*mfs).m_out.m_payload.raw;
            let reply_mode = u16::from_ne_bytes(reply_raw[0..2].try_into().unwrap_or([0; 2]));
            assert_eq!(reply_mode, (*rip).i_mode, "reply carries new mode");
        }
    }

    #[test]
    fn test_fs_getdents_returns_einval_when_no_inode() {
        init();
        assert_eq!(fs_getdents(), EINVAL);
    }
}
