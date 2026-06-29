//! Permission checking and file attribute ops — adapted from `minix/fs/mfs/protect.c`

use crate::mfs::consts::*;
use crate::mfs::glo;

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
    // TODO: read inode_nr and mode from IPC message
    // Currently returns EINVAL to avoid silently corrupting inode 0.
    EINVAL
}

pub fn fs_chown() -> i32 {
    // TODO: read inode_nr, uid, gid from IPC message
    // Currently returns EINVAL to avoid silently corrupting inode 0.
    EINVAL
}

pub fn fs_getdents() -> i32 {
    todo!("fs_getdents: not yet wired")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate.
            *crate::mfs::glo::UNUSED_INODES_HEAD.get() = None;
            let p = crate::mfs::glo::HASH_INODES.get();
            for i in 0..crate::mfs::consts::INODE_HASH_SIZE {
                let elem = core::ptr::addr_of_mut!((*p)[i]);
                elem.write(None);
            }
        }
    }

    #[test]
    fn test_read_only_no_super_returns_erofs() {
        // After init, inode_table[0].i_sp is None → read_only returns EROFS.
        init();
        assert_eq!(read_only(0), EROFS);
    }

    #[test]
    fn test_forbidden_default_inode_returns_ok() {
        // After init, inode_table[0].i_mode == 0, caller_uid == INVAL_UID
        // (not SU_UID), and neither uid nor gid matches, so shift = 0,
        // perm_bits = 0, and (0 | 0) == 0 → OK is returned.
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
    #[should_panic(expected = "not yet wired")]
    fn test_fs_getdents_panics() {
        fs_getdents();
    }
}
