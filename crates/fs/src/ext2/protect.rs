//! Permission checks, chmod/chown, getdents — adapted from `minix/fs/ext2/protect.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;

/// fs_chmod — change file mode.
pub unsafe fn fs_chmod() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message, get inode, set mode
    let _ = ext2;
    EINVAL
}

/// fs_chown — change file owner.
pub unsafe fn fs_chown() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message, get inode, set uid/gid
    let _ = ext2;
    EINVAL
}

/// fs_getdents — get directory entries.
pub unsafe fn fs_getdents() -> i32 {
    let ext2 = glo::ext2_ptr();
    // TODO: parse message, iterate directory, copy entries
    let _ = ext2;
    EINVAL
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
