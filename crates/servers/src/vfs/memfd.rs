//! Anonymous shared-memory objects — `memfd_create`.
//!
//! MINIX 3.3.0 has no `memfd`. Its closest relative is SysV shared memory, but
//! `wl_shm` — Wayland's shared-buffer pool, and so the thing that puts a client's
//! pixels in front of the compositor — needs an object a process can hold as a
//! **file descriptor**, pass over a socket (`SCM_RIGHTS`), and map in another
//! address space. That is what this provides.
//!
//! The object is given an *identity* rather than a filesystem: a vnode whose
//! `v_dev` is [`MEMFD_DEV`] and whose `v_inode_nr` is a never-reused id. VM's
//! file-page machinery then treats that identity like any file's, which is the
//! whole trick — the pages of a `MAP_SHARED` mapping of a memfd are the
//! `(dev, ino, offset)` entries of VM's page cache (`vm/cache.rs`), so every
//! mapping of one memfd shares one set of frames, across processes as much as
//! within one. A memfd therefore needs no new mapping machinery at all.
//!
//! Nothing here involves a filesystem server. `v_fs_count` stays 0, which is
//! what makes `put_vnode` skip its `req_putnode` without a special case, and the
//! four operations that would otherwise reach a filesystem — read, write,
//! truncate and stat — are answered here:
//!
//! * `read`/`write` go through the object's **cache pages** (VM's
//!   `VM_MAPCACHEPAGE` window, the same mechanism a filesystem uses for block
//!   I/O), not through a private buffer. So a `write` and a mapping of the same
//!   object see the same bytes, which is the property a memfd exists for; a
//!   separate store would drift from the mapping silently.
//! * `ftruncate` sets the vnode's size. Growth needs no blocks: a page appears
//!   when it is first written or faulted.
//!
//! See `WAYLAND.md` §6.4 for the design and §5 row 18 for the deviation.

use minix_std::fs::Stat;

use crate::vfs::call::{SELF, sys_vircopy};
use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::Vnode;

/// `S_IFREG` with the permissions a memfd is created with.
const MEMFD_MODE: u32 = S_IFREG | 0o600;

/// Page size for the offset arithmetic; the kernel's is the same.
const PAGE: u64 = 4096;

/// True for a memfd's vnode.
///
/// # Safety
///
/// `vp` must be a live vnode pointer.
pub unsafe fn is_memfd(vp: *const Vnode) -> bool {
    !vp.is_null() && (*vp).v_dev == arch_common::com::MEMFD_DEV
}

/// `memfd_create(name, flags)`: one anonymous object, returned as an fd.
///
/// The name is ignored, as it is in the reference's absence of the call: a
/// memfd has no directory entry to be named in.
pub fn do_create() -> i32 {
    unsafe { create_inner() }
}

/// # Safety
///
/// Reaches the global fproc and filp tables; only VFS's own dispatch may call it.
unsafe fn create_inner() -> i32 {
    let (_vp, _id, fd) =
        match crate::vfs::anon::create_anon(arch_common::com::MEMFD_DEV, MEMFD_MODE, 0) {
            Ok(v) => v,
            Err(e) => return e,
        };

    // `memfd_create`'s MFD_CLOEXEC, bit 0 of its flags.
    if r_i32_msg(8) & 1 != 0 {
        crate::vfs::anon::set_cloexec(fd);
    }

    crate::vfs::anon::reply_fd(fd);
    OK
}

/// `ftruncate` on a memfd: the size is the vnode's, and no blocks are involved.
///
/// Shrinking leaves any page already cached beyond the new end in place; nothing
/// reads it (every access clamps to the size) and VM evicts it on pressure.
///
/// # Safety
///
/// `vp` must be a live memfd vnode pointer.
pub unsafe fn truncate(vp: *mut Vnode, newsize: i64) -> i32 {
    if newsize < 0 {
        return EINVAL;
    }
    (*vp).v_size = newsize;
    OK
}

/// `fstat` on a memfd, built here rather than asked of a filesystem.
///
/// # Safety
///
/// `vp` must be a live memfd vnode pointer.
pub unsafe fn fstat(vp: *const Vnode, user_ep: i32, buf_addr: u64) -> i32 {
    let st = Stat {
        st_dev: (*vp).v_dev as u64,
        st_ino: (*vp).v_inode_nr as u64,
        st_mode: (*vp).v_mode,
        // Anonymous: nothing names it, so nothing links to it.
        st_nlink: 0,
        st_uid: (*vp).v_uid as u32,
        st_gid: (*vp).v_gid as u32,
        st_rdev: 0,
        st_size: (*vp).v_size,
        st_blksize: 0,
        st_blocks: 0,
        st_atime: 0,
        st_mtime: 0,
        st_ctime: 0,
    };
    let bytes = core::mem::size_of::<Stat>();
    sys_vircopy(SELF, &st as *const Stat as u64, user_ep, buf_addr, bytes)
}

/// `read`/`write` on a memfd, through the object's cache pages.
///
/// `pos` is the file offset and `count` the caller's byte count. Both directions
/// stop at the object's end — a `write` past it does not grow the object, as a
/// memfd's does not.
pub fn transfer(
    user_ep: i32,
    ino: u32,
    size: i64,
    pos: i64,
    buf: u64,
    count: usize,
    write: bool,
) -> i32 {
    if count == 0 || pos < 0 {
        return if pos < 0 { EINVAL } else { 0 };
    }
    let avail = if size > pos { (size - pos) as usize } else { 0 };
    let want = count.min(avail);
    if want == 0 {
        return 0;
    }

    let mut done = 0usize;
    while done < want {
        let off = (pos + done as i64) as u64;
        let page = off & !(PAGE - 1);
        let page_off = off - page;
        let chunk = ((PAGE - page_off) as usize).min(want - done);

        let window = match map_pages(ino, page, 1) {
            Ok(va) => va,
            // A page that cannot be created: report what did move, so a partial
            // transfer is not lost.
            Err(e) => return if done > 0 { done as i32 } else { e },
        };
        let local = window + page_off;
        let r = if write {
            unsafe { sys_vircopy(user_ep, buf + done as u64, SELF, local, chunk) }
        } else {
            unsafe { sys_vircopy(SELF, local, user_ep, buf + done as u64, chunk) }
        };
        unmap_pages(window, PAGE);
        if r != 0 {
            return if done > 0 { done as i32 } else { r };
        }
        done += chunk;
    }
    done as i32
}

/// Map `pages` of the object's cache pages into VFS's own address space,
/// writable (VM `VM_MAPCACHEPAGE`), returning the base address.
///
/// This is the mechanism a filesystem uses to get a window on a cached block; a
/// memfd borrows it so its `read`/`write` touch the same frames a mapping does,
/// rather than a buffer of their own. VM allocates and zero-fills the page if the
/// object has none there yet.
fn map_pages(ino: u32, page: u64, pages: u32) -> Result<u64, i32> {
    #[cfg(target_os = "minix")]
    unsafe {
        let mut msg = [0u8; 64];
        msg[4..8].copy_from_slice(&arch_common::com::VM_MAPCACHEPAGE.to_le_bytes());
        // m1i1 = dev, m1i2:m1i3 = dev_offset, m1i4 = ino,
        // m1i5:m1i6 = ino_offset, m1i7 = pages. A memfd has no device offset —
        // its pages are keyed by the file offset alone.
        msg[8..12].copy_from_slice(&arch_common::com::MEMFD_DEV.to_ne_bytes());
        msg[12..16].copy_from_slice(&0u32.to_ne_bytes());
        msg[16..20].copy_from_slice(&0u32.to_ne_bytes());
        msg[20..24].copy_from_slice(&ino.to_ne_bytes());
        msg[24..32].copy_from_slice(&page.to_ne_bytes());
        msg[32..36].copy_from_slice(&pages.to_ne_bytes());

        let r = minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::VM_PROC_NR as u64,
            msg.as_mut_ptr() as u64,
        );
        if r < 0 {
            return Err(r as i32);
        }
        let status = i32::from_ne_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
        if status != 0 {
            return Err(status);
        }
        let lo = i32::from_ne_bytes(msg[8..12].try_into().unwrap_or([0; 4]));
        let hi = i32::from_ne_bytes(msg[12..16].try_into().unwrap_or([0; 4]));
        Ok(((hi as u32 as u64) << 32) | (lo as u32 as u64))
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (ino, page, pages, ENOSYS);
        Err(ENOSYS)
    }
}

/// Release a window [`map_pages`] returned (VM `VM_MUNMAP`).
fn unmap_pages(addr: u64, len: u64) {
    #[cfg(target_os = "minix")]
    unsafe {
        let mut msg = [0u8; 64];
        msg[4..8].copy_from_slice(&arch_common::com::VM_MUNMAP.to_le_bytes());
        // VM_MUNMAP reads the length and address at these payload offsets.
        msg[20..28].copy_from_slice(&len.to_ne_bytes());
        msg[28..36].copy_from_slice(&addr.to_ne_bytes());
        let _ = minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::VM_PROC_NR as u64,
            msg.as_mut_ptr() as u64,
        );
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (addr, len);
    }
}

/// Read an `i32` field of the current VFS request message.
///
/// # Safety
///
/// Reads the global request buffer.
unsafe fn r_i32_msg(off: usize) -> i32 {
    let glob = vfs_global();
    i32::from_ne_bytes(
        (&(*glob).fs_m_in)[off..off + 4]
            .try_into()
            .unwrap_or([0; 4]),
    )
}
