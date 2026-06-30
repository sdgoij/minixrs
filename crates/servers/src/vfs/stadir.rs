//! File stat operations — adapted from `minix/servers/vfs/stadir.c`
//!
//! Stat and statvfs implementations for VFS. These fill in `struct stat`
//! and `struct statvfs` from vnode/vmnt data, then copy results to
//! userspace via the FS request layer.

use crate::vfs::consts::*;
use crate::vfs::types::*;

// StatvfsCache

/// Cached statvfs fields per mount point (avoids 2KB per mount entry).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct StatvfsCache {
    pub svc_version: u32,
    pub svc_bsize: u32,
    pub svc_frsize: u32,
    pub svc_blocks: u64,
    pub svc_bfree: u64,
    pub svc_bavail: u64,
    pub svc_files: u64,
    pub svc_ffree: u64,
    pub svc_favail: u64,
    pub svc_fsid: u64,
    pub svc_flag: u64,
    pub svc_namemax: u64,
}

// Stat helpers

/// Fill a `statvfs` struct from `StatvfsCache` and mount flags.
///
/// TODO: copy cached values to output statvfs buffer.
/// Calls req_statvfs(fs_e) to refresh cache if needed.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/stadir.c` (update_statvfs)
pub fn update_statvfs(vmp: &Vmnt, buf: &mut Statvfs) -> i32 {
    let _ = (vmp, buf);
    ENOSYS
}

/// Fill a `stat` struct from vnode data.
///
/// Copies vnode fields (mode, size, inode_nr, dev) into the stat struct
/// and updates timestamps from vnode times.
///
/// TODO: fill stat fields from vp, copy to user buffer via req_stat.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/stadir.c`
pub fn stat_inode(vp: &Vnode, stat_buf: &mut [u8; 144]) -> i32 {
    let _ = (vp, stat_buf);
    ENOSYS
}

// Directory change helpers

/// Change the current working directory to a new vnode.
///
/// Validates that the vnode is a directory and checks permissions,
/// then updates `fp_wd` to point to the new vnode.
///
/// TODO: check S_ISDIR(vp->v_mode), call forbidden(rfp, vp, X_BIT),
/// then put_vnode on old wd and dup_vnode on new wd.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/stadir.c` (change_into)
pub fn change_into(fp_wd: &mut u32, vp: &Vnode) -> i32 {
    let _ = (fp_wd, vp);
    ENOSYS
}

// File descriptor

/// Close a file descriptor in a process's fd table.
///
/// Decrements filp refcount and clears the fd slot.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/open.c` (close_fd)
pub fn close_fd(rfp: &mut Fproc, fd_nr: i32) -> i32 {
    if fd_nr < 0 || (fd_nr as usize) >= OPEN_MAX {
        return EBADF;
    }
    let filp_idx = rfp.fp_filp[fd_nr as usize];
    if filp_idx < 0 {
        return EBADF;
    }
    rfp.fp_filp[fd_nr as usize] = -1;
    rfp.fp_cloexec &= !(1u64 << fd_nr);
    unsafe { crate::vfs::filedes::close_filp(rfp, filp_idx) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_statvfs_cache_default() {
        let c = StatvfsCache::default();
        assert_eq!(c.svc_version, 0);
    }

    #[test]
    fn test_update_statvfs_returns_enosys() {
        let vmp = Vmnt::default();
        let mut buf = Statvfs::default();
        assert_eq!(update_statvfs(&vmp, &mut buf), ENOSYS);
    }

    #[test]
    fn test_stat_inode_returns_enosys() {
        let vp = Vnode::default();
        let mut buf = [0u8; 144];
        assert_eq!(stat_inode(&vp, &mut buf), ENOSYS);
    }
}
