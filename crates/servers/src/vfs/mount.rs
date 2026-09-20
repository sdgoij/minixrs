//! VFS filesystem mount operations — adapted from `minix/servers/vfs/mount.c`,
//! `vmnt.c`, `vnode.c`
//!
//! Mount point management: vmnt table operations, mount/unmount syscalls,
//! filesystem server communication for readsuper/putnode.

use crate::vfs::consts::*;
use crate::vfs::dmap;
use crate::vfs::glo::vfs_global;
use crate::vfs::request::{req_lookup, req_readsuper};
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

/// Find a vmnt whose mount point is `inode` in the filesystem `fs_e` (the
/// fs that owns the mounted-on directory). This is the mount-crossing
/// lookup: the resolved node is a mounted-on directory, and the vmnt's
/// `m_fs`/`m_mounted_on` identify where it is attached.
pub fn find_vmnt_mounted_on(fs_e: i32, inode: u32) -> *mut Vmnt {
    unsafe {
        let glob = vfs_global();
        let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &mut *vmnt_arr.add(i);
            if vmp.m_fs == fs_e && vmp.m_mounted_on == inode {
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
            // The inode could have been mapped (a FIFO opened via its
            // directory entry): release the mapped FS's inode too — unless
            // it is the same FS (C vnode.c put_vnode).
            if (*vp).v_mapfs_e != crate::vfs::consts::NONE_ENDPOINT
                && (*vp).v_mapfs_e != (*vp).v_fs_e
                && (*vp).v_mapfs_count > 0
            {
                let _ = crate::vfs::request::req_putnode(
                    (*vp).v_mapfs_e,
                    (*vp).v_mapinode_nr,
                    (*vp).v_mapfs_count,
                );
                (*vp).v_mapfs_count = 0;
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
            0, /* label_len */
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

/// Find the mounted filesystem with this device number.
///
/// The test is `m_fs_e`, not `m_dev`: a free slot and the root filesystem both
/// say `m_dev == 0` on this port (C separates them with a distinct `NO_DEV`).
fn find_vmnt_by_dev(dev: u32) -> *mut Vmnt {
    unsafe {
        let glob = vfs_global();
        let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &mut *vmnt_arr.add(i);
            if vmp.m_fs_e >= 0 && vmp.m_dev == dev {
                return vmp;
            }
        }
    }
    core::ptr::null_mut()
}

/// Unmount one mounted filesystem.
///
/// `force` is C's `unmount_all` argument and is passed on to the filesystem: it says
/// that this unmount is a shutdown's, whose guarantee is VFS's own (the busy check
/// below), so a filesystem must come off whatever its own caches still reference.
///
/// The busy check is C's in meaning and not in form. C sums the reference counts of
/// the vnodes on the device and allows exactly one — the vmnt's own root reference.
/// That number is not trustworthy on this port: VFS's path resolution leaked a
/// reference per lookup (`eat_path` and `last_dir` dup'd the directory they started
/// from, and `eat_path` dup'd its result on top of the reference `advance` already
/// returns) and `pm_fork` copied a parent's root and working directories into the
/// child without duplicating them; those are fixed, and `PORTING_PLAN.md` has the
/// rest of what the reference counts still do not add up to. So what is checked here
/// is the thing the count stands for — an open file description on the device —
/// which is what C's check is for: putting a filesystem down under an open file is
/// the mistake, not the bookkeeping.
///
/// Returns `OK` when the filesystem confirmed the unmount, `EBUSY` when something
/// still holds the device, and otherwise what the filesystem answered — in which
/// case the mount record stays: the filesystem has given up nothing, so a later pass
/// (the shutdown's forced one) is still able to ask it again, and the record is the
/// only thing that says there is something there to ask.
///
/// # Safety
///
/// `vmp` must be null or point to a valid, initialized Vmnt entry whose `m_fs_e`
/// names a mounted filesystem.
unsafe fn unmount_vmnt(vmp: *mut Vmnt, force: i32) -> i32 {
    if vmp.is_null() {
        return EINVAL;
    }
    unsafe {
        let glob = vfs_global();
        let dev = (*vmp).m_dev;
        let fs_e = (*vmp).m_fs_e;

        // An open file description on the device: a filp is the reference that
        // makes a filesystem busy, and the one an unmount must not happen under.
        let filp_arr = addr_of_mut!((*glob).filp) as *mut Filp;
        for i in 0..NR_FILPS {
            let f = &*filp_arr.add(i);
            if f.filp_count > 0 && !f.filp_vno.is_null() && (*f.filp_vno).v_dev == dev {
                return EBUSY;
            }
        }

        // Tell the filesystem to drop all inode references for its root but one,
        // then to flush everything it is holding and unmount.
        let root_vp = find_vnode(fs_e, (*vmp).m_root_node);
        vnode_clean_refs(root_vp);
        let r = crate::vfs::request::req_unmount(fs_e, force);
        if r != OK {
            return r;
        }

        // This filesystem is gone, so stop listing it in statistics, and let its
        // root vnode go with it: C reuses the entry rather than freeing it, and so
        // does this.
        (*vmp).m_flags &= !VMNT_CANSTAT;
        if !root_vp.is_null() {
            (*root_vp).v_ref_count = 0;
            (*root_vp).v_fs_count = 0;
        }
        mark_vmnt_free(vmp);
        OK
    }
}

/// Unmount the filesystem mounted on `dev`.
///
/// The half of `umount(2)` that has a filesystem to take down. `do_umount` still
/// needs the path-to-device lookup and the label copy back to the caller, and that
/// caller is why this is the strict form: `force == 0`, so the filesystem applies
/// its own busy check as well as VFS's.
///
/// Returns `EINVAL` when nothing is mounted on `dev`, `EBUSY` when something still
/// holds it, and otherwise the filesystem's answer — with the mount record freed only
/// when the unmount actually happened (see `unmount_vmnt`).
pub fn unmount(dev: u32) -> i32 {
    let vmp = find_vmnt_by_dev(dev);
    if vmp.is_null() {
        return EINVAL;
    }
    unsafe { unmount_vmnt(vmp, 0) }
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

/// PFS is not really mounted onto the filesystem tree — it just needs a
/// vmnt entry so pipe operations can find and lock it (matching the
/// original C `mount_pfs()` in `minix/servers/vfs/mount.c`).
pub fn mount_pfs() {
    let vmp = get_free_vmnt();
    if vmp.is_null() {
        return;
    }
    unsafe {
        (*vmp).m_dev = u32::MAX; // NO_DEV — PFS has no real device
        (*vmp).m_fs_e = PFS_PROC_NR;
        (*vmp).m_flags = 0;
        let label = b"pfs";
        let m_label = &mut (*vmp).m_label;
        m_label[..label.len()].copy_from_slice(label);
        m_label[label.len()] = 0;
    }
}

/// Register the devman FS vmnt and mount it via `req_readsuper`, so the
/// device tree gets populated at boot (devman's `init_hook` runs on mount).
///
/// The mount point is `/devices` in the root filesystem: the directory is
/// resolved in MFS, the vmnt records it as `m_mounted_on`/`m_fs`, and a
/// vnode is created for devman's root so path traversal can cross into the
/// tree (`advance`'s mount-crossing). `m_path` lets the path resolver
/// split a `/devices/...` request at the mount boundary.
///
/// # Safety
///
/// `root_vp` must point to a valid MFS root vnode (from `mount_root`).
pub unsafe fn mount_devman(root_vp: *mut Vnode) -> i32 {
    let vmp = get_free_vmnt();
    if vmp.is_null() {
        return ENFILE;
    }
    unsafe {
        (*vmp).m_fs_e = arch_common::com::DEVMAN_PROC_NR;
    }

    let (r, node, _flags_reply) = unsafe {
        req_readsuper(
            vmp,
            core::ptr::null(),
            0, /* label_len */
            0, /* dev: none */
            0, /* readonly: writable */
            0, /* isroot: devman must not be mounted as root */
        )
    };
    if r != OK {
        unsafe { mark_vmnt_free(vmp) };
        return r;
    }

    // Resolve the mount point (/devices) in the root filesystem.
    let mount_path: &[u8] = b"/devices";
    let mut resolve = Lookup::default();
    resolve.l_path[..mount_path.len()].copy_from_slice(mount_path);
    resolve.l_path_len = mount_path.len();
    let root_ino = (*root_vp).v_inode_nr;
    let (lr, lres) = unsafe {
        req_lookup(
            arch_common::com::MFS_PROC_NR,
            root_ino,
            root_ino,
            0,    /* uid */
            0,    /* gid */
            None, /* superuser: no supplemental groups */
            &resolve,
        )
    };
    if lr != OK {
        unsafe { mark_vmnt_free(vmp) };
        return lr;
    }

    unsafe {
        (*vmp).m_dev = node.dev;
        (*vmp).m_root_node = node.inode_nr;
        (*vmp).m_mounted_on = lres.inode_nr;
        (*vmp).m_fs = arch_common::com::MFS_PROC_NR; // mounted-on dir lives on MFS
        (*vmp).m_flags = 0;
        let label = b"devman";
        let m_label = &mut (*vmp).m_label;
        let copy_len = label.len().min(LABEL_MAX - 1);
        m_label[..copy_len].copy_from_slice(&label[..copy_len]);
        m_label[copy_len] = 0;
        // Mount path for the path resolver's mount-prefix splitter.
        let m_path = &mut (*vmp).m_path;
        m_path[..mount_path.len()].copy_from_slice(mount_path);
        m_path[mount_path.len()] = 0;
    }

    // Create the devman root vnode so mount crossing can find it
    // (find_vnode(DEVMAN, m_root_node)).
    let vp = get_free_vnode();
    if vp.is_null() {
        unsafe { mark_vmnt_free(vmp) };
        return ENFILE;
    }
    unsafe {
        (*vp).v_fs_e = arch_common::com::DEVMAN_PROC_NR;
        (*vp).v_inode_nr = node.inode_nr;
        (*vp).v_mode = node.mode;
        (*vp).v_size = node.file_size;
        (*vp).v_dev = node.dev;
        (*vp).v_ref_count = 1;
        (*vp).v_fs_count = 1;
    }
    OK
}

/// Check if a device is NONE (no device).
pub fn is_nonedev(dev: u32) -> i32 {
    if dev == u32::MAX { OK } else { ENOSYS }
}

/// Unmount all filesystems (for reboot or shutdown).
///
/// Filesystems are mounted on filesystems, so pulling the loose ones off once is
/// not enough: each pass takes off whatever its mount point no longer needs, and
/// `NR_MNTS` passes is deeper than the table can nest (C runs the same loop).
///
/// The passes walk table slots rather than device numbers, because the two mounts
/// on this port that have no device — PFS and devman — carry the same `NO_DEV`
/// sentinel, so `unmount(dev)` would have to guess which was meant. Those two are
/// skipped, as `do_sync` already skips them: there is no medium to leave
/// consistent, and PFS's record is fabricated by `mount_pfs` at init rather than
/// created by a mount that succeeded, so it is the one vmnt that does not imply a
/// server behind it — the wasm boot starts none, and asking it to unmount is a
/// wait for an answer that cannot come.
///
/// Returns how many filesystems did not come off cleanly — the ones still mounted
/// after the passes, which is the reference's own verification. `force` is C's: it is
/// the pass whose answer is a requirement, and the count is only worth returning for
/// it — a server on this port has no console and the wasm disposition is `abort`,
/// which would hang the host instead of reporting it (the disposition `ramdisk.rs`
/// states).
pub fn unmount_all(force: i32) -> i32 {
    unsafe {
        let glob = vfs_global();
        let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;

        for _ in 0..NR_MNTS {
            let mut live = 0;
            for i in 0..NR_MNTS {
                let vmp = vmnt_arr.add(i);
                if (*vmp).m_fs_e < 0 || is_nonedev((*vmp).m_dev) == OK {
                    continue;
                }
                live += 1;
                // The failure is what is left to see in the table: an unmount that did
                // not happen leaves its record mounted, which is what the count below
                // reads, so there is nothing to carry out of this call.
                unmount_vmnt(vmp, force);
            }
            if live == 0 {
                // Nothing left to pull off, so no further pass can reach anything.
                break;
            }
        }

        if force == 0 {
            return 0;
        }
        mounted_count(vmnt_arr)
    }
}

/// Count the table's mounted filesystems that have a device (the port's free marker
/// is `m_fs_e < 0`, and a mount with no device is one `unmount_all` does not put
/// down — see its note).
///
/// # Safety
///
/// `vmnt_arr` must point to `NR_MNTS` valid, initialized Vmnt entries.
unsafe fn mounted_count(vmnt_arr: *mut Vmnt) -> i32 {
    let mut n = 0;
    for i in 0..NR_MNTS {
        let vmp = unsafe { &*vmnt_arr.add(i) };
        if vmp.m_fs_e >= 0 && is_nonedev(vmp.m_dev) != OK {
            n += 1;
        }
    }
    n
}

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

    // Step 4: Call req_readsuper on MFS with dev=0, passing the label of
    // the block driver that backs the root device. MFS resolves the label
    // to a driver endpoint (bdev_driver) and routes root block I/O to it.
    //
    // The label is a preference, not a requirement: MFS probes the driver and
    // falls back to the ramdisk driver when it has no device, which is how a
    // diskless boot mounts the embedded filesystem image instead. With a
    // virtio disk attached the root lives on it (x86_64 legacy PCI,
    // RISC-V/AArch64 modern virtio-mmio) and that is what gets used.
    #[cfg(target_os = "minix")]
    let driver_label: &[u8] = b"virtio_blk";

    #[cfg(target_os = "minix")]
    let (r, node, _flags_reply) = unsafe {
        req_readsuper(
            vmp,
            driver_label.as_ptr(),
            driver_label.len(),
            0, /* dev: root block device */
            0, /* readonly: writable */
            1, /* isroot */
        )
    };
    #[cfg(not(target_os = "minix"))]
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

    /// Give the table a mounted filesystem on `dev`, with its root vnode; a filp may
    /// then hold that vnode to make it busy.
    unsafe fn mount_one(dev: u32, fs_e: i32) -> *mut Vnode {
        unsafe {
            let vmp = get_free_vmnt();
            (*vmp).m_fs_e = fs_e;
            (*vmp).m_dev = dev;
            (*vmp).m_root_node = 1;
            let vp = get_free_vnode();
            (*vp).v_fs_e = fs_e;
            (*vp).v_inode_nr = 1;
            (*vp).v_dev = dev;
            (*vp).v_ref_count = 1;
            (*vp).v_fs_count = 1;
            vp
        }
    }

    /// Point an open filp at `vp` (filps are what `unmount` treats as "in use").
    unsafe fn open_a_filp_on(vp: *mut Vnode) {
        unsafe {
            let glob = vfs_global();
            let filp_arr = addr_of_mut!((*glob).filp) as *mut crate::vfs::types::Filp;
            (*filp_arr).filp_count = 1;
            (*filp_arr).filp_vno = vp;
        }
    }

    #[test]
    fn unmount_reports_einval_when_nothing_is_mounted_there() {
        unsafe {
            init_tables();
            assert_eq!(unmount(7), EINVAL);
        }
    }

    #[test]
    fn unmount_refuses_a_device_with_an_open_filp() {
        unsafe {
            init_tables();
            let vp = mount_one(3, 42);
            open_a_filp_on(vp);
            assert_eq!(unmount(3), EBUSY);
            // The record is the only thing that says there is still something to
            // unmount, so a refusal must leave it alone.
            assert!(!find_vmnt_by_dev(3).is_null());
        }
    }

    #[test]
    fn unmount_learns_the_filesystems_answer_and_keeps_the_record_when_it_fails() {
        unsafe {
            init_tables();
            mount_one(3, 42);
            // On this build there is no filesystem to ask — `req_unmount` is the host
            // stub — so what this checks is that the answer reaches the caller and
            // that a failed unmount leaves the mount in place.
            assert_eq!(unmount(3), ENOSYS);
            assert!(!find_vmnt_by_dev(3).is_null());
        }
    }

    #[test]
    fn unmount_all_skips_a_mount_with_no_device() {
        unsafe {
            init_tables();
            // PFS and devman live on the same NO_DEV sentinel: nothing to put down, and
            // on the wasm boot nothing behind the record to ask.
            let vmp = get_free_vmnt();
            (*vmp).m_fs_e = 9;
            (*vmp).m_dev = u32::MAX;
            assert_eq!(unmount_all(1), 0);
        }
    }

    #[test]
    fn unmount_all_counts_what_is_still_mounted() {
        unsafe {
            init_tables();
            mount_one(3, 42);
            // The filesystem's answer decides: with the host stub refusing, the mount
            // stays, and the forced pass is the one that reports it.
            assert_eq!(unmount_all(0), 0);
            assert_eq!(unmount_all(1), 1);
        }
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

    #[test]
    fn test_find_vmnt_mounted_on_matches_fs_and_inode() {
        unsafe {
            init_tables();
            let glob = vfs_global();
            let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;

            // Simulate the devman mount: mounted on inode 5 of MFS (7).
            let vmp = &mut *vmnt_arr;
            vmp.m_fs = 7; // MFS_PROC_NR — the mounted-on dir's fs
            vmp.m_mounted_on = 5;
            vmp.m_fs_e = 15; // DEVMAN_PROC_NR

            // Exact (fs, inode) match.
            let hit = find_vmnt_mounted_on(7, 5);
            assert!(!hit.is_null());
            assert_eq!((*hit).m_fs_e, 15);

            // Wrong fs or wrong inode: no match.
            assert!(find_vmnt_mounted_on(7, 6).is_null());
            assert!(find_vmnt_mounted_on(9, 5).is_null());

            // A zeroed vmnt (no mount) never matches a real fs.
            init_vmnts();
            assert!(find_vmnt_mounted_on(7, 0).is_null());
        }
    }
}
