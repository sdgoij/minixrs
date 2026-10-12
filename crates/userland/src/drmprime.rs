//! `/bin/drmprime` — a render node's object exported to a descriptor and imported back
//! (`.notes/wayland.md` §6.10, stage 3d-b-2).
//!
//! PRIME is the handoff that makes a node's object shareable: `HANDLE_TO_FD` turns a GEM
//! handle into a descriptor another process can be given, and `FD_TO_HANDLE` is the way
//! back. What this client proves is the round trip and, with it, that the thing in the
//! middle is a *descriptor* and not the node's own name for the object: it exports, checks
//! the descriptor's kind and device with `fstat`, imports it back to the object's handle,
//! and reads the bytes it wrote before the export through a mapping of the handle it got
//! back. Two refusals surround it — an export of a handle the node does not hold, and an
//! import of a descriptor that is not a dma-buf — and a third closes the loop: once the
//! object is closed its exported descriptor is refused rather than re-aimed at a reused slot.
//!
//! Like `/bin/drmmap` it declares the ABI itself — request numbers and argument structs
//! written out the way a C client writes them — so that its agreement with the server is
//! evidence rather than a tautology.

use crate::{Decimal, append, write_err, write_out};

/// The render node.
const NODE: &[u8] = b"/dev/dri/renderD128";

/// `_IOWR` / `_IOW` from `asm-generic/ioctl.h`.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}
const fn iow(type_: u32, nr: u32, size: u32) -> u32 {
    (1u32 << 30) | (type_ << 8) | (size << 16) | nr
}

const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_IOCTL_GET_CAP: u32 = iowr(DRM_IOCTL_BASE, 0x0c, 16);
const DRM_IOCTL_GEM_CLOSE: u32 = iow(DRM_IOCTL_BASE, 0x09, 8);
const DRM_IOCTL_PRIME_HANDLE_TO_FD: u32 = iowr(DRM_IOCTL_BASE, 0x2d, 12);
const DRM_IOCTL_PRIME_FD_TO_HANDLE: u32 = iowr(DRM_IOCTL_BASE, 0x2e, 12);
const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x01, 16);
const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x04, 56);
const DRM_IOCTL_VIRTGPU_RESOURCE_INFO: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x05, 16);

const DRM_CAP_PRIME: u64 = 0x5;
const DRM_PRIME_CAP_IMPORT: u64 = 0x1;
const DRM_PRIME_CAP_EXPORT: u64 = 0x2;

/// The export flags the ABI accepts: the port's `O_RDWR` and `O_CLOEXEC`, which the node
/// passes straight through to the descriptor VFS makes.
const DRM_RDWR: u32 = 0o02;
const DRM_CLOEXEC: u32 = 0o200000;

/// The anonymous major a dma-buf descriptor's device carries
/// (`arch_common::com::DMABUF_MAJOR`). `fstat` on the exported descriptor is checked
/// against it: a name the node handed back would not have a device number at all.
const DMABUF_MAJOR: u64 = 25;

/// The object this asks for: 64×64 B8G8R8X8, four bytes a pixel — already a whole number of
/// pages, so the object's length is the length asked for.
const SIZE: usize = 64 * 64 * 4;
const FORMAT_B8G8R8X8: u32 = 2;
const TARGET_TEXTURE_2D: u32 = 2;
/// `VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW`, the bind `/bin/drmmap` uses.
const BIND_TEXTURE: u32 = (1 << 1) | (1 << 3);

/// `struct drm_get_cap`.
#[repr(C)]
struct GetCap {
    capability: u64,
    value: u64,
}

/// `struct drm_prime_handle`: the handle in (or out), the flags, and the descriptor that is
/// out for an export and in for an import.
#[repr(C)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

/// `struct drm_virtgpu_resource_create`.
#[repr(C)]
struct ResourceCreate {
    target: u32,
    format: u32,
    bind: u32,
    width: u32,
    height: u32,
    depth: u32,
    array_size: u32,
    last_level: u32,
    nr_samples: u32,
    flags: u32,
    bo_handle: u32,
    res_handle: u32,
    size: u32,
    stride: u32,
}

/// `struct drm_virtgpu_resource_info`.
#[repr(C)]
struct ResourceInfo {
    bo_handle: u32,
    res_handle: u32,
    size: u32,
    blob_mem: u32,
}

/// `struct drm_virtgpu_map`: the handle in, the `mmap` offset out.
#[repr(C)]
struct Map {
    offset: u64,
    handle: u32,
    pad: u32,
}

/// `struct drm_gem_close`.
#[repr(C)]
struct GemClose {
    handle: u32,
    pad: u32,
}

/// An ioctl whose result is a `Result` rather than an errno in a buffer: `MinixErr` keeps the
/// code positive, so the `Err` here reads as the number a C client would see.
fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<i32, i32> {
    match unsafe { minix_std::fs::ioctl(fd, request, arg) } {
        Ok(code) => Ok(code),
        Err(e) => Err(e.0),
    }
}

/// `DRM_IOCTL_GET_CAP`: the value of a generic capability.
fn get_cap(fd: i32, capability: u64) -> Result<u64, i32> {
    let mut arg = GetCap {
        capability,
        value: 0,
    };
    ioctl(
        fd,
        DRM_IOCTL_GET_CAP,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )?;
    Ok(arg.value)
}

/// Create one 2D object and return its handle.
fn create(fd: i32, width: u32, height: u32) -> Result<u32, i32> {
    let mut arg = ResourceCreate {
        target: TARGET_TEXTURE_2D,
        format: FORMAT_B8G8R8X8,
        bind: BIND_TEXTURE,
        width,
        height,
        depth: 1,
        array_size: 1,
        last_level: 0,
        nr_samples: 0,
        flags: 0,
        bo_handle: 0,
        res_handle: 0,
        size: (width as usize * height as usize * 4) as u32,
        stride: width * 4,
    };
    ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_RESOURCE_CREATE,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )?;
    Ok(arg.bo_handle)
}

/// `DRM_IOCTL_VIRTGPU_RESOURCE_INFO`: the object's resource id and its length.
fn info(fd: i32, handle: u32) -> Result<(u32, u32), i32> {
    let mut arg = ResourceInfo {
        bo_handle: handle,
        res_handle: 0,
        size: 0,
        blob_mem: 0,
    };
    ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_RESOURCE_INFO,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )?;
    Ok((arg.res_handle, arg.size))
}

/// `DRM_IOCTL_VIRTGPU_MAP`: the offset this object's memory is mapped at.
fn map_offset(fd: i32, handle: u32) -> Result<u64, i32> {
    let mut arg = Map {
        offset: 0,
        handle,
        pad: 0,
    };
    ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_MAP,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )?;
    Ok(arg.offset)
}

/// `DRM_IOCTL_PRIME_HANDLE_TO_FD`: export `handle` to a descriptor.
///
/// The descriptor is VFS's, so the number in the `fd` field is a real descriptor the moment
/// this returns; the flags travel to it.
fn prime_export(fd: i32, handle: u32) -> Result<i32, i32> {
    let mut arg = PrimeHandle {
        handle,
        flags: DRM_RDWR | DRM_CLOEXEC,
        fd: -1,
    };
    ioctl(
        fd,
        DRM_IOCTL_PRIME_HANDLE_TO_FD,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )?;
    Ok(arg.fd)
}

/// `DRM_IOCTL_PRIME_FD_TO_HANDLE`: import `prime_fd` back to the object's handle.
fn prime_import(fd: i32, prime_fd: i32) -> Result<u32, i32> {
    let mut arg = PrimeHandle {
        handle: 0,
        flags: 0,
        fd: prime_fd,
    };
    ioctl(
        fd,
        DRM_IOCTL_PRIME_FD_TO_HANDLE,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )?;
    Ok(arg.handle)
}

/// `DRM_IOCTL_GEM_CLOSE`.
fn close_object(fd: i32, handle: u32) -> Result<(), i32> {
    let mut arg = GemClose { handle, pad: 0 };
    ioctl(
        fd,
        DRM_IOCTL_GEM_CLOSE,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )
    .map(|_| ())
}

/// The byte written to, and expected at, index `i`. Never zero, so "every byte survived" is a
/// count a fresh slot's zeroed memory cannot satisfy.
fn pattern(i: usize) -> u8 {
    ((i as u8) | 1) ^ 0x5A
}

/// Map `handle` `SIZE` bytes wide and return where.
fn map_object(fd: i32, handle: u32) -> Result<*mut u8, i32> {
    let offset = map_offset(fd, handle)?;
    unsafe {
        minix_std::vmem::mmap_status(
            core::ptr::null_mut(),
            SIZE,
            minix_std::vmem::PROT_READ | minix_std::vmem::PROT_WRITE,
            minix_std::vmem::MAP_SHARED,
            fd,
            offset as i64,
        )
    }
}

/// A status line's field: the name, then a decimal value.
fn field(line: &mut [u8], at: &mut usize, name: &[u8], value: u32) {
    append(line, at, name);
    append(line, at, Decimal::of(value).bytes());
}

/// Export an object to a descriptor, check the descriptor, import it back, and read the
/// object's bytes through the handle the import returned.
pub fn drmprime(_args: &[&str]) -> i32 {
    let fd = match unsafe { minix_std::fs::open(NODE, minix_std::fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            write_err(b"drmprime: no render node at /dev/dri/renderD128\n");
            return 1;
        }
    };

    // What the node says it can do first: a node without PRIME has nothing else here to test.
    let cap = match get_cap(fd, DRM_CAP_PRIME) {
        Ok(v) => v,
        Err(e) => return fail(b"drmprime: GET_CAP failed, errno ", e),
    };
    let want_cap = DRM_PRIME_CAP_IMPORT | DRM_PRIME_CAP_EXPORT;
    if cap != want_cap {
        write_err(b"drmprime: DRM_CAP_PRIME is not import|export\n");
        return 1;
    }

    let handle = match create(fd, 64, 64) {
        Ok(h) => h,
        Err(e) => return fail(b"drmprime: RESOURCE_CREATE failed, errno ", e),
    };
    let (_res, size) = match info(fd, handle) {
        Ok(v) => v,
        Err(e) => return fail(b"drmprime: RESOURCE_INFO failed, errno ", e),
    };
    if size as usize != SIZE {
        write_err(b"drmprime: the object is not the size that was asked for\n");
        return 1;
    }

    // Write the pattern through one mapping; the import must find it in place.
    let a = match map_object(fd, handle) {
        Ok(p) => p,
        Err(e) => return fail(b"drmprime: mmap of the object failed, errno ", e),
    };
    for i in 0..SIZE {
        unsafe { core::ptr::write_volatile(a.add(i), pattern(i)) };
    }

    // Export to a descriptor.
    let prime_fd = match prime_export(fd, handle) {
        Ok(fd) if fd >= 0 => fd,
        Ok(_) => {
            write_err(b"drmprime: export returned no descriptor\n");
            return 1;
        }
        Err(e) => return fail(b"drmprime: PRIME_HANDLE_TO_FD failed, errno ", e),
    };

    // The descriptor is a descriptor: it has a kind and a device of its own, which the
    // node's name for the object does not.
    let (kind, major) = match minix_std::fs::fstat(prime_fd) {
        Ok(st) => (st.st_mode & minix_std::fs::S_IFMT, st.st_dev >> 16),
        Err(_) => {
            write_err(b"drmprime: fstat of the exported descriptor failed\n");
            return 1;
        }
    };
    if kind != minix_std::fs::S_IFREG || major != DMABUF_MAJOR {
        write_err(b"drmprime: the exported descriptor is not a dma-buf\n");
        return 1;
    }

    // Import it back and prove the bytes survived.
    let handle2 = match prime_import(fd, prime_fd) {
        Ok(h) => h,
        Err(e) => return fail(b"drmprime: PRIME_FD_TO_HANDLE failed, errno ", e),
    };
    let b = match map_object(fd, handle2) {
        Ok(p) => p,
        Err(e) => return fail(b"drmprime: mmap of the imported handle failed, errno ", e),
    };
    let mut alias = 0usize;
    for i in 0..SIZE {
        if unsafe { core::ptr::read_volatile(b.add(i)) } == pattern(i) {
            alias += 1;
        }
    }
    let same = (handle2 == handle) as u32;

    // Two refusals: a handle the node does not hold, and a descriptor that is not a dma-buf
    // (the render node's own fd).
    let export_errno = match prime_export(fd, 0x99) {
        Ok(_) => 0,
        Err(e) => e,
    };
    let import_errno = match prime_import(fd, fd) {
        Ok(_) => 0,
        Err(e) => e,
    };

    // Closing the object makes its exported descriptor stale: the import names a slot that
    // no longer holds an object, which the node refuses rather than re-aiming.
    if let Err(e) = close_object(fd, handle) {
        return fail(b"drmprime: GEM_CLOSE failed, errno ", e);
    }
    let stale_errno = match prime_import(fd, prime_fd) {
        Ok(_) => 0,
        Err(e) => e,
    };
    if minix_std::fs::close(prime_fd).is_err() {
        write_err(b"drmprime: close of the exported descriptor failed\n");
        return 1;
    }

    let mut line = [0u8; 128];
    let mut at = 0usize;
    append(&mut line, &mut at, b"drmprime: cap=");
    append(&mut line, &mut at, Decimal::of(cap as u32).bytes());
    field(&mut line, &mut at, b" h=", handle);
    field(&mut line, &mut at, b" size=", size);
    field(&mut line, &mut at, b" alias=", alias as u32);
    field(&mut line, &mut at, b" same=", same);
    field(&mut line, &mut at, b" kind=", kind);
    field(&mut line, &mut at, b" major=", major as u32);
    field(&mut line, &mut at, b" export_errno=", export_errno as u32);
    field(&mut line, &mut at, b" import_errno=", import_errno as u32);
    field(&mut line, &mut at, b" stale_errno=", stale_errno as u32);
    append(&mut line, &mut at, b"\n");
    write_out(&line[..at]);
    0
}

/// Report a failed call, with the errno, and fail.
fn fail(what: &[u8], errno: i32) -> i32 {
    write_err(what);
    write_err(Decimal::of(errno as u32).bytes());
    write_err(b"\n");
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    /// The size field of a request number, from the client's side.
    const fn arg_size(request: u32) -> u32 {
        (request >> 16) & 0x3fff
    }

    /// The argument structs against the sizes their request numbers carry — the same check
    /// `drm.rs` makes on its own side and `/bin/drmmap` makes for its three. A struct that
    /// grew or lost a field would make every request a number the node does not serve.
    #[test]
    fn the_argument_structs_are_the_sizes_their_numbers_carry() {
        assert_eq!(arg_size(DRM_IOCTL_GET_CAP), size_of::<GetCap>() as u32);
        assert_eq!(
            arg_size(DRM_IOCTL_PRIME_HANDLE_TO_FD),
            size_of::<PrimeHandle>() as u32
        );
        assert_eq!(
            arg_size(DRM_IOCTL_PRIME_FD_TO_HANDLE),
            size_of::<PrimeHandle>() as u32
        );
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE),
            size_of::<ResourceCreate>() as u32
        );
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_RESOURCE_INFO),
            size_of::<ResourceInfo>() as u32
        );
        assert_eq!(arg_size(DRM_IOCTL_VIRTGPU_MAP), size_of::<Map>() as u32);
        assert_eq!(arg_size(DRM_IOCTL_GEM_CLOSE), size_of::<GemClose>() as u32);
    }

    /// The capability the node must report: both halves of PRIME. A node that reported only
    /// one could export and never import, or the reverse, and the round trip this client
    /// makes would not be defined.
    #[test]
    fn prime_is_both_halves() {
        assert_eq!(DRM_PRIME_CAP_IMPORT | DRM_PRIME_CAP_EXPORT, 0x3);
    }

    /// The pattern has no zero byte, so "every byte survived" is a count a fresh slot's
    /// zeroed memory cannot reach.
    #[test]
    fn the_pattern_never_writes_a_zero() {
        for i in 0..SIZE {
            assert_ne!(pattern(i), 0, "index {i}");
        }
    }
}
