//! The port's dma-buf, as the render node gives it out (`.notes/wayland.md` section 5, row 25):
//! a `virtgpu` resource in the node's memory, mapped through the node and shared with another
//! process as a descriptor. The client makes and fills one, the compositor takes the descriptor
//! apart again, and both halves live here so the ABI is declared once.
//!
//! The object is one linear plane in the node's `B8G8R8X8`, whose byte order is
//! `DRM_FORMAT_XRGB8888`, the format the `zwp_linux_dmabuf_v1` buffer is created with. The
//! modifier is `DRM_FORMAT_MOD_LINEAR`: rows sit back to back, as `gbm_bo_map` gives them, and
//! there is no tiling to describe.

use std::os::fd::BorrowedFd;

/// The render node, the only DRM device the port gives userspace.
pub const NODE: &[u8] = b"/dev/dri/renderD128";

/// `VIRTIO_GPU_FORMAT_B8G8R8X8`: the node's name for `DRM_FORMAT_XRGB8888`.
const VIRTGPU_FORMAT_B8G8R8X8: u32 = 2;
const TARGET_TEXTURE_2D: u32 = 2;
/// `VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW`, what the node's objects are made with.
const BIND_TEXTURE: u32 = (1 << 1) | (1 << 3);

/// `_IOWR` from `asm-generic/ioctl.h`.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}

const DRM_BASE: u32 = b'd' as u32;
const PRIME_HANDLE_TO_FD: u32 = iowr(DRM_BASE, 0x2d, 12);
const PRIME_FD_TO_HANDLE: u32 = iowr(DRM_BASE, 0x2e, 12);
const VIRTGPU_MAP: u32 = iowr(DRM_BASE, 0x40 + 0x01, 16);
const VIRTGPU_RESOURCE_CREATE: u32 = iowr(DRM_BASE, 0x40 + 0x04, 56);
const VIRTGPU_TRANSFER_TO_HOST: u32 = iowr(DRM_BASE, 0x40 + 0x07, 44);

const RDWR: u32 = 0o02;
const CLOEXEC: u32 = 0o200000;

/// `struct drm_prime_handle`: `u32 handle`, `u32 flags`, `s32 fd`.
#[repr(C)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

/// `struct drm_virtgpu_map`: `u64 offset`, `u32 handle`, `u32 pad`.
#[repr(C)]
struct Map {
    offset: u64,
    handle: u32,
    pad: u32,
}

/// `struct drm_virtgpu_3d_transfer_to_host`: the box `x, y, z, w, h, d`, the object, and the
/// strides the host reads its backing with.
#[repr(C)]
struct TransferToHost {
    resource_id: u32,
    x: u32,
    y: u32,
    z: u32,
    w: u32,
    h: u32,
    d: u32,
    level: u32,
    offset: u32,
    stride: u32,
    layer_stride: u32,
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

/// An object the client made, with the descriptor it hands over. The descriptor stays open until
/// the request carrying it has been flushed (`wayland-client` sends the fd with the message, not
/// with the call), so it is closed when this value goes out of scope.
pub struct Owned {
    pub fd: i32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

/// The render node's descriptor, closed when it goes out of scope. Closing cannot fail in a way
/// the caller can act on, so `Drop` is where it happens.
struct Node(i32);

impl Node {
    fn fd(&self) -> i32 {
        self.0
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from `open_node` and is closed once.
        let _ = unsafe { minix_std::fs::close(self.0) };
    }
}

fn open_node() -> Result<Node, String> {
    // SAFETY: a fixed path opened for the node's ioctls.
    unsafe { minix_std::fs::open(NODE, minix_std::fs::O_RDWR, 0) }
        .map(Node)
        .map_err(|e| format!("open /dev/dri/renderD128 failed ({})", e.0))
}

fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<(), String> {
    // SAFETY: every caller passes a struct whose size is part of `request`.
    match unsafe { minix_std::fs::ioctl(fd, request, arg) } {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("ioctl 0x{request:08x} failed ({})", e.0)),
    }
}

/// Maps one of the node's handles into this process. `writable` is what the client's fill needs;
/// the compositor takes it read-only.
fn map_handle(fd: i32, handle: u32, len: usize, writable: bool) -> Result<Mapping, String> {
    let mut map = Map {
        offset: 0,
        handle,
        pad: 0,
    };
    ioctl(fd, VIRTGPU_MAP, core::ptr::addr_of_mut!(map).cast::<u8>())?;
    let prot = if writable {
        minix_rt::vmem::PROT_READ | minix_rt::vmem::PROT_WRITE
    } else {
        minix_rt::vmem::PROT_READ
    };
    // SAFETY: the node named the mapping with that offset, and `len` is the object's own size.
    let ptr = unsafe {
        minix_rt::vmem::mmap(
            core::ptr::null_mut(),
            len,
            prot,
            minix_rt::vmem::MAP_SHARED,
            fd,
            map.offset as i64,
        )
    };
    if ptr == minix_rt::vmem::MAP_FAILED {
        return Err("mmap of the node's object failed".to_string());
    }
    Ok(Mapping {
        ptr: ptr as *mut u8,
        len,
    })
}

/// Makes a `width`x`height` object in the node's memory, fills it through `fill` and exports it
/// to a descriptor.
pub fn create(width: u32, height: u32, fill: impl FnOnce(&mut [u8])) -> Result<Owned, String> {
    let stride = width * 4;
    let len = stride as usize * height as usize;
    let node = open_node()?;
    let mut resource = ResourceCreate {
        target: TARGET_TEXTURE_2D,
        format: VIRTGPU_FORMAT_B8G8R8X8,
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
        size: len as u32,
        stride,
    };
    ioctl(
        node.fd(),
        VIRTGPU_RESOURCE_CREATE,
        core::ptr::addr_of_mut!(resource).cast::<u8>(),
    )?;

    let mapped = map_handle(node.fd(), resource.bo_handle, len, true)?;
    fill(mapped.as_mut_slice());
    drop(mapped);

    // The fill wrote the object's *guest* pages. A node with GL also holds a host copy of every
    // resource, and that is the copy a GL renderer samples, so the pixels have to be sent up
    // before the object is handed over. Without this the guest's bytes are right, which is all a
    // 2D reader (the software renderer, the scan-out) ever sees, and the host's copy is empty,
    // which a GL compositor reads as a black frame.
    let mut transfer = TransferToHost {
        resource_id: resource.bo_handle,
        x: 0,
        y: 0,
        z: 0,
        w: width,
        h: height,
        d: 1,
        level: 0,
        offset: 0,
        stride,
        layer_stride: 0,
    };
    ioctl(
        node.fd(),
        VIRTGPU_TRANSFER_TO_HOST,
        core::ptr::addr_of_mut!(transfer).cast::<u8>(),
    )?;

    let mut export = PrimeHandle {
        handle: resource.bo_handle,
        flags: RDWR | CLOEXEC,
        fd: -1,
    };
    ioctl(
        node.fd(),
        PRIME_HANDLE_TO_FD,
        core::ptr::addr_of_mut!(export).cast::<u8>(),
    )?;
    if export.fd < 0 {
        return Err("the node exported no descriptor".to_string());
    }
    Ok(Owned {
        fd: export.fd,
        width,
        height,
        stride,
    })
}

/// Maps another process's descriptor into this one. The mapping is read-only: the compositor
/// samples the client's pixels and must not write to them.
pub fn map_descriptor(fd: i32, len: usize) -> Result<Mapping, String> {
    let node = open_node()?;
    let mut handle = PrimeHandle {
        handle: 0,
        flags: 0,
        fd,
    };
    ioctl(
        node.fd(),
        PRIME_FD_TO_HANDLE,
        core::ptr::addr_of_mut!(handle).cast::<u8>(),
    )?;
    map_handle(node.fd(), handle.handle, len, false)
}

/// A descriptor's plane, mapped into this process.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

impl Mapping {
    /// The plane's bytes, `len` of them as the caller asked for.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the mapping is `len` readable bytes and lives as long as `self`.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn as_mut_slice(&self) -> &mut [u8] {
        // SAFETY: the mapping is `len` writable bytes for the client's fill, and `&self` is the
        // only handle to it in this call.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `len` are the arguments the mapping was made with.
        let _ = unsafe { minix_rt::vmem::munmap(self.ptr.cast(), self.len) };
    }
}

impl Owned {
    /// The descriptor as the wayland API wants it.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: the descriptor stays open for as long as `self` is.
        unsafe { BorrowedFd::borrow_raw(self.fd) }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the descriptor was exported by `create` and is closed once.
        let _ = unsafe { minix_std::fs::close(self.fd) };
    }
}
