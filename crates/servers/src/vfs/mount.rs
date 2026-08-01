//! VFS filesystem mount operations — adapted from `minix/servers/vfs/mount.c`,
//! `vmnt.c`, `vnode.c`
//!
//! Mount point management: vmnt table operations, mount/unmount syscalls,
//! filesystem server communication for readsuper/putnode.

use crate::vfs::consts::*;
use crate::vfs::dmap;
use crate::vfs::glo::vfs_global;
use crate::vfs::request::req_readsuper;
use crate::vfs::types::*;

use core::ptr::addr_of_mut;

// Message offsets (mess_lc_vfs_mount, 32-bit layout, payload starts at 8)
// struct { int flags; size_t devlen,pathlen,typelen,labellen;
//          vir_bytes dev,path,type,label; uint8_t padding[20]; }
// All size fields are 4 bytes (matching 32-bit ABI for message compatibility).
// Pointer fields (vir_bytes) are 8 bytes on x86_64.

const MOUNT_FLAGS_OFF: usize = 8;
const MOUNT_DEVLEN_OFF: usize = 12;
const MOUNT_PATHLEN_OFF: usize = 16;
const MOUNT_TYPELEN_OFF: usize = 20;
const MOUNT_LABELLEN_OFF: usize = 24;
const MOUNT_DEV_OFF: usize = 28;
const MOUNT_PATH_OFF: usize = 36;
const MOUNT_TYPE_OFF: usize = 44;
const MOUNT_LABEL_OFF: usize = 52;

// helpers
fn r_i32(buf: &[u8; 64], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}
fn r_u32(buf: &[u8; 64], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}
fn r_u64(buf: &[u8; 64], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}

// Vmnt table helpers

/// Find a vmnt entry by FS endpoint.
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
pub unsafe fn mark_vmnt_free(vmp: *mut Vmnt) {
    if !vmp.is_null() {
        unsafe { *vmp = Vmnt::default() }
    }
}

pub fn lock_vmnt(_vmp: *mut Vmnt, _locktype: i32) -> i32 {
    OK
}
pub fn unlock_vmnt(_vmp: *mut Vmnt) {}
pub fn upgrade_vmnt_lock(_vmp: *mut Vmnt) {}
pub fn downgrade_vmnt_lock(_vmp: *mut Vmnt) {}

// Vnode table helpers

/// Get a free vnode slot.
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
pub fn init_vnodes() {
    unsafe {
        let glob = vfs_global();
        let vnode_arr = addr_of_mut!((*glob).vnode) as *mut Vnode;
        for i in 0..NR_VNODES {
            *vnode_arr.add(i) = Vnode::default();
        }
    }
}

pub fn lock_vnode(_vp: *mut Vnode, _locktype: i32) -> i32 {
    OK
}
pub fn unlock_vnode(_vp: *mut Vnode) {}

/// Increment a vnode's reference count.
///
/// # Safety
///
/// `vp` must point to a valid, initialized Vnode entry.
pub unsafe fn dup_vnode(vp: *mut Vnode) {
    if !vp.is_null() {
        unsafe { (*vp).v_ref_count += 1 }
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
pub unsafe fn put_vnode(vp: *mut Vnode) {
    if vp.is_null() {
        return;
    }
    unsafe {
        if (*vp).v_ref_count > 0 {
            (*vp).v_ref_count -= 1
        }
        if (*vp).v_ref_count == 0 {
            if (*vp).v_fs_count > 0 {
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
pub unsafe fn vnode_clean_refs(vp: *mut Vnode) {
    if !vp.is_null() {
        unsafe {
            (*vp).v_fs_count = 0;
            (*vp).v_fs_count_check = 0;
        }
    }
}

// Mount/Unmount

/// Maximum label length for FS driver lookups.
const LABEL_BUF_SIZE: usize = 64;

/// Copy a string from a user-space process into a kernel buffer.
///
/// Uses `kernel::vm::virtual_copy` to read from the caller's address
/// space. Returns the buffer and actual length on success.
unsafe fn copy_string_from_user(
    caller_ep: i32,
    user_addr: u64,
    max_len: usize,
    buf: &mut [u8],
) -> Result<usize, i32> {
    if user_addr == 0 || max_len == 0 || buf.is_empty() {
        return Err(EINVAL);
    }
    let copy_len = max_len.min(buf.len() - 1);
    let caller_slot = kernel::table::endpoint_slot(caller_ep);
    let r = kernel::vm::virtual_copy(
        caller_slot,
        user_addr,
        -1, // kernel
        buf.as_mut_ptr() as u64,
        copy_len,
    );
    if r != 0 {
        return Err(r);
    }
    let actual_len = buf[..copy_len]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(copy_len);
    buf[actual_len] = 0; // null-terminate
    Ok(actual_len)
}

/// Mount a filesystem.
///
/// Parses the mount request from fs_m_in: flags, device path, mount point
/// path, filesystem type, and label. Validates superuser, copies the label
/// from userspace, resolves the FS driver via dmap, calls req_readsuper on
/// the FS server, and links the root vnode into the VFS name space.
pub fn do_mount() -> i32 {
    let glob = unsafe { &*vfs_global() };

    // Only super-user may mount.
    let fp = match unsafe { glob.fp.as_ref() } {
        Some(fp) => fp,
        None => return EINVAL,
    };
    if fp.fp_effuid != SU_UID {
        return EPERM;
    }
    let caller_ep = fp.fp_endpoint;

    // Parse message fields.
    let flags = r_i32(&glob.fs_m_in, MOUNT_FLAGS_OFF);
    let _devlen = r_u32(&glob.fs_m_in, MOUNT_DEVLEN_OFF);
    let _pathlen = r_u32(&glob.fs_m_in, MOUNT_PATHLEN_OFF);
    let _typelen = r_u32(&glob.fs_m_in, MOUNT_TYPELEN_OFF);
    let labellen = r_u32(&glob.fs_m_in, MOUNT_LABELLEN_OFF);
    let _dev_addr = r_u64(&glob.fs_m_in, MOUNT_DEV_OFF);
    let _path_addr = r_u64(&glob.fs_m_in, MOUNT_PATH_OFF);
    let _type_addr = r_u64(&glob.fs_m_in, MOUNT_TYPE_OFF);
    let label_addr = r_u64(&glob.fs_m_in, MOUNT_LABEL_OFF);

    // Step 1: Copy the FS label from userspace.
    let mut label_buf = [0u8; LABEL_BUF_SIZE];
    let label_len = match unsafe {
        copy_string_from_user(caller_ep, label_addr, labellen as usize, &mut label_buf)
    } {
        Ok(len) => len,
        Err(e) => return e,
    };
    let label = &label_buf[..label_len];

    // Step 2: Look up the FS driver by label in dmap.
    let major = dmap::find_dmap_by_label(label);
    if major < 0 {
        return ENOSYS; // No driver found for this label
    }
    let dp = dmap::get_dmap_by_major(major);
    if dp.is_null() {
        return ENOSYS;
    }
    let fs_e = unsafe { (*dp).dmap_ep };

    // Step 3: Allocate a vmnt entry and fill the FS endpoint.
    let vmp = get_free_vmnt();
    if vmp.is_null() {
        return ENFILE;
    }
    unsafe {
        (*vmp).m_fs_e = fs_e;
    }

    // Step 4: Call req_readsuper on the FS server.
    let readonly = if (flags & 1) != 0 { 1 } else { 0 }; // TODO: proper flag constants
    let (r, node, _flags_reply) = unsafe {
        req_readsuper(
            vmp,
            core::ptr::null(),
            0, /*dev*/
            readonly,
            1, /*isroot*/
        )
    };
    if r != OK {
        unsafe { mark_vmnt_free(vmp) };
        return r;
    }

    // Fill remaining vmnt fields.
    unsafe {
        let copy_len = label.len().min(LABEL_MAX - 1);
        (*vmp).m_dev = node.dev;
        (*vmp).m_root_node = node.inode_nr;
        (*vmp).m_flags = 0;
        let m_label = &mut (*vmp).m_label;
        m_label[..copy_len].copy_from_slice(&label[..copy_len]);
        m_label[copy_len] = 0;
    }

    // Step 5: Allocate a root vnode and link it.
    let vp = get_free_vnode();
    if vp.is_null() {
        unsafe { mark_vmnt_free(vmp) };
        return ENFILE;
    }
    unsafe {
        (*vp).v_fs_e = fs_e;
        (*vp).v_inode_nr = node.inode_nr;
        (*vp).v_mode = node.mode;
        (*vp).v_size = node.file_size;
        (*vp).v_dev = node.dev;
        (*vp).v_ref_count = 1;
        (*vp).v_fs_count = 1;
    }

    // Step 6: Set root directory references (for / mount).
    unsafe {
        let glob_mut = &mut *vfs_global();
        glob_mut.root_dev = node.dev;
        glob_mut.root_fs_e = fs_e;
    }

    OK
}

/// Unmount a filesystem.
pub fn do_umount() -> i32 {
    let glob = unsafe { &*vfs_global() };

    // Only super-user may unmount.
    let fp = unsafe { &*glob.fp };
    if fp.fp_effuid != SU_UID {
        return EPERM;
    }

    // TODO: read device or path from message; find vmnt; flush; req_unmount.
    ENOSYS
}

/// Mount a filesystem with explicit parameters (internal use).
pub fn mount_fs(
    _dev: u32,
    _mount_dev: &[u8],
    _mount_path: &[u8],
    _fs_e: i32,
    _flags: i32,
    _mount_type: &[u8],
    _mount_label: &[u8],
) -> i32 {
    ENOSYS
}

/// Unmount a filesystem by device or label.
pub fn unmount(_dev: u32, _label: Option<&[u8]>) -> i32 {
    ENOSYS
}

/// Mount the Pipe File System (PFS) for pipe operations.
pub fn mount_pfs() {}

/// Check if a device is NONE (no device).
pub fn is_nonedev(dev: u32) -> i32 {
    if dev == u32::MAX { OK } else { ENOSYS }
}

/// Unmount all filesystems (for reboot).
pub fn unmount_all(_force: i32) {}

/// Mount the root filesystem at boot time.
///
/// Registers MFS in the dmap table, allocates a vmnt entry, calls
/// req_readsuper on MFS, and sets root_dev / root_fs_e so VFS can
/// resolve absolute paths. Returns a pointer to the root vnode, or
/// null on failure. Should be called once during VFS init.
pub fn mount_root() -> *mut Vnode {
    // Step 1: Register MFS in the dmap table.
    // MFS is at endpoint 7 (MFS_PROC_NR, generation 0).
    // Label "mfs" is matched by do_mount / init mount.
    let label = b"mfs";
    unsafe {
        dmap::map_driver(label, 0, arch_common::com::MFS_PROC_NR);
    }

    // Step 2: Look up the FS driver by label in dmap.
    let major = dmap::find_dmap_by_label(label);
    if major < 0 {
        return core::ptr::null_mut();
    }
    let dp = dmap::get_dmap_by_major(major);
    if dp.is_null() {
        return core::ptr::null_mut();
    }
    let fs_e = unsafe { (*dp).dmap_ep };

    // Step 3: Allocate a vmnt entry and fill the FS endpoint.
    let vmp = get_free_vmnt();
    if vmp.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        (*vmp).m_fs_e = fs_e;
    }

    // Step 4: Call req_readsuper on MFS with dev=0 (RAM disk), isroot=1.
    #[cfg(target_os = "none")]
    let (r, node, _flags_reply) = unsafe {
        req_readsuper(
            vmp,
            label.as_ptr(),
            0, /* dev: RAM disk */
            0, /* readonly: writable */
            1, /* isroot */
        )
    };
    #[cfg(not(target_os = "none"))]
    let (r, node, _flags_reply) = (ENOSYS, crate::vfs::types::NodeDetails::default(), 0);

    if r != OK {
        unsafe { mark_vmnt_free(vmp) };
        return core::ptr::null_mut();
    }

    // Step 5: Fill remaining vmnt fields.
    unsafe {
        (*vmp).m_dev = node.dev;
        (*vmp).m_root_node = node.inode_nr;
        (*vmp).m_flags = 0;
        let m_label = &mut (*vmp).m_label;
        let copy_len = label.len().min(LABEL_MAX - 1);
        m_label[..copy_len].copy_from_slice(&label[..copy_len]);
        m_label[copy_len] = 0;
    }

    // Step 6: Allocate a root vnode and link it.
    let vp = get_free_vnode();
    if vp.is_null() {
        unsafe { mark_vmnt_free(vmp) };
        return core::ptr::null_mut();
    }
    unsafe {
        (*vp).v_fs_e = fs_e;
        (*vp).v_inode_nr = node.inode_nr;
        (*vp).v_mode = node.mode;
        (*vp).v_size = node.file_size;
        (*vp).v_dev = node.dev;
        (*vp).v_ref_count = 1;
        (*vp).v_fs_count = 1;
    }

    // Step 7: Set root directory references.
    unsafe {
        let glob_mut = &mut *vfs_global();
        glob_mut.root_dev = node.dev;
        glob_mut.root_fs_e = fs_e;
    }

    vp
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_mount_rejects_non_superuser() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let fproc_arr = addr_of_mut!((*glob).fproc) as *mut crate::vfs::types::Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_effuid = 1000; // not superuser
            (*glob).fp = fp;
        }
        assert_eq!(do_mount(), EPERM);
    }

    #[test]
    fn test_umount_rejects_non_superuser() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let fproc_arr = addr_of_mut!((*glob).fproc) as *mut crate::vfs::types::Fproc;
            let fp = &mut *fproc_arr.add(0);
            fp.fp_effuid = 1000; // not superuser
            (*glob).fp = fp;
        }
        assert_eq!(do_umount(), EPERM);
    }

    #[test]
    fn test_get_free_vmnt_finds_entry_after_init() {
        unsafe {
            init_tables();
            let vmp = get_free_vmnt();
            assert!(!vmp.is_null());
            assert_eq!((*vmp).m_fs_e, -1);
        }
    }

    #[test]
    fn test_get_free_vmnt_returns_null_when_full() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
            for i in 0..NR_MNTS {
                (*vmnt_arr.add(i)).m_fs_e = 42;
            }
            assert!(get_free_vmnt().is_null());
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
            assert!(find_vmnt(999).is_null())
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
            assert_eq!(vmp.m_fs_e, -1);
            assert_eq!(vmp.m_dev, 0);
        }
    }

    #[test]
    fn test_get_free_vnode_finds_entry_after_init() {
        unsafe {
            init_tables();
            let vp = get_free_vnode();
            assert!(!vp.is_null());
            assert_eq!((*vp).v_ref_count, 0);
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
            assert!(get_free_vnode().is_null());
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
            assert!(find_vnode(999, 0).is_null())
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
            vp.v_ref_count = 0;
            assert!(find_vnode(10, 100).is_null());
        }
    }

    #[test]
    fn test_dup_vnode_increments_refcount() {
        unsafe {
            let mut v = Vnode::default();
            let vp = &mut v as *mut Vnode;
            dup_vnode(vp);
            assert_eq!((*vp).v_ref_count, 1);
            dup_vnode(vp);
            assert_eq!((*vp).v_ref_count, 2);
        }
    }

    #[test]
    fn test_dup_vnode_null_is_noop() {
        unsafe { dup_vnode(core::ptr::null_mut()) }
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
            assert_eq!(vp.v_ref_count, 1);
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
            assert_eq!(vp.v_fs_e, -1);
            assert_eq!(vp.v_ref_count, 0);
        }
    }

    #[test]
    fn test_put_vnode_null_is_noop() {
        unsafe { put_vnode(core::ptr::null_mut()) }
    }

    #[test]
    fn test_vnode_clean_refs_resets_fs_count() {
        unsafe {
            let mut v = Vnode {
                v_fs_count: 5,
                v_fs_count_check: 3,
                ..Default::default()
            };
            vnode_clean_refs(&mut v as *mut Vnode);
            assert_eq!(v.v_fs_count, 0);
            assert_eq!(v.v_fs_count_check, 0);
        }
    }

    #[test]
    fn test_vnode_clean_refs_null_is_noop() {
        unsafe { vnode_clean_refs(core::ptr::null_mut()) }
    }
}
