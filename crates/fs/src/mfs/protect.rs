//! Permission checking and file attribute ops — adapted from `minix/fs/mfs/protect.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::read::*;
use crate::mfs::types::{DIR_ENTRY_SIZE, Direct};
use libs::libminixfs::cache::{lmfs_get_block, lmfs_put_block};

/// SAFECOPYTO kernel call offset (KERNEL_CALL + 32).
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
        match (*rip).i_sp.as_ref() {
            Some(sp) => {
                if sp.s_rd_only != 0 {
                    EROFS
                } else {
                    OK
                }
            }
            None => EROFS,
        }
    }
}

pub fn fs_chmod() -> i32 {
    EINVAL
}

pub fn fs_chown() -> i32 {
    EINVAL
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

        let block_size = (*rip_ref)
            .i_sp
            .as_ref()
            .map_or(0, |sp| sp.s_block_size as i64);
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
        buf_offset as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
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
    fn test_fs_chmod_returns_einval_when_uninitialized() {
        init();
        assert_eq!(fs_chmod(), EINVAL);
    }

    #[test]
    fn test_fs_chown_returns_einval_when_uninitialized() {
        init();
        assert_eq!(fs_chown(), EINVAL);
    }

    #[test]
    fn test_fs_getdents_returns_einval_when_no_inode() {
        init();
        assert_eq!(fs_getdents(), EINVAL);
    }
}
