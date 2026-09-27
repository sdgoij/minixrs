//! `/bin/drmmap` — a render node's memory object, mapped into a client (`WAYLAND.md` §6.10,
//! stage 3b-3).
//!
//! What this is here to prove is *aliasing*, and that is not something one mapping can say:
//! a mapping of private anonymous memory would look exactly like a mapping of the object.
//! Two mappings of one object's offset are the same frames, and a second object's mapping is
//! not — those two claims together are the evidence, and the object's length, the offset that
//! names it, and a `GEM_CLOSE` that takes the name away are what surround them.
//!
//! Like `/bin/drminfo` it declares the ABI itself — request numbers and argument structs
//! written out the way a C client writes them — so that its agreement with the server is
//! evidence rather than a tautology.

use crate::{Decimal, append, write_err, write_out};

/// The render node. A render node is what a client that draws without a display uses, and
/// DRM numbers them from 128 so a `cardN` and a `renderDN` never collide.
const NODE: &[u8] = b"/dev/dri/renderD128";

/// `_IOWR` / `_IOW` from `asm-generic/ioctl.h`.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}
const fn iow(type_: u32, nr: u32, size: u32) -> u32 {
    (1u32 << 30) | (type_ << 8) | (size << 16) | nr
}

const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_IOCTL_GEM_CLOSE: u32 = iow(DRM_IOCTL_BASE, 0x09, 8);
const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x01, 16);
const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x04, 56);
const DRM_IOCTL_VIRTGPU_RESOURCE_INFO: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x05, 16);

/// The object this asks for: 64×64 B8G8R8X8, four bytes a pixel.
const SIZE: usize = 64 * 64 * 4;
const FORMAT_B8G8R8X8: u32 = 2;
/// `PIPE_TEXTURE_2D`.
const TARGET_TEXTURE_2D: u32 = 2;
/// `VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW` — a texture bind of zero, or one of
/// the buffer binds, is rejected for a texture target, and a 3D resource carries the bind.
const BIND_TEXTURE: u32 = (1 << 1) | (1 << 3);

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
/// code positive, so `errno` here reads as the number a C client would see.
fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<i32, i32> {
    match unsafe { minix_std::fs::ioctl(fd, request, arg) } {
        Ok(code) => Ok(code),
        Err(e) => Err(e.0),
    }
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

/// `DRM_IOCTL_GEM_CLOSE`.
fn close(fd: i32, handle: u32) -> Result<(), i32> {
    let mut arg = GemClose { handle, pad: 0 };
    ioctl(
        fd,
        DRM_IOCTL_GEM_CLOSE,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )
    .map(|_| ())
}

/// The byte written to, and expected at, index `i`.
///
/// Never zero, so that "every byte of a fresh object differs" is a count the gate can read off
/// a slot the server zeroed: a zero byte in the pattern would match the fresh slot's own byte
/// and make the count one short for a reason that has nothing to do with aliasing.
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

/// Create two objects, map one of them twice and the other once, and report what the three
/// mappings and the handles say.
pub fn drmmap(_args: &[&str]) -> i32 {
    let fd = match unsafe { minix_std::fs::open(NODE, minix_std::fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            write_err(b"drmmap: no render node at /dev/dri/renderD128\n");
            return 1;
        }
    };

    let handle = match create(fd, 64, 64) {
        Ok(h) => h,
        Err(e) => return fail(b"drmmap: RESOURCE_CREATE failed, errno ", e),
    };
    // The length the object has, which is not the length that was asked for unless the
    // request was already a whole number of pages.
    let (res_handle, size) = match info(fd, handle) {
        Ok(v) => v,
        Err(e) => return fail(b"drmmap: RESOURCE_INFO failed, errno ", e),
    };
    if size as usize != SIZE {
        write_err(b"drmmap: RESOURCE_INFO says the object is not the size that was asked for\n");
        return 1;
    }

    let a = match map_object(fd, handle) {
        Ok(p) => p,
        Err(e) => return fail(b"drmmap: mmap of the first object failed, errno ", e),
    };
    let b = match map_object(fd, handle) {
        Ok(p) => p,
        Err(e) => return fail(b"drmmap: mmap of the second mapping failed, errno ", e),
    };
    if a == b {
        // Two mappings at one address is one mapping, and would prove nothing.
        write_err(b"drmmap: two mappings of one object came back at one address\n");
        return 1;
    }

    // Write through `a`, read through `b`: only one set of frames can do this.
    for i in 0..SIZE {
        unsafe { core::ptr::write_volatile(a.add(i), pattern(i)) };
    }
    let mut alias = 0usize;
    for i in 0..SIZE {
        if unsafe { core::ptr::read_volatile(b.add(i)) } == pattern(i) {
            alias += 1;
        }
    }

    // A second object is other memory: its mapping must not show the first one's bytes.
    let other = match create(fd, 64, 64) {
        Ok(h) => h,
        Err(e) => return fail(b"drmmap: RESOURCE_CREATE of the second object failed, errno ", e),
    };
    let c = match map_object(fd, other) {
        Ok(p) => p,
        Err(e) => return fail(b"drmmap: mmap of the second object failed, errno ", e),
    };
    let mut apart = 0usize;
    for i in 0..SIZE {
        if unsafe { core::ptr::read_volatile(c.add(i)) } != pattern(i) {
            apart += 1;
        }
    }

    // Closing the first object takes its name away: the offset it was mapped at is not a way
    // back in, and the node says so rather than handing out the memory again.
    if let Err(e) = close(fd, handle) {
        return fail(b"drmmap: GEM_CLOSE failed, errno ", e);
    }
    let close_errno = match map_offset(fd, handle) {
        Ok(_) => 0,
        Err(e) => e,
    };
    if let Err(e) = close(fd, other) {
        return fail(b"drmmap: GEM_CLOSE of the second object failed, errno ", e);
    }

    let mut line = [0u8; 128];
    let mut at = 0usize;
    append(&mut line, &mut at, b"drmmap: h=");
    append(&mut line, &mut at, Decimal::of(handle).bytes());
    append(&mut line, &mut at, b" res=");
    append(&mut line, &mut at, Decimal::of(res_handle).bytes());
    field(&mut line, &mut at, b" size=", size);
    field(&mut line, &mut at, b" alias=", alias as u32);
    field(&mut line, &mut at, b" apart=", apart as u32);
    append(&mut line, &mut at, b" close=err ");
    append(&mut line, &mut at, Decimal::of(close_errno as u32).bytes());
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

    /// The argument structs against the sizes their request numbers carry, as `drm.rs` checks
    /// on its own side and `drminfo` does for its three. A struct that grew or lost a field
    /// would make every request a number the node does not serve.
    #[test]
    fn the_argument_structs_are_the_sizes_their_numbers_carry() {
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

    /// `GEM_CLOSE` is the one request here that is `_IOW` and not `_IOWR`, which is what makes
    /// it the request that tells the two ioctl encodings apart (see `net::ioc_linux_copies_in`).
    #[test]
    fn gem_close_is_the_unidirectional_one() {
        assert_eq!(DRM_IOCTL_GEM_CLOSE >> 30, 1, "_IOW");
        assert_eq!(DRM_IOCTL_VIRTGPU_MAP >> 30, 3, "_IOWR");
        assert_eq!(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE >> 30, 3, "_IOWR");
        assert_eq!(DRM_IOCTL_VIRTGPU_RESOURCE_INFO >> 30, 3, "_IOWR");
    }

    /// The pattern has no zero byte, which is what lets "every byte differs" be read off a
    /// fresh object's zeroed memory, and is not constant, so a mapping that aliases in the
    /// wrong place still shows up.
    #[test]
    fn the_pattern_never_writes_a_zero() {
        for i in 0..SIZE {
            assert_ne!(pattern(i), 0, "index {i}");
        }
        assert_ne!(pattern(2), pattern(1));
        assert_ne!(pattern(4), pattern(3));
    }
}
