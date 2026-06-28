//! VFS filesystem mount operations — adapted from `minix/servers/vfs/mount.c`,
//! `vmnt.c`, `vnode.c`
//!
//! Mount point management: vmnt table operations, mount/unmount syscalls,
//! filesystem server communication for readsuper/putnode.

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::*;

use core::ptr::addr_of_mut;

// ── Vmnt table helpers ──────────────────────────────────────────────────

/// Find a vmnt entry by FS endpoint.
///
/// Scans the vmnt table for an entry whose `m_fs_e` matches the given
/// endpoint. Returns a pointer to the entry, or null if not found.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn find_vmnt(fs_e: i32) -> *mut Vmnt {
    unsafe {
        let glob = vfs_global();
        let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &mut *vmnt_arr.add(i);
            if vmp.m_fs_e == fs_e {
                return vmp;
            }
        }
    }
    core::ptr::null_mut()
}

/// Get a free vmnt slot.
///
/// Scans the vmnt table for an entry with `m_fs_e == -1` (NONE). Returns
/// a pointer to the free entry, or null if the table is full.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn get_free_vmnt() -> *mut Vmnt {
    unsafe {
        let glob = vfs_global();
        let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &mut *vmnt_arr.add(i);
            if vmp.m_fs_e == -1 {
                return vmp;
            }
        }
    }
    core::ptr::null_mut()
}

/// Initialize the vmnt table.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn init_vmnts() {
    unsafe {
        let glob = vfs_global();
        let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
        for i in 0..NR_MNTS {
            *vmnt_arr.add(i) = Vmnt::default();
        }
    }
}

/// Mark a vmnt entry as free.
///
/// # Safety
///
/// `vmp` must point to a valid, initialized Vmnt entry.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub unsafe fn mark_vmnt_free(vmp: *mut Vmnt) {
    if !vmp.is_null() {
        unsafe {
            *vmp = Vmnt::default();
        }
    }
}

/// Lock a vmnt entry.
///
/// TODO: use tll locking once integrated into Vmnt struct.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn lock_vmnt(_vmp: *mut Vmnt, _locktype: i32) -> i32 {
    OK
}

/// Unlock a vmnt entry.
///
/// TODO: use tll locking once integrated into Vmnt struct.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn unlock_vmnt(_vmp: *mut Vmnt) {}

/// Upgrade a vmnt lock from read to write.
///
/// TODO: use tll locking once integrated into Vmnt struct.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn upgrade_vmnt_lock(_vmp: *mut Vmnt) {}

/// Downgrade a vmnt lock from write to read.
///
/// TODO: use tll locking once integrated into Vmnt struct.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vmnt.c`
pub fn downgrade_vmnt_lock(_vmp: *mut Vmnt) {}

// ── Vnode table helpers ─────────────────────────────────────────────────

/// Get a free vnode slot.
///
/// Scans the vnode table for an entry with `v_ref_count == 0`. Returns
/// a pointer to the free entry, or null if the table is full.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn get_free_vnode() -> *mut Vnode {
    unsafe {
        let glob = vfs_global();
        let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
        for i in 0..NR_VNODES {
            let vp = &mut *vnode_arr.add(i);
            if vp.v_ref_count == 0 {
                return vp;
            }
        }
    }
    core::ptr::null_mut()
}

/// Find a vnode by FS endpoint and inode number.
///
/// Scans the vnode table for an entry whose `v_fs_e` and `v_inode_nr`
/// match the given values. Returns a pointer to the entry, or null.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn find_vnode(fs_e: i32, inode_nr: u32) -> *mut Vnode {
    unsafe {
        let glob = vfs_global();
        let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
        for i in 0..NR_VNODES {
            let vp = &mut *vnode_arr.add(i);
            if vp.v_fs_e == fs_e && vp.v_inode_nr == inode_nr && vp.v_ref_count > 0 {
                return vp;
            }
        }
    }
    core::ptr::null_mut()
}

/// Initialize the vnode table.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn init_vnodes() {
    unsafe {
        let glob = vfs_global();
        let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
        for i in 0..NR_VNODES {
            *vnode_arr.add(i) = Vnode::default();
        }
    }
}

/// Lock a vnode.
///
/// TODO: use tll locking once integrated into Vnode struct.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn lock_vnode(_vp: *mut Vnode, _locktype: i32) -> i32 {
    OK
}

/// Unlock a vnode.
///
/// TODO: use tll locking once integrated into Vnode struct.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub fn unlock_vnode(_vp: *mut Vnode) {}

/// Increment a vnode's reference count.
///
/// # Safety
///
/// `vp` must point to a valid, initialized Vnode entry.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub unsafe fn dup_vnode(vp: *mut Vnode) {
    if !vp.is_null() {
        unsafe {
            (*vp).v_ref_count += 1;
        }
    }
}

/// Decrement a vnode's reference count.
///
/// When `v_ref_count` reaches 0 and `v_fs_count > 0`, calls
/// `req_putnode` to release the FS server's reference, then
/// resets the entry to default.
///
/// # Safety
///
/// `vp` must point to a valid, initialized Vnode entry.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub unsafe fn put_vnode(vp: *mut Vnode) {
    if vp.is_null() {
        return;
    }
    unsafe {
        if (*vp).v_ref_count > 0 {
            (*vp).v_ref_count -= 1;
        }
        if (*vp).v_ref_count == 0 {
            if (*vp).v_fs_count > 0 {
                // Tell the FS server to release its reference.
                // req_putnode is a stub until IPC is wired (Phase 13).
                let _ = crate::vfs::request::req_putnode(
                    (*vp).v_fs_e,
                    (*vp).v_inode_nr,
                    (*vp).v_fs_count,
                );
                (*vp).v_fs_count = 0;
            }
            *vp = Vnode::default();
        }
    }
}

/// Clean a vnode's FS reference count.
///
/// # Safety
///
/// `vp` must point to a valid, initialized Vnode entry.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/vnode.c`
pub unsafe fn vnode_clean_refs(vp: *mut Vnode) {
    if !vp.is_null() {
        unsafe {
            (*vp).v_fs_count = 0;
            (*vp).v_fs_count_check = 0;
        }
    }
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

    /// Helper: initialize VFS globals and vmnt/vnode tables for testing.
    unsafe fn init_tables() {
        crate::vfs::glo::vfs_init();
        init_vmnts();
        init_vnodes();
    }

    #[test]
    fn test_is_nonedev_ok() {
        assert_eq!(is_nonedev(u32::MAX), OK);
    }

    #[test]
    fn test_do_mount_returns_enosys() {
        assert_eq!(do_mount(), ENOSYS);
    }

    // ── Vmnt table ────────────────────────────────────────────────────────

    #[test]
    fn test_get_free_vmnt_finds_entry_after_init() {
        unsafe {
            init_tables();
            let vmp = get_free_vmnt();
            assert!(!vmp.is_null(), "should find free vmnt after init");
            assert_eq!((*vmp).m_fs_e, -1, "free vmnt should have m_fs_e == -1");
        }
    }

    #[test]
    fn test_get_free_vmnt_returns_null_when_full() {
        unsafe {
            init_tables();
            // Mark all entires as used
            let glob = vfs_global();
            let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
            for i in 0..NR_MNTS {
                (*vmnt_arr.add(i)).m_fs_e = 42;
            }
            assert!(get_free_vmnt().is_null(), "should be null when table full");
        }
    }

    #[test]
    fn test_find_vmnt_finds_by_fs_e() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
            (*vmnt_arr.add(3)).m_fs_e = 99;
            let found = find_vmnt(99);
            assert!(!found.is_null());
            assert_eq!((*found).m_fs_e, 99);
        }
    }

    #[test]
    fn test_find_vmnt_returns_null_for_missing() {
        unsafe {
            init_tables();
            assert!(find_vmnt(999).is_null());
        }
    }

    #[test]
    fn test_mark_vmnt_free_resets_entry() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
            let vmp = &mut *vmnt_arr;
            vmp.m_fs_e = 42;
            vmp.m_dev = 0xDEAD;
            mark_vmnt_free(vmp);
            assert_eq!(vmp.m_fs_e, -1, "should reset m_fs_e to -1");
            assert_eq!(vmp.m_dev, 0, "should reset m_dev to 0");
        }
    }

    // ── Vnode table ───────────────────────────────────────────────────────

    #[test]
    fn test_get_free_vnode_finds_entry_after_init() {
        unsafe {
            init_tables();
            let vp = get_free_vnode();
            assert!(!vp.is_null(), "should find free vnode after init");
            assert_eq!(
                (*vp).v_ref_count,
                0,
                "free vnode should have v_ref_count == 0"
            );
        }
    }

    #[test]
    fn test_get_free_vnode_returns_null_when_full() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
            for i in 0..NR_VNODES {
                (*vnode_arr.add(i)).v_ref_count = 1;
            }
            assert!(get_free_vnode().is_null(), "should be null when table full");
        }
    }

    #[test]
    fn test_find_vnode_finds_by_fs_e_and_inode() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
            let vp = &mut *vnode_arr.add(7);
            vp.v_fs_e = 10;
            vp.v_inode_nr = 100;
            vp.v_ref_count = 1;
            let found = find_vnode(10, 100);
            assert!(!found.is_null());
            assert_eq!((*found).v_fs_e, 10);
            assert_eq!((*found).v_inode_nr, 100);
        }
    }

    #[test]
    fn test_find_vnode_returns_null_for_missing() {
        unsafe {
            init_tables();
            assert!(find_vnode(999, 0).is_null());
        }
    }

    #[test]
    fn test_find_vnode_skips_zero_refcount() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
            let vp = &mut *vnode_arr.add(5);
            vp.v_fs_e = 10;
            vp.v_inode_nr = 100;
            vp.v_ref_count = 0; // explicitly free
            assert!(find_vnode(10, 100).is_null(), "should skip free vnodes");
        }
    }

    #[test]
    fn test_dup_vnode_increments_refcount() {
        unsafe {
            init_tables();
            let mut v = Vnode::default();
            let vp = &mut v as *mut Vnode;
            dup_vnode(vp);
            assert_eq!((*vp).v_ref_count, 1, "dup_vnode should increment");
            dup_vnode(vp);
            assert_eq!((*vp).v_ref_count, 2, "dup_vnode should increment again");
        }
    }

    #[test]
    fn test_dup_vnode_null_is_noop() {
        unsafe { dup_vnode(core::ptr::null_mut()) }; // should not panic
    }

    #[test]
    fn test_put_vnode_decrements_refcount() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
            let vp = &mut *vnode_arr;
            vp.v_fs_e = 10;
            vp.v_inode_nr = 42;
            vp.v_ref_count = 2;
            put_vnode(vp);
            assert_eq!((*vp).v_ref_count, 1, "should decrement refcount");
        }
    }

    #[test]
    fn test_put_vnode_resets_when_refcount_reaches_zero() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
            let vp = &mut *vnode_arr;
            vp.v_fs_e = 10;
            vp.v_inode_nr = 42;
            vp.v_ref_count = 1;
            vp.v_fs_count = 1;
            put_vnode(vp);
            // After reaching 0, vnode should be reset to default
            assert_eq!((*vp).v_fs_e, -1, "should reset v_fs_e to -1");
            assert_eq!((*vp).v_ref_count, 0, "should reset v_ref_count to 0");
        }
    }

    #[test]
    fn test_put_vnode_null_is_noop() {
        unsafe { put_vnode(core::ptr::null_mut()) }; // should not panic
    }

    #[test]
    fn test_vnode_clean_refs_resets_fs_count() {
        unsafe {
            let mut v = Vnode::default();
            v.v_fs_count = 5;
            v.v_fs_count_check = 3;
            vnode_clean_refs(&mut v as *mut Vnode);
            assert_eq!(v.v_fs_count, 0);
            assert_eq!(v.v_fs_count_check, 0);
        }
    }

    #[test]
    fn test_vnode_clean_refs_null_is_noop() {
        unsafe { vnode_clean_refs(core::ptr::null_mut()) }; // should not panic
    }
}
