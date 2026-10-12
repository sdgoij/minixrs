//! dma-buf descriptors — the render node's PRIME handoff (`.notes/wayland.md` §6.10,
//! stage 3d-b-2).
//!
//! A GEM handle is not shareable: it names an object inside the render node, and no
//! other process can see it. `DRM_IOCTL_PRIME_HANDLE_TO_FD` is what turns one into
//! something another process can be handed — a *descriptor* — and
//! `DRM_IOCTL_PRIME_FD_TO_HANDLE` is the other direction. Descriptors belong to
//! VFS, not to a device, so the two PRIME ioctls are served here rather than by the
//! node: VFS asks the node only for the object's own name (its *export token*),
//! wraps that name in an anonymous descriptor, and turns a descriptor back into a
//! token when the receiving process imports it. This is the same division the
//! socket control path uses (§6.2): the transport carries a rendezvous, VFS owns the
//! descriptors, and the driver is asked only what it alone knows.
//!
//! The descriptor is a VFS-internal object (`DMABUF_DEV` and a never-reused id) with
//! no filesystem (`v_fs_e` = NONE, `v_fs_count` = 0), exactly like a memfd or an
//! eventfd. What makes it a *dma-buf* is that its identity is a render node's
//! object: the token the node handed back, which is also what it answers `mmap` with.
//! It can be passed (`SCM_RIGHTS`) because it is a descriptor like any other. It has
//! no `mmap` of its own yet — a consumer imports it to a GEM handle and maps that —
//! which is the piece 3d-b-4 adds when a buffer must be mapped from the descriptor
//! alone.
//!
//! A descriptor does **not** hold a reference on the object it names: this node has
//! one handle space and no object refcount (`§6.10` D3's deviation), so a `GEM_CLOSE`
//! after an export makes the token stale and a later import `ENOENT` — an fd that
//! outlives its object is refused rather than silently re-aimed at a reused slot.

use minix_std::fs::Stat;

use crate::vfs::call::{SELF, sys_vircopy};
use crate::vfs::consts::*;
use crate::vfs::glo::{current_fp, vfs_global};
use crate::vfs::types::{Filp, Vnode};
use drivers::video::drm;

/// `struct drm_prime_handle`: `u32 handle`, `u32 flags`, `s32 fd`.
const PRIME_FLAGS_OFF: usize = 4;
const PRIME_FD_OFF: usize = 8;

/// `S_IFREG` with the permissions a dma-buf descriptor is created with.
const DMABUF_MODE: u32 = S_IFREG | 0o600;

/// True for a dma-buf descriptor's vnode.
///
/// # Safety
///
/// `vp` must be a live vnode pointer.
pub unsafe fn is_dmabuf(vp: *const Vnode) -> bool {
    !vp.is_null() && (*vp).v_dev == arch_common::com::DMABUF_DEV
}

/// Handle an intercepted PRIME ioctl on a render node, or `None` for any other
/// request (which then travels to the driver as usual).
///
/// `dev` is the render node's device number, `user_ep`/`va` the calling process and
/// its `struct drm_prime_handle`, and `flags` the opened node descriptor's `O_*` flags,
/// passed through to the driver as the nonblock flag the generic path uses.
pub fn prime_ioctl(request: u32, dev: u32, user_ep: i32, va: u64, flags: i32) -> Option<i32> {
    match request {
        drm::DRM_IOCTL_PRIME_HANDLE_TO_FD => Some(export(dev, user_ep, va, flags)),
        drm::DRM_IOCTL_PRIME_FD_TO_HANDLE => Some(import(dev, user_ep, va, flags)),
        _ => None,
    }
}

/// `PRIME_HANDLE_TO_FD`: ask the node to name the object, then wrap that name in a
/// descriptor and hand the descriptor back in the ABI's `fd` word.
fn export(dev: u32, user_ep: i32, va: u64, flags: i32) -> i32 {
    // The node is asked first: only it can say whether the handle names an object, and
    // the token it answers with is the object's own name. Its `dispatch` writes that
    // token into the struct's `fd` word; the real descriptor replaces it below, so the
    // caller never sees the token.
    let r = crate::vfs::device::cdev_io(
        CDEV_IOCTL,
        dev,
        user_ep,
        va,
        0,
        drm::DRM_IOCTL_PRIME_HANDLE_TO_FD as u64,
        flags,
    );
    if r != OK {
        return r;
    }
    let mut token_bytes = [0u8; 4];
    if let Err(e) = copy_in(user_ep, va + PRIME_FD_OFF as u64, &mut token_bytes) {
        return e;
    }
    let token = u32::from_ne_bytes(token_bytes);

    // The descriptor's identity *is* the token, so importing it needs nothing but the
    // number the node hands back.
    let (vp, _id, fd) = match unsafe {
        crate::vfs::anon::create_anon(arch_common::com::DMABUF_DEV, DMABUF_MODE, 0)
    } {
        Ok(v) => v,
        Err(e) => return e,
    };
    unsafe { (*vp).v_inode_nr = token };

    // The ABI's export flags are the descriptor's: `DRM_CLOEXEC` asks for close-on-exec,
    // and `DRM_RDWR` is the read/write mode the anonymous filp already has.
    let mut flag_bytes = [0u8; 4];
    if let Err(e) = copy_in(user_ep, va + PRIME_FLAGS_OFF as u64, &mut flag_bytes) {
        return e;
    }
    if u32::from_ne_bytes(flag_bytes) & drm::DRM_CLOEXEC != 0 {
        unsafe { crate::vfs::anon::set_cloexec(fd) };
    }

    match copy_out(user_ep, va + PRIME_FD_OFF as u64, &fd.to_ne_bytes()) {
        Ok(()) => OK,
        Err(e) => e,
    }
}

/// `PRIME_FD_TO_HANDLE`: resolve the caller's descriptor to the token it carries, and
/// let the node name the handle back from it.
fn import(dev: u32, user_ep: i32, va: u64, flags: i32) -> i32 {
    let mut fd_bytes = [0u8; 4];
    if let Err(e) = copy_in(user_ep, va + PRIME_FD_OFF as u64, &mut fd_bytes) {
        return e;
    }
    let fd = i32::from_ne_bytes(fd_bytes);
    let token = match token_of_fd(fd) {
        Ok(token) => token,
        Err(e) => return e,
    };

    // The node's side of the request is the token, not the descriptor, so the token goes
    // where an import reads it. The descriptor is put back afterwards: the caller's
    // struct is unchanged but for the handle it asked for, as the ABI leaves it.
    if let Err(e) = copy_out(user_ep, va + PRIME_FD_OFF as u64, &token.to_ne_bytes()) {
        return e;
    }
    let r = crate::vfs::device::cdev_io(
        CDEV_IOCTL,
        dev,
        user_ep,
        va,
        0,
        drm::DRM_IOCTL_PRIME_FD_TO_HANDLE as u64,
        flags,
    );
    if r == OK {
        // Put the caller's descriptor back: the ABI leaves the input word as it was sent.
        if let Err(e) = copy_out(user_ep, va + PRIME_FD_OFF as u64, &fd.to_ne_bytes()) {
            return e;
        }
    }
    r
}

/// The export token a descriptor of the caller's carries, or `EBADF` for a descriptor
/// it does not hold and `EINVAL` for one that is not a dma-buf.
fn token_of_fd(fd: i32) -> Result<u32, i32> {
    let fp = unsafe { current_fp() };
    if fp.is_null() {
        return Err(EINVAL);
    }
    let idx = unsafe { crate::vfs::filedes::get_filp(fd, &*fp) };
    if idx < 0 {
        return Err(EBADF);
    }
    let vp = unsafe {
        let glob = vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        (*filp_arr.add(idx as usize)).filp_vno
    };
    if vp.is_null() || !unsafe { is_dmabuf(vp) } {
        // Import is defined only for a dma-buf descriptor; anything else is a request
        // about the wrong kind of object.
        return Err(EINVAL);
    }
    Ok(unsafe { (*vp).v_inode_nr })
}

/// `fstat` on a dma-buf, built here rather than asked of a filesystem.
///
/// The length is 0: the object's length lives on the node, and nothing a client does
/// with a dma-buf descriptor needs it here (a consumer imports the descriptor and reads
/// the length from the handle). Reporting the file kind is what lets `fdopen`-style
/// code tell it apart from a socket or a tty.
///
/// # Safety
///
/// `vp` must be a live dma-buf vnode pointer.
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
        st_size: 0,
        st_blksize: 0,
        st_blocks: 0,
        st_atime: 0,
        st_mtime: 0,
        st_ctime: 0,
    };
    let bytes = core::mem::size_of::<Stat>();
    sys_vircopy(SELF, &st as *const Stat as u64, user_ep, buf_addr, bytes)
}

/// Copy the caller's argument struct in.
fn copy_in(user_ep: i32, va: u64, dst: &mut [u8]) -> Result<(), i32> {
    let r = unsafe { sys_vircopy(user_ep, va, SELF, dst.as_mut_ptr() as u64, dst.len()) };
    if r == OK { Ok(()) } else { Err(r) }
}

/// Copy a word of the answer back to the caller.
fn copy_out(user_ep: i32, va: u64, src: &[u8]) -> Result<(), i32> {
    let r = unsafe { sys_vircopy(SELF, src.as_ptr() as u64, user_ep, va, src.len()) };
    if r == OK { Ok(()) } else { Err(r) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two request numbers this module answers are the driver's, and the struct it
    /// reads is the size the numbers carry: a mismatch is a request that never reaches
    /// VFS at all (it would fall through to the driver) or one whose argument overruns
    /// the grant. The fd is the last field, so the struct's size is its offset plus four.
    #[test]
    fn the_request_numbers_and_the_struct_match_the_driver() {
        let size = (PRIME_FD_OFF + 4) as u32;
        assert_eq!(drm::arg_size(drm::DRM_IOCTL_PRIME_HANDLE_TO_FD), size);
        assert_eq!(drm::arg_size(drm::DRM_IOCTL_PRIME_FD_TO_HANDLE), size);
        // Both are `_IOWR`: an argument travels in and an answer comes back.
        assert!(drm::carries_in(drm::DRM_IOCTL_PRIME_HANDLE_TO_FD));
        assert!(drm::carries_out(drm::DRM_IOCTL_PRIME_HANDLE_TO_FD));
        assert!(drm::carries_in(drm::DRM_IOCTL_PRIME_FD_TO_HANDLE));
        assert!(drm::carries_out(drm::DRM_IOCTL_PRIME_FD_TO_HANDLE));
    }

    /// Only the two PRIME requests are VFS's; every other DRM request falls through to
    /// the driver, which is what keeps this interception from swallowing the node's
    /// whole ABI.
    #[test]
    fn only_the_prime_requests_are_intercepted() {
        assert!(prime_ioctl(drm::DRM_IOCTL_PRIME_HANDLE_TO_FD, 0, 0, 0, 0).is_some());
        assert!(prime_ioctl(drm::DRM_IOCTL_PRIME_FD_TO_HANDLE, 0, 0, 0, 0).is_some());
        assert!(prime_ioctl(drm::DRM_IOCTL_GET_CAP, 0, 0, 0, 0).is_none());
        assert!(prime_ioctl(drm::DRM_IOCTL_VIRTGPU_MAP, 0, 0, 0, 0).is_none());
    }

    /// The field offsets are the ABI's: a `struct drm_prime_handle` puts the flags at 4
    /// and the fd at 8, and getting either wrong would read the flags as a descriptor (or
    /// write one over them).
    #[test]
    fn the_fields_are_at_the_abis_offsets() {
        assert_eq!(PRIME_FLAGS_OFF, 4);
        assert_eq!(PRIME_FD_OFF, 8);
    }
}
