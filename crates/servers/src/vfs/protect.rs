//! VFS file permission and access checks — ported from `minix/servers/vfs/protect.c`.
//!
//! Implements POSIX file permission checking: `access()`, `forbidden()`,
//! `read_only()`, `in_group()`, and the owner/superuser guards for
//! `chmod()` and `chown()`.

use crate::vfs::consts::*;
use crate::vfs::types::*;

/// POSIX access() mode flags.
pub const R_OK: i32 = 0x04;
pub const W_OK: i32 = 0x02;
pub const X_OK: i32 = 0x01;
pub const F_OK: i32 = 0x00;

/// Single-bit permission masks (permissions for one user class).
pub const R_BIT: u32 = 0o4;
pub const W_BIT: u32 = 0o2;
pub const X_BIT: u32 = 0o1;
pub const ALL_MODES: u32 = 0o777;

/// Setgid bit — stripped when non-root caller chmods a file whose group
/// does not match the caller's effective gid.
pub const I_SET_GID_BIT: u32 = 0o2000;

/// Check whether the caller belongs to the given group, accounting for
/// both the primary (effective) gid and supplementary groups.
pub fn in_group(fp: &Fproc, grp: i32) -> bool {
    if fp.fp_effgid as i32 == grp {
        return true;
    }
    for i in 0..fp.fp_ngroups.min(NGROUPS_MAX as i32) as usize {
        if fp.fp_sgroups[i] as i32 == grp {
            return true;
        }
    }
    false
}

/// Returns `EROFS` if the vnode's mount point is read-only, `OK` otherwise.
pub fn read_only(vp: &Vnode) -> i32 {
    if vp.v_vmnt.is_null() {
        return OK;
    }
    let vmnt = unsafe { &*vp.v_vmnt };
    if vmnt.m_flags & VMNT_READONLY != 0 {
        EROFS
    } else {
        OK
    }
}

/// Check whether the calling process has the requested access to a file.
///
/// Resolves the caller's identity against the file's owner/group and mode
/// bits using the standard POSIX 3-tier (owner → group → other) fallback.
/// Superuser (uid 0) bypasses all checks except execute on non-directory
/// files with no execute bits set.
///
/// If `use_real_ids` is true, uses real uid/gid (for `access()`); otherwise
/// uses effective uid/gid (for all other operations).
///
/// Returns `OK` on success, `EACCES` on denial, `EROFS` on write to
/// read-only filesystem.
pub fn forbidden(fp: &Fproc, vp: &Vnode, access_desired: u32, use_real_ids: bool) -> i32 {
    let uid: i32 = if use_real_ids {
        fp.fp_realuid.into()
    } else {
        fp.fp_effuid.into()
    };
    let gid: i32 = if use_real_ids {
        fp.fp_realgid.into()
    } else {
        fp.fp_effgid.into()
    };

    // Guard: deny if vnode has sentinel uid/gid values (-1).
    if vp.v_uid == -1 || vp.v_gid == -1 {
        return EACCES;
    }

    // Superuser shortcut.
    if uid == SU_UID as i32 {
        let implied = if vp.v_mode & S_IFDIR != 0 {
            R_BIT | W_BIT | X_BIT
        } else if vp.v_mode & (0o1 | 0o10 | 0o100) != 0 {
            // Any execute bit set on the file → full access.
            R_BIT | W_BIT | X_BIT
        } else {
            R_BIT | W_BIT // no execute on non-directories without any x bit
        };
        if access_desired & !implied == 0 {
            return OK;
        }
    }

    // Determine which permission tier applies.
    let shift = if uid == vp.v_uid {
        6 // owner
    } else if gid == vp.v_gid || in_group(fp, vp.v_gid) {
        3 // group
    } else {
        0 // other
    };

    let perm_bits = (vp.v_mode >> shift) & ALL_MODES;

    if (perm_bits | access_desired) != perm_bits {
        return EACCES;
    }

    // Write check against read-only filesystem.
    if access_desired & W_BIT != 0 {
        let r = read_only(vp);
        if r != OK {
            return r;
        }
    }

    OK
}

/// Check `access(path, mode)` using the real uid/gid (POSIX semantics).
/// Called from `call.rs` after path resolution obtains the vnode.
///
/// # Safety
///
/// `vp` must point to a valid, initialized `Vnode`. `fp` must reference
/// the calling process's file context.
pub unsafe fn check_access(fp: &Fproc, vp: &Vnode, amode: u32) -> i32 {
    if amode == F_OK as u32 {
        return OK; // existence check only
    }
    forbidden(fp, vp, amode, true)
}

/// Check whether the caller is allowed to change the mode of `vp`.
/// Returns `OK` or `EPERM`.
pub fn chmod_allowed(fp: &Fproc, vp: &Vnode) -> i32 {
    if fp.fp_effuid as i32 != SU_UID as i32 && fp.fp_effuid as i32 != vp.v_uid {
        return EPERM;
    }
    OK
}

/// Strip the setgid bit if the caller is not root and does not belong to
/// the file's group.
pub fn chmod_strip_setgid(fp: &Fproc, vp: &Vnode, new_mode: &mut u32) {
    if fp.fp_effuid as i32 != SU_UID as i32 && !in_group(fp, vp.v_gid) {
        *new_mode &= !I_SET_GID_BIT;
    }
}

/// Check whether the caller may change the owner/group of `vp` to
/// (`new_uid`, `new_gid`).  Returns `OK` or `EPERM`.
///
/// Non-root callers must own the file, the new owner must be unchanged,
/// and the new group must match the caller's effective gid.
pub fn chown_allowed(fp: &Fproc, vp: &Vnode, new_uid: i32, new_gid: i32) -> i32 {
    if fp.fp_effuid as i32 == SU_UID as i32 {
        return OK;
    }
    if fp.fp_effuid as i32 != vp.v_uid {
        return EPERM;
    }
    // Not allowed to give the file away to another user.
    if new_uid != -1 && new_uid != vp.v_uid {
        return EPERM;
    }
    // Group must match caller's effective gid.
    if new_gid != -1 && !in_group(fp, new_gid) {
        return EPERM;
    }
    OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::types::Vnode;

    fn make_fproc(uid: i32, gid: i32) -> Fproc {
        let mut fp = Fproc::default();
        fp.fp_effuid = uid as u16;
        fp.fp_effgid = gid as u16;
        fp.fp_realuid = uid as u16;
        fp.fp_realgid = gid as u16;
        fp.fp_ngroups = 0;
        fp
    }

    fn make_vnode(uid: i32, gid: i32, mode: u32) -> Vnode {
        Vnode {
            v_uid: uid,
            v_gid: gid,
            v_mode: mode,
            ..Default::default()
        }
    }

    #[test]
    fn test_in_group_primary() {
        let fp = make_fproc(100, 50);
        assert!(in_group(&fp, 50));
    }

    #[test]
    fn test_in_group_supplementary() {
        let mut fp = make_fproc(100, 50);
        fp.fp_ngroups = 2;
        fp.fp_sgroups[0] = 30;
        fp.fp_sgroups[1] = 40;
        assert!(in_group(&fp, 40));
        assert!(!in_group(&fp, 99));
    }

    #[test]
    fn test_forbidden_owner_read() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(100, 50, 0o400); // owner-read only
        assert_eq!(forbidden(&fp, &vn, R_BIT, false), OK);
        assert_eq!(forbidden(&fp, &vn, W_BIT, false), EACCES);
    }

    #[test]
    fn test_forbidden_owner_write() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(100, 50, 0o200); // owner-write only
        assert_eq!(forbidden(&fp, &vn, W_BIT, false), OK);
        assert_eq!(forbidden(&fp, &vn, R_BIT, false), EACCES);
    }

    #[test]
    fn test_forbidden_owner_rwx() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(100, 50, 0o700);
        assert_eq!(forbidden(&fp, &vn, R_BIT | W_BIT | X_BIT, false), OK);
    }

    #[test]
    fn test_forbidden_group_read() {
        let fp = make_fproc(200, 50);
        let vn = make_vnode(100, 50, 0o040); // group-read only
        assert_eq!(forbidden(&fp, &vn, R_BIT, false), OK);
        assert_eq!(forbidden(&fp, &vn, W_BIT, false), EACCES);
    }

    #[test]
    fn test_forbidden_other_read() {
        let fp = make_fproc(200, 60);
        let vn = make_vnode(100, 50, 0o004); // other-read only
        assert_eq!(forbidden(&fp, &vn, R_BIT, false), OK);
        assert_eq!(forbidden(&fp, &vn, W_BIT, false), EACCES);
    }

    #[test]
    fn test_forbidden_superuser() {
        let fp = make_fproc(0, 0); // root
        let vn = make_vnode(100, 50, 0o000);
        assert_eq!(forbidden(&fp, &vn, R_BIT | W_BIT, false), OK);
    }

    #[test]
    fn test_forbidden_superuser_dir() {
        let fp = make_fproc(0, 0);
        let vn = make_vnode(100, 50, S_IFDIR | 0o000);
        assert_eq!(forbidden(&fp, &vn, R_BIT | W_BIT | X_BIT, false), OK);
    }

    #[test]
    fn test_forbidden_use_real_ids() {
        let mut fp = make_fproc(0, 0); // effective is root
        fp.fp_realuid = 200; // real is non-owner
        fp.fp_realgid = 60;
        let vn = make_vnode(100, 50, 0o004); // other-read
        assert_eq!(forbidden(&fp, &vn, R_BIT, false), OK); // effective=root → OK
        assert_eq!(forbidden(&fp, &vn, R_BIT, true), OK); // real=other → OK (other-read)
        assert_eq!(forbidden(&fp, &vn, W_BIT, true), EACCES); // real=other → no write
    }

    #[test]
    fn test_forbidden_invalid_vnode() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(-1, -1, 0o777); // sentinel uid/gid
        assert_eq!(forbidden(&fp, &vn, R_BIT, false), EACCES);
    }

    #[test]
    fn test_chmod_allowed_owner() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(100, 50, 0o644);
        assert_eq!(chmod_allowed(&fp, &vn), OK);
    }

    #[test]
    fn test_chmod_allowed_root() {
        let fp = make_fproc(0, 0);
        let vn = make_vnode(100, 50, 0o644);
        assert_eq!(chmod_allowed(&fp, &vn), OK);
    }

    #[test]
    fn test_chmod_allowed_denied() {
        let fp = make_fproc(200, 50);
        let vn = make_vnode(100, 50, 0o644);
        assert_eq!(chmod_allowed(&fp, &vn), EPERM);
    }

    #[test]
    fn test_chown_allowed_root() {
        let fp = make_fproc(0, 0);
        let vn = make_vnode(100, 50, 0o644);
        assert_eq!(chown_allowed(&fp, &vn, 200, 60), OK);
    }

    #[test]
    fn test_chown_allowed_same_owner() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(100, 50, 0o644);
        assert_eq!(chown_allowed(&fp, &vn, -1, 50), OK);
    }

    #[test]
    fn test_chown_allowed_denied_not_owner() {
        let fp = make_fproc(200, 50);
        let vn = make_vnode(100, 50, 0o644);
        assert_eq!(chown_allowed(&fp, &vn, -1, 50), EPERM);
    }

    #[test]
    fn test_chown_allowed_denied_give_away() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(100, 50, 0o644);
        assert_eq!(chown_allowed(&fp, &vn, 200, -1), EPERM);
    }

    #[test]
    fn test_chmod_strip_setgid() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(100, 60, 0o644); // file group 60, caller not in it
        let mut mode = 0o2755;
        chmod_strip_setgid(&fp, &vn, &mut mode);
        assert_eq!(mode, 0o0755);
    }

    #[test]
    fn test_chmod_strip_setgid_in_group() {
        let fp = make_fproc(100, 50);
        let vn = make_vnode(100, 50, 0o644); // file group = caller gid
        let mut mode = 0o2755;
        chmod_strip_setgid(&fp, &vn, &mut mode);
        assert_eq!(mode, 0o2755); // not stripped — caller is in the group
    }
}
