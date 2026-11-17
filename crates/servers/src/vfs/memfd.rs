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
//! operations that would otherwise reach a filesystem are answered here:
//!
//! * `read`/`write` go through the object's **cache pages** (VM's
//!   `VM_MAPCACHEPAGE` window, the same mechanism a filesystem uses for block
//!   I/O), not through a private buffer. So a `write` and a mapping of the same
//!   object see the same bytes, which is the property a memfd exists for; a
//!   separate store would drift from the mapping silently.
//! * `ftruncate` sets the vnode's size. Growth needs no blocks: a page appears
//!   when it is first written or faulted.
//! * `fstat` reports the vnode's identity, size and times, and
//!   `utimens` (`futimens`) stores those times in it: an object with no inode
//!   has nowhere else to keep them.
//! * `fsync` succeeds: there is no backing store to flush.
//! * `fstatvfs` describes the object's own pages (item 30). It is not a
//!   filesystem's capacity, because there is no filesystem.
//!
//! See `.notes/wayland.md` §6.4 for the design and §5 row 18 for the deviation.

use minix_std::fs::Stat;

use crate::vfs::call::{SELF, sys_vircopy};
use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::{Statvfs, Vnode};

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
    let (vp, _id, fd) =
        match crate::vfs::anon::create_anon(arch_common::com::MEMFD_DEV, MEMFD_MODE, 0) {
            Ok(v) => v,
            Err(e) => return e,
        };

    // A memfd has no inode, so its vnode is the only place its times can live. A
    // fresh object is as old as its creation; a clock that cannot be read leaves
    // them at zero rather than failing the create.
    if let Ok(ts) = minix_std::time::clock_gettime(minix_std::time::CLOCK_REALTIME) {
        unsafe {
            (*vp).v_atime = ts.tv_sec;
            (*vp).v_mtime = ts.tv_sec;
        }
    }

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
        st_atime: (*vp).v_atime,
        st_mtime: (*vp).v_mtime,
        st_ctime: (*vp).v_mtime,
    };
    let bytes = core::mem::size_of::<Stat>();
    sys_vircopy(SELF, &st as *const Stat as u64, user_ep, buf_addr, bytes)
}

/// Store the times `futimens` resolved for a memfd.
///
/// The caller has already applied `UTIME_NOW`/`UTIME_OMIT`, so these are the
/// values to keep. Second resolution: `Stat` cannot express nanoseconds.
///
/// # Safety
///
/// `vp` must be a live memfd vnode pointer.
pub unsafe fn set_times(vp: *mut Vnode, atime: i64, mtime: i64) {
    (*vp).v_atime = atime;
    (*vp).v_mtime = mtime;
}

/// The `statvfs` this object answers with: its own pages, not a filesystem's.
///
/// There is no storage to measure, so the object is the whole of it: the block
/// size is the page size, `f_blocks` is the object rounded up to pages, and
/// nothing is free. `f_fsid` carries the identity the object is keyed by, and
/// `f_namemax` is zero because nothing can be named in it. Describing a
/// filesystem here would be the wrong answer item 30 warns about; describing the
/// object is what a caller can act on.
///
/// # Safety
///
/// `vp` must be a live memfd vnode pointer.
pub unsafe fn build_statvfs(vp: *const Vnode) -> Statvfs {
    let size = (*vp).v_size.max(0) as u64;
    Statvfs {
        f_flags: 0,
        f_bsize: PAGE as u32,
        f_frsize: PAGE as u32,
        f_blocks: size.div_ceil(PAGE),
        f_bfree: 0,
        f_bavail: 0,
        f_files: 1,
        f_ffree: 0,
        f_favail: 0,
        f_fsid: (*vp).v_dev as u64,
        f_flag: 0,
        f_namemax: 0,
    }
}

/// `fstatvfs` on a memfd, built here rather than asked of a filesystem.
///
/// # Safety
///
/// `vp` must be a live memfd vnode pointer.
pub unsafe fn fstatvfs(vp: *const Vnode, user_ep: i32, buf_addr: u64) -> i32 {
    let st = unsafe { build_statvfs(vp) };
    let bytes = core::mem::size_of::<Statvfs>();
    sys_vircopy(SELF, &st as *const Statvfs as u64, user_ep, buf_addr, bytes)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn memfd_vnode(size: i64) -> Vnode {
        Vnode {
            v_dev: arch_common::com::MEMFD_DEV,
            v_inode_nr: 7,
            v_mode: MEMFD_MODE,
            v_size: size,
            v_ref_count: 1,
            ..Default::default()
        }
    }

    #[test]
    fn test_statvfs_describes_the_object_not_a_filesystem() {
        // There is no filesystem to measure. The answer has to be about the object:
        // page-sized blocks, the object's own pages, one file, nothing free. Asking
        // a caller to treat it as a filesystem's capacity would be the wrong answer.
        let st = unsafe { build_statvfs(&memfd_vnode(8192)) };
        assert_eq!(st.f_bsize, 4096);
        assert_eq!(st.f_frsize, 4096);
        assert_eq!(st.f_blocks, 2);
        assert_eq!(st.f_bfree, 0);
        assert_eq!(st.f_bavail, 0);
        assert_eq!(st.f_files, 1);
        assert_eq!(st.f_ffree, 0);
        assert_eq!(st.f_favail, 0);
        assert_eq!(st.f_fsid, arch_common::com::MEMFD_DEV as u64);
        assert_eq!(
            st.f_namemax, 0,
            "nothing can be named in an anonymous object"
        );

        // A partial page still costs a page.
        assert_eq!(unsafe { build_statvfs(&memfd_vnode(1)) }.f_blocks, 1);
        // And a size the arithmetic must not wrap on reads as empty.
        assert_eq!(unsafe { build_statvfs(&memfd_vnode(-1)) }.f_blocks, 0);
    }

    #[test]
    fn test_set_times_keeps_seconds_on_the_vnode() {
        let mut v = memfd_vnode(4096);
        assert_eq!((v.v_atime, v.v_mtime), (0, 0));
        unsafe { set_times(&mut v, 1_700_000_000, 1_700_000_001) };
        assert_eq!((v.v_atime, v.v_mtime), (1_700_000_000, 1_700_000_001));
    }
}
