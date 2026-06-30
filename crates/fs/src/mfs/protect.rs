//! Permission checking and file attribute ops — adapted from `minix/fs/mfs/protect.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::read::*;
use crate::mfs::types::{DIR_ENTRY_SIZE, Direct};
use libs::libminixfs::cache::{lmfs_get_block, lmfs_put_block};

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
        let ino = (*glo::mfs_ptr()).cch[0] as u32;
        let mut pos = (*glo::mfs_ptr()).cch[1] as i64;
        let dev = (*glo::mfs_ptr()).fs_dev;

        let rip = match find_inode(dev, ino) {
            Some(r) => r,
            None => return EINVAL,
        };

        let rip_ref = &*glo::get_inode_ptr(rip as usize);
        let dir_size = (*rip_ref).i_size as i64;
        if pos < 0 || pos >= dir_size {
            return OK;
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

        while pos < dir_size {
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
                    continue;
                }

                let dst = &mut (*glo::mfs_ptr()).user_path;
                if buf_offset + DIR_ENTRY_SIZE <= dst.len() {
                    let dst_entry = &mut *(dst.as_mut_ptr().add(buf_offset) as *mut Direct);
                    *dst_entry = *entry;
                }
                buf_offset += DIR_ENTRY_SIZE;
                pos = block_start + (i as i64 + 1) * DIR_ENTRY_SIZE as i64;
            }

            lmfs_put_block(bp, DIRECTORY_BLOCK);

            if pos >= dir_size || (pos / block_size) != block_num {
                continue;
            }
        }

        (*glo::mfs_ptr()).cch[0] = buf_offset as i32;
        OK
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
