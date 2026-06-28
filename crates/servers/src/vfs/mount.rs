//! VFS filesystem mount operations — adapted from `minix/servers/vfs/mount.c`,
//! `vmnt.c`, `vnode.c`
//!
//! Mount point management: vmnt table operations, mount/unmount syscalls,
//! filesystem server communication for readsuper/putnode.

use crate::vfs::consts::*;
use crate::vfs::types::*;

// ── Vmnt table helpers ──────────────────────────────────────────────────

/// Find a vmnt entry by FS endpoint.
///
/// Scans the vmnt table for an entry whose `m_fs_e` matches the given
/// endpoint. Returns a pointer to the entry, or null if not found.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn find_vmnt(fs_e: i32) -> *mut Vmnt {
    let _ = fs_e;
    core::ptr::null_mut()
}

/// Get a free vmnt slot.
///
/// Scans the vmnt table for an entry with `m_dev == NO_DEV`. Returns
/// a pointer to the free entry, or null if the table is full.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn get_free_vmnt() -> *mut Vmnt {
    core::ptr::null_mut()
}

/// Initialize the vmnt table.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn init_vmnts() {
    // TODO: zero all vmnt entries
}

/// Mark a vmnt entry as free.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn mark_vmnt_free(vmp: *mut Vmnt) {
    let _ = vmp;
}

/// Lock a vmnt entry.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn lock_vmnt(vmp: *mut Vmnt, locktype: i32) -> i32 {
    let _ = (vmp, locktype);
    OK
}

/// Unlock a vmnt entry.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn unlock_vmnt(vmp: *mut Vmnt) {
    let _ = vmp;
}

/// Upgrade a vmnt lock from read to write.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn upgrade_vmnt_lock(vmp: *mut Vmnt) {
    let _ = vmp;
}

/// Downgrade a vmnt lock from write to read.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn downgrade_vmnt_lock(vmp: *mut Vmnt) {
    let _ = vmp;
}

// ── Vnode table helpers ─────────────────────────────────────────────────

/// Get a free vnode slot.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn get_free_vnode() -> *mut Vnode {
    core::ptr::null_mut()
}

/// Find a vnode by FS endpoint and inode number.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn find_vnode(fs_e: i32, inode_nr: u32) -> *mut Vnode {
    let _ = (fs_e, inode_nr);
    core::ptr::null_mut()
}

/// Initialize the vnode table.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn init_vnodes() {
    // TODO: zero all vnode entries
}

/// Lock a vnode.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn lock_vnode(vp: *mut Vnode, locktype: i32) -> i32 {
    let _ = (vp, locktype);
    OK
}

/// Unlock a vnode.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn unlock_vnode(vp: *mut Vnode) {
    let _ = vp;
}

/// Increment a vnode's reference count.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn dup_vnode(vp: *mut Vnode) {
    let _ = vp;
}

/// Decrement a vnode's reference count.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn put_vnode(vp: *mut Vnode) {
    let _ = vp;
}

/// Clean a vnode's FS reference count.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn vnode_clean_refs(vp: *mut Vnode) {
    let _ = vp;
}

// ── Mount/Unmount ───────────────────────────────────────────────────────

/// Mount a filesystem.
///
/// TODO: parse message for dev, path, type, label; find FS driver endpoint;
/// call req_readsuper to read superblock; add vmnt entry; link vnode root.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/mount.c` (do_mount)
pub fn do_mount() -> i32 {
    ENOSYS
}

/// Unmount a filesystem.
///
/// TODO: find vmnt by dev or path; flush all dirty blocks; call
/// req_unmount on FS server; free vmnt and vnode entries.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/mount.c` (do_umount)
pub fn do_umount() -> i32 {
    ENOSYS
}

/// Mount a filesystem with explicit parameters (internal use).
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/mount.c` (mount_fs)
pub fn mount_fs(
    dev: u32,
    mount_dev: &[u8],
    mount_path: &[u8],
    fs_e: i32,
    flags: i32,
    mount_type: &[u8],
    mount_label: &[u8],
) -> i32 {
    let _ = (
        dev,
        mount_dev,
        mount_path,
        fs_e,
        flags,
        mount_type,
        mount_label,
    );
    ENOSYS
}

/// Unmount a filesystem by device or label.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/mount.c` (unmount)
pub fn unmount(dev: u32, label: Option<&[u8]>) -> i32 {
    let _ = (dev, label);
    ENOSYS
}

/// Mount the Pipe File System (PFS) for pipe operations.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/mount.c` (mount_pfs)
pub fn mount_pfs() {
    // TODO: mount PFS for pipe backing
}

/// Check if a device is NONE (no device).
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/mount.c` (is_nonedev)
pub fn is_nonedev(dev: u32) -> i32 {
    if dev == u32::MAX { OK } else { ENOSYS }
}

/// Unmount all filesystems (for reboot).
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/mount.c` (unmount_all)
pub fn unmount_all(force: i32) {
    let _ = force;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_nonedev_ok() {
        assert_eq!(is_nonedev(u32::MAX), OK);
    }

    #[test]
    fn test_do_mount_returns_enosys() {
        assert_eq!(do_mount(), ENOSYS);
    }

    #[test]
    fn test_get_free_vmnt_returns_null() {
        assert!(get_free_vmnt().is_null());
    }
}
