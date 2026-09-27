//! `/bin/drmblob` — the render node's blob objects (`WAYLAND.md` §6.10, stage 3b-4).
//!
//! A blob is a create whose request names the *kind* of memory it wants rather than a 2D shape,
//! and it is the one create that carries its own backing: the pages travel inside the command, so
//! no `ATTACH_BACKING` follows it. That is what gives this client two things no other step has.
//!
//! The first is a field: `RESOURCE_INFO` reports the kind an object was made with, and that field
//! read 0 for every object until this stage. The second is a *reason*: the kinds this node has no
//! memory for are refused, rather than made into objects no client could reach. The kind served
//! here is the guest's, because this node's memory *is* guest memory; a host blob needs a
//! shared-memory window to reach the host's, and this device has none — which is what its 0 for
//! `VIRTGPU_PARAM_HOST_VISIBLE` already said. So the host kinds are `ENOSYS`, a different statement
//! from the `EINVAL` a request this ABI cannot read gets, and this client prints both so the two
//! are told apart.
//!
//! That the blob's memory really is the object's is `/bin/drmmap`'s claim about a mapping and
//! `/bin/drmexec`'s about a transfer, not this one's: what is here is the request surface — the
//! kind that comes back, the mapping a guest blob has and a host one could not, and each rule the
//! node enforces before it would have to reach the host.

use crate::{Decimal, append, write_err, write_out};

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
const DRM_IOCTL_VIRTGPU_RESOURCE_INFO: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x05, 16);
const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x0a, 56);

/// The memory kinds and flags of `drm/virtgpu_drm.h`, spelled out here so that this client's
/// agreement with the node is evidence rather than a shared include.
const BLOB_MEM_GUEST: u32 = 0x0001;
const BLOB_MEM_HOST3D: u32 = 0x0002;
const BLOB_MEM_HOST3D_GUEST: u32 = 0x0003;
const BLOB_FLAG_USE_MAPPABLE: u32 = 0x0001;
const BLOB_FLAG_USE_CROSS_DEVICE: u32 = 0x0004;

/// The blob this asks for: four pages, so the length is a whole number of them and more than one —
/// a mapping of four pages cannot be a single page's worth of the wrong thing.
const SIZE: u64 = 16384;

/// `struct drm_virtgpu_resource_create_blob` — 56 bytes. `size` is the length the ABI carries as a
/// `u64` (at 16), `cmd_size`/`cmd` are the pair a guest blob must leave at zero, and `blob_hints`
/// is the hint word a request like this one has no use for.
#[repr(C)]
struct BlobCreate {
    blob_mem: u32,
    blob_flags: u32,
    bo_handle: u32,
    res_handle: u32,
    size: u64,
    pad: u32,
    cmd_size: u32,
    cmd: u64,
    blob_id: u64,
    blob_hints: u32,
    pad2: u32,
}

impl BlobCreate {
    /// A guest blob of `size` bytes. Every request this client makes starts from one and changes
    /// the single field it is asking about, so a refusal cannot be an accident of another field.
    fn guest(size: u64) -> Self {
        Self {
            blob_mem: BLOB_MEM_GUEST,
            blob_flags: BLOB_FLAG_USE_MAPPABLE,
            bo_handle: 0,
            res_handle: 0,
            size,
            pad: 0,
            cmd_size: 0,
            cmd: 0,
            blob_id: 0,
            blob_hints: 0,
            pad2: 0,
        }
    }
}

/// `struct drm_virtgpu_resource_info`: the handle in, the object's numbers out.
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

fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<i32, i32> {
    match unsafe { minix_std::fs::ioctl(fd, request, arg) } {
        Ok(code) => Ok(code),
        Err(e) => Err(e.0),
    }
}

/// One `RESOURCE_CREATE_BLOB`. The handle comes back in the word the request already carries, which
/// is why this returns it rather than taking one.
fn create(fd: i32, arg: &mut BlobCreate) -> Result<u32, i32> {
    ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB,
        core::ptr::addr_of_mut!(*arg) as *mut u8,
    )?;
    Ok(arg.bo_handle)
}

/// `RESOURCE_INFO`: the object's resource id, length and blob kind.
fn info(fd: i32, handle: u32) -> Result<ResourceInfo, i32> {
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
    Ok(arg)
}

/// Map the object `SIZE` bytes wide, and return where — or the errno `MAP` or `mmap` gave.
fn map_object(fd: i32, handle: u32) -> Result<*mut u8, i32> {
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
    unsafe {
        minix_std::vmem::mmap_status(
            core::ptr::null_mut(),
            SIZE as usize,
            minix_std::vmem::PROT_READ | minix_std::vmem::PROT_WRITE,
            minix_std::vmem::MAP_SHARED,
            fd,
            arg.offset as i64,
        )
    }
}

/// `GEM_CLOSE`.
fn close(fd: i32, handle: u32) -> Result<(), i32> {
    let mut arg = GemClose { handle, pad: 0 };
    ioctl(
        fd,
        DRM_IOCTL_GEM_CLOSE,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )
    .map(|_| ())
}

/// Ask for `arg` and report the answer as a *refusal*: the errno, or `err 0` for a request this node
/// should not have accepted. Zero is not an errno, so a request that unexpectedly succeeded is
/// visible in the line rather than reading as another `ok`.
fn refused(fd: i32, arg: &mut BlobCreate) -> Result<i32, i32> {
    match create(fd, arg) {
        Ok(handle) => {
            // The node made an object it should have refused, so the `err 0` below is what the gate
            // hears about. The object still goes back, so a request under test cannot also spend a
            // slot of the node's table — and a close that fails is the answer instead, because that
            // is a failure the caller can still act on.
            match close(fd, handle) {
                Ok(()) => Err(0),
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// A field that is either `ok` or the errno, which is the pair of answers every request here has.
fn field(line: &mut [u8], at: &mut usize, label: &[u8], r: Result<i32, i32>) {
    append(line, at, label);
    match r {
        Ok(_) => append(line, at, b"ok"),
        Err(e) => {
            append(line, at, b"err ");
            append(line, at, Decimal::of(e as u32).bytes());
        }
    }
}

/// A `name=value` field.
fn named(line: &mut [u8], at: &mut usize, name: &[u8], value: u32) {
    append(line, at, name);
    append(line, at, Decimal::of(value).bytes());
}

/// Report a failure that leaves the rest of the client unwritable, with the errno.
fn fail(what: &[u8], errno: i32) -> i32 {
    write_err(what);
    write_err(Decimal::of(errno as u32).bytes());
    write_err(b"\n");
    1
}

/// Create a guest blob, report the kind that comes back, and ask for everything this node cannot
/// give — one request per rule, each differing from the good one in a single field.
pub fn drmblob(_args: &[&str]) -> i32 {
    let fd = match unsafe { minix_std::fs::open(NODE, minix_std::fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            write_err(b"drmblob: no render node at /dev/dri/renderD128\n");
            return 1;
        }
    };

    // The create is the first thing this client asks, so on a device with no blob feature there is
    // nothing else to report — which is what makes this one line the no-GL gate's expectation.
    let handle = match create(fd, &mut BlobCreate::guest(SIZE)) {
        Ok(h) => h,
        Err(e) => {
            let mut line = [0u8; 64];
            let mut at = 0usize;
            append(&mut line, &mut at, b"drmblob: create=err ");
            append(&mut line, &mut at, Decimal::of(e as u32).bytes());
            append(&mut line, &mut at, b"\n");
            write_out(&line[..at]);
            return 0;
        }
    };

    // The kind, and the length: the field this stage exists to make non-zero, and the one a client
    // sizes a mapping from. Named `object` rather than `info` so it does not shadow the request it
    // came from.
    let object = match info(fd, handle) {
        Ok(i) => i,
        Err(e) => return fail(b"drmblob: RESOURCE_INFO failed, errno ", e),
    };
    let mapped = map_object(fd, handle).map(|_| 0);

    // A host blob, and the kind that is host memory with a guest shadow: this device has no window
    // to reach either, and says so with a reason rather than with an errno for a request it could
    // not read.
    let mut host = BlobCreate::guest(SIZE);
    host.blob_mem = BLOB_MEM_HOST3D;
    let host3d = refused(fd, &mut host);
    let mut host_guest = BlobCreate::guest(SIZE);
    host_guest.blob_mem = BLOB_MEM_HOST3D_GUEST;
    let hostguest = refused(fd, &mut host_guest);

    // A memory kind the ABI does not define.
    let mut mem0 = BlobCreate::guest(SIZE);
    mem0.blob_mem = 0;
    let mem0 = refused(fd, &mut mem0);

    // The two fields that name another object's memory. `cmd` is deliberately left null: a node that
    // read it before refusing would answer `EFAULT` rather than `EINVAL`, and the errno here is what
    // tells those two orders apart.
    let mut blob_id = BlobCreate::guest(SIZE);
    blob_id.blob_id = 5;
    let blob_id = refused(fd, &mut blob_id);
    let mut cmd_size = BlobCreate::guest(SIZE);
    cmd_size.cmd_size = 4;
    let cmd_size = refused(fd, &mut cmd_size);

    // A flag the ABI does not define, and the one that needs a UUID command.
    let mut undefined = BlobCreate::guest(SIZE);
    undefined.blob_flags = 0x8;
    let undefined = refused(fd, &mut undefined);
    let mut cross = BlobCreate::guest(SIZE);
    cross.blob_flags = BLOB_FLAG_USE_MAPPABLE | BLOB_FLAG_USE_CROSS_DEVICE;
    let cross = refused(fd, &mut cross);

    // Closing takes the handle away, which is the same claim `/bin/drmmap` makes about an ordinary
    // object — and it is what keeps this client from spending a slot of the node's table.
    let closed = close(fd, handle).map(|()| 0);
    let gone = match info(fd, handle) {
        Ok(_) => Ok(0),
        Err(e) => Err(e),
    };

    let mut line = [0u8; 224];
    let mut at = 0usize;
    append(&mut line, &mut at, b"drmblob: create=ok");
    named(&mut line, &mut at, b" mem=", object.blob_mem);
    named(&mut line, &mut at, b" size=", object.size);
    field(&mut line, &mut at, b" map=", mapped);
    field(&mut line, &mut at, b" host3d=", host3d);
    field(&mut line, &mut at, b" hostguest=", hostguest);
    field(&mut line, &mut at, b" mem0=", mem0);
    field(&mut line, &mut at, b" blobid=", blob_id);
    field(&mut line, &mut at, b" cmdsize=", cmd_size);
    field(&mut line, &mut at, b" flags=", undefined);
    field(&mut line, &mut at, b" cross=", cross);
    field(&mut line, &mut at, b" close=", closed);
    field(&mut line, &mut at, b" gone=", gone);
    append(&mut line, &mut at, b"\n");
    write_out(&line[..at]);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    /// The size field of a request number, from the client's side.
    const fn arg_size(request: u32) -> u32 {
        (request >> 16) & 0x3fff
    }

    /// Every argument struct against the size its request number carries. A struct that lost the
    /// ABI's trailing `blob_hints` and padding would make every blob request a number the node does
    /// not serve — which is exactly what building against a stale header does.
    #[test]
    fn the_argument_structs_are_the_sizes_their_numbers_carry() {
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB),
            size_of::<BlobCreate>() as u32
        );
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_RESOURCE_INFO),
            size_of::<ResourceInfo>() as u32
        );
        assert_eq!(arg_size(DRM_IOCTL_VIRTGPU_MAP), size_of::<Map>() as u32);
        assert_eq!(arg_size(DRM_IOCTL_GEM_CLOSE), size_of::<GemClose>() as u32);
    }

    /// The fields the node reads, at the offsets the ABI puts them in: `size` at 16, `cmd_size` at
    /// 28, `cmd` at 32, `blob_id` at 40 and `blob_hints` at 48 — with the two handles between the
    /// flags and the length, which is where they are written back.
    #[test]
    fn the_blob_struct_is_the_layout_the_request_reads() {
        let arg = BlobCreate::guest(SIZE);
        let base = &arg as *const BlobCreate as usize;
        let at = |p: *const u8| p as usize - base;
        assert_eq!(at(&arg.blob_mem as *const u32 as *const u8), 0);
        assert_eq!(at(&arg.blob_flags as *const u32 as *const u8), 4);
        assert_eq!(at(&arg.bo_handle as *const u32 as *const u8), 8);
        assert_eq!(at(&arg.res_handle as *const u32 as *const u8), 12);
        assert_eq!(at(&arg.size as *const u64 as *const u8), 16);
        assert_eq!(at(&arg.cmd_size as *const u32 as *const u8), 28);
        assert_eq!(at(&arg.cmd as *const u64 as *const u8), 32);
        assert_eq!(at(&arg.blob_id as *const u64 as *const u8), 40);
        assert_eq!(at(&arg.blob_hints as *const u32 as *const u8), 48);
        assert_eq!(size_of::<BlobCreate>(), 56);
    }

    /// The template every request here is built from is the one shape this node serves: the guest
    /// kind, the mappable flag, and the two fields that would name another object's memory at zero.
    #[test]
    fn the_template_is_a_guest_blob_with_nothing_else_set() {
        let arg = BlobCreate::guest(SIZE);
        assert_eq!(arg.blob_mem, BLOB_MEM_GUEST);
        assert_eq!(arg.blob_flags, BLOB_FLAG_USE_MAPPABLE);
        assert_eq!(arg.size, SIZE);
        assert_eq!(arg.cmd_size, 0);
        assert_eq!(arg.cmd, 0);
        assert_eq!(arg.blob_id, 0);
        assert_eq!(arg.blob_hints, 0);
        assert_eq!(arg.pad, 0);
    }
}
