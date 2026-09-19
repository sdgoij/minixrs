//! Caller credentials: the `vfs_ucred_t` VFS hands an FS through a grant.
//!
//! Ported from Minix 3.3.0's `libminixfs/fetch_credentials.c` and the
//! `vfs_ucred_t` layout in `minix/include/minix/vfsif.h`. VFS attaches this
//! block only when the caller belongs to supplemental groups
//! (`PATH_GET_UCRED`); otherwise uid and gid travel in the request itself.

use crate::libminixfs::errors::EINVAL;
#[cfg(target_os = "minix")]
use crate::libminixfs::errors::OK;

/// Maximum supplemental groups. PM rejects `setgroups` beyond this, and VFS's
/// `Fproc` table holds this many, so a credential block is always large enough
/// for what VFS can send. (The C's `NGROUPS_MAX` is 16; the port's PM allows
/// 32, so the block has to match that instead.)
pub const NGROUPS_MAX: usize = 32;

/// The caller's credentials (C `vfs_ucred_t`). uid and gid are u16 here,
/// matching VFS's `Fproc` and the FS inode fields (the C's are 32-bit).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VfsUCred {
    /// Effective user id.
    pub vu_uid: u16,
    /// Effective group id.
    pub vu_gid: u16,
    /// Number of valid entries in `vu_sgroups`.
    pub vu_ngroups: i32,
    /// Supplemental groups; the first `vu_ngroups` entries are valid.
    pub vu_sgroups: [u16; NGROUPS_MAX],
}

impl VfsUCred {
    pub const fn zeroed() -> Self {
        Self {
            vu_uid: 0,
            vu_gid: 0,
            vu_ngroups: 0,
            vu_sgroups: [0; NGROUPS_MAX],
        }
    }

    /// Whether `grp` is one of the caller's supplemental groups (C
    /// `in_group()`). Its `EINVAL` for a corrupt `vu_ngroups` is reported as
    /// "not a member" here, which is what the C's callers end up doing with
    /// it: the permission bits of "other" apply.
    pub fn in_group(&self, grp: u16) -> bool {
        if self.vu_ngroups > NGROUPS_MAX as i32 {
            return false;
        }
        let n = self.vu_ngroups.clamp(0, NGROUPS_MAX as i32) as usize;
        self.vu_sgroups[..n].contains(&grp)
    }
}

/// Bytes VFS grants for a credential block.
pub const VFS_UCRED_SIZE: usize = core::mem::size_of::<VfsUCred>();

/// C `fs_lookup_credentials()`: copy the caller's credential block out of the
/// grant VFS attached to the request, and take the caller's uid/gid from it.
/// A block claiming more groups than `NGROUPS_MAX` is refused, as the C's
/// `assert(credentials->vu_ngroups <= NGROUPS_MAX)` would abort.
///
/// A host build has no kernel to copy through, so `credentials` is used as it
/// stands; the host tests that set `PATH_GET_UCRED` fill the block in
/// themselves.
pub fn fs_lookup_credentials(
    credentials: &mut VfsUCred,
    grant_ucred: i32,
) -> Result<(u16, u16), i32> {
    #[cfg(target_os = "minix")]
    {
        // The C memsets the block before the copy; without it a grant shorter
        // than this struct would leave the tail holding the previous request's
        // groups.
        *credentials = VfsUCred::zeroed();
        // Safety: `credentials` outlives the copy and the grant was made on
        // `VFS_UCRED_SIZE` bytes by VFS.
        let r = unsafe {
            safecopy_from_grant(
                grant_ucred,
                credentials as *mut VfsUCred as *mut u8,
                VFS_UCRED_SIZE,
            )
        };
        if r != OK {
            return Err(r);
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = grant_ucred;
    }

    if credentials.vu_ngroups > NGROUPS_MAX as i32 {
        return Err(EINVAL);
    }
    Ok((credentials.vu_uid, credentials.vu_gid))
}

/// `sys_safecopyfrom(VFS_PROC_NR, grant, 0, dst, len)` — the copy
/// `fetch_credentials.c` makes.
#[cfg(target_os = "minix")]
unsafe fn safecopy_from_grant(grant: i32, dst: *mut u8, len: usize) -> i32 {
    let mut kmsg = [0u8; 64];
    kmsg[8..12].copy_from_slice(&arch_common::com::VFS_PROC_NR.to_le_bytes());
    kmsg[12..16].copy_from_slice(&grant.to_le_bytes());
    kmsg[16..24].copy_from_slice(&0u64.to_le_bytes());
    kmsg[24..32].copy_from_slice(&(dst as u64).to_le_bytes());
    kmsg[32..40].copy_from_slice(&(len as u64).to_le_bytes());
    minix_rt::kernel_call(31, &mut kmsg) // SYS_SAFECOPYFROM
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::offset_of;

    #[test]
    fn test_credential_block_layout() {
        // VFS fills this block and the FS copies it, both through
        // `VfsUCred` — the offsets are only fixed by the C `vfs_ucred_t`
        // field order (uid, gid, ngroups, sgroups).
        assert_eq!(offset_of!(VfsUCred, vu_uid), 0);
        assert_eq!(offset_of!(VfsUCred, vu_gid), 2);
        assert_eq!(offset_of!(VfsUCred, vu_ngroups), 4);
        assert_eq!(offset_of!(VfsUCred, vu_sgroups), 8);
        assert_eq!(VFS_UCRED_SIZE, 8 + NGROUPS_MAX * 2);
    }

    #[test]
    fn test_in_group_finds_member() {
        let mut cred = VfsUCred::zeroed();
        cred.vu_ngroups = 3;
        cred.vu_sgroups[0] = 100;
        cred.vu_sgroups[1] = 200;
        cred.vu_sgroups[2] = 300;

        assert!(cred.in_group(200));
        assert!(!cred.in_group(250));
        assert!(!cred.in_group(1000));
    }

    #[test]
    fn test_in_group_ignores_entries_past_ngroups() {
        let mut cred = VfsUCred::zeroed();
        cred.vu_ngroups = 1;
        cred.vu_sgroups[0] = 100;
        cred.vu_sgroups[1] = 200; // not part of the caller's groups

        assert!(cred.in_group(100));
        assert!(!cred.in_group(200));
    }

    #[test]
    fn test_in_group_rejects_oversized_ngroups() {
        let mut cred = VfsUCred::zeroed();
        cred.vu_ngroups = NGROUPS_MAX as i32 + 1;
        cred.vu_sgroups[0] = 100;

        // The C returns EINVAL here, which its caller treats as "not a
        // member": the sgroups array must not be trusted.
        assert!(!cred.in_group(100));
    }

    #[test]
    fn test_in_group_empty() {
        let cred = VfsUCred::zeroed();
        assert!(!cred.in_group(0));
    }

    #[test]
    fn test_fs_lookup_credentials_takes_uid_and_gid() {
        // On the host there is no grant to copy through: the block is read as
        // it stands, so this covers the validation half of the C's
        // `fs_lookup_credentials()`.
        let mut cred = VfsUCred::zeroed();
        cred.vu_uid = 1000;
        cred.vu_gid = 100;
        cred.vu_ngroups = 2;
        cred.vu_sgroups[0] = 100;

        assert_eq!(fs_lookup_credentials(&mut cred, -1), Ok((1000, 100)));
    }

    #[test]
    fn test_fs_lookup_credentials_rejects_oversized_ngroups() {
        let mut cred = VfsUCred::zeroed();
        cred.vu_uid = 1000;
        cred.vu_ngroups = NGROUPS_MAX as i32 + 1;

        assert_eq!(fs_lookup_credentials(&mut cred, -1), Err(EINVAL));
    }
}
