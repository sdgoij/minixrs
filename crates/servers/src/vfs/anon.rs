//! Anonymous, VFS-owned objects — a descriptor with no filesystem behind it.
//!
//! `memfd_create` and `eventfd` both make a vnode no filesystem serves:
//! `v_fs_e`/`v_mapfs_e` are `NONE` and `v_fs_count` stays 0, which is what makes
//! `put_vnode` skip its `req_putnode` rather than send one to an endpoint of
//! `NONE`. They differ only in the identity they take (`v_dev`, and in what VFS
//! answers on the descriptor), so the vnode + descriptor + filp wiring is shared
//! here.
//!
//! The identity's inode number is a never-reused id shared across kinds; its
//! uniqueness is load-bearing where the object's `(dev, ino)` keys shared state
//! (VM's page cache for a memfd).

use core::sync::atomic::{AtomicU32, Ordering};

use crate::vfs::consts::*;
use crate::vfs::types::{Filp, Vnode};

/// Never-reused object ids, shared across anonymous object kinds.
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Allocate an anonymous object's vnode, descriptor and filp, wired together.
///
/// `v_dev`, `v_mode` and `v_size` come from the arguments; ownership from the
/// calling fproc. Returns the vnode pointer, its new object id, and the fd the
/// caller should report.
///
/// # Safety
///
/// Reaches the global fproc/filp/vnode tables; only VFS dispatch may call it, and
/// only where `glob.fp` names the caller.
pub unsafe fn create_anon(dev: u32, mode: u32, size: i64) -> Result<(*mut Vnode, u32, i32), i32> {
    let glob = crate::vfs::glo::vfs_global();
    let fp = (*glob).fp;
    if fp.is_null() {
        return Err(EINVAL);
    }

    let vp = crate::vfs::mount::get_free_vnode();
    if vp.is_null() {
        return Err(ENFILE);
    }
    crate::vfs::mount::lock_vnode(vp, VNODE_OPCL);

    let mut fd = 0i32;
    let r = crate::vfs::filedes::get_fd(&mut *fp, 0, &mut fd);
    if r != OK {
        crate::vfs::mount::unlock_vnode(vp);
        return Err(r);
    }
    let filp = crate::vfs::filedes::alloc_filp();
    if filp < 0 {
        (*fp).fp_filp[fd as usize] = -1;
        crate::vfs::mount::unlock_vnode(vp);
        return Err(ENFILE);
    }
    (*fp).fp_filp[fd as usize] = filp;

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    // No filesystem serves this vnode (see the module docs on `put_vnode`).
    (*vp).v_fs_e = NONE_ENDPOINT;
    (*vp).v_mapfs_e = NONE_ENDPOINT;
    (*vp).v_inode_nr = id;
    (*vp).v_mode = mode;
    (*vp).v_size = size;
    (*vp).v_dev = dev;
    (*vp).v_fs_count = 0;
    (*vp).v_ref_count = 1;
    (*vp).v_uid = (*fp).fp_effuid as i32;
    (*vp).v_gid = (*fp).fp_effgid as i32;

    let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
    let f = &mut *filp_arr.add(filp as usize);
    f.filp_mode = crate::vfs::protect::R_BIT | crate::vfs::protect::W_BIT;
    f.filp_vno = vp;

    crate::vfs::mount::unlock_vnode(vp);
    Ok((vp, id, fd))
}

/// Set `FD_CLOEXEC` on a descriptor the caller just created.
///
/// # Safety
///
/// `glob.fp` must name the calling fproc and `fd` must be a descriptor it holds.
pub unsafe fn set_cloexec(fd: i32) {
    let glob = crate::vfs::glo::vfs_global();
    let fp = (*glob).fp;
    if !fp.is_null() && fd >= 0 {
        (*fp).fp_cloexec |= 1u64 << fd;
    }
}

/// Write a created descriptor into the outgoing reply (VFS convention: the
/// result slot at `fs_m_out[8..12]`).
///
/// # Safety
///
/// Must be called from VFS dispatch.
pub unsafe fn reply_fd(fd: i32) {
    let glob = crate::vfs::glo::vfs_global();
    (&mut (*glob).fs_m_out)[8..12].copy_from_slice(&fd.to_le_bytes());
}
