//! `/bin/drmexec` — an object's contents through the transfer and submit ABI (`WAYLAND.md` §6.10,
//! stage 3b-4).
//!
//! This is the boot probe's round trip, done through the DRM ABI instead of the driver's own
//! calls: create an object, put a known pattern in the guest's pages, transfer those pages *to*
//! the host's copy of the resource, hand the renderer a command that overwrites one row of it,
//! transfer the result *back*, and read the two regions off. A transfer round trip alone would
//! prove nothing — it only ever moves bytes the guest already had, so it would agree with itself
//! whether or not the renderer ran anything — which is why the command between the two transfers
//! is the point, and why the answer is two counts rather than one.
//!
//! It also exercises the object path, the context path and the submit path together for the first
//! time: `EXECBUFFER` names the object, and the node has to have put it in the context's table
//! before the command runs, or the host refuses the command *without telling anyone*.

use crate::{Decimal, append, write_err, write_out};

const NODE: &[u8] = b"/dev/dri/renderD128";

/// `_IOWR` / `_IOW` from `asm-generic/ioctl.h`.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}

const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x01, 16);
const DRM_IOCTL_VIRTGPU_EXECBUFFER: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x02, 64);
const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x04, 56);
const DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x06, 44);
const DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x07, 44);
const DRM_IOCTL_VIRTGPU_WAIT: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x08, 8);

/// The object: 64×64 B8G8R8X8, four bytes a pixel, and a whole number of pages.
const W: u32 = 64;
const H: u32 = 64;
const SIZE: usize = (W * H * 4) as usize;
/// `PIPE_TEXTURE_2D`, `VIRTIO_GPU_FORMAT_B8G8R8X8`, and a texture bind — a bind of zero is
/// rejected for a texture target.
const TARGET_TEXTURE_2D: u32 = 2;
const FORMAT_B8G8R8X8: u32 = 2;
const BIND_TEXTURE: u32 = (1 << 1) | (1 << 3);

/// `VIRGL_CCMD_RESOURCE_INLINE_WRITE`: the simplest command that makes the *host* write known
/// bytes into a resource — no framebuffer, shader or bound state, just the resource, a box and the
/// data.
const VIRGL_CCMD_RESOURCE_INLINE_WRITE: u32 = 9;
/// `VIRGL_OBJECT_NULL`: a resource-level command names no object.
const VIRGL_OBJECT_NULL: u32 = 0;
/// Pixels the command writes: one row of the object.
const IW_PIXELS: u32 = 8;
const IW_BYTES: usize = (IW_PIXELS * 4) as usize;
/// The command is a header dword, the eleven field dwords it shares with a transfer, then the
/// data — all in one buffer, which is why it is small.
const IW_CMD_LEN: usize = 4 * (12 + IW_PIXELS as usize);

/// The pattern the *guest* puts in the object's pages.
fn pattern(i: usize) -> u8 {
    (i.wrapping_mul(31).wrapping_add(7) & 0xff) as u8
}

/// The pattern the command makes the *host* write. The complement of the guest's, so a row that
/// was never written cannot be mistaken for one that was.
fn host_pattern(i: usize) -> u8 {
    !pattern(i)
}

/// What the object should hold when everything worked: the host's row first, the guest's pattern
/// after it.
fn expected(i: usize) -> u8 {
    if i < IW_BYTES {
        host_pattern(i)
    } else {
        pattern(i)
    }
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

/// `struct drm_virtgpu_3d_transfer_to_host` — the box and strides are all this client uses, and
/// zero strides mean "derive them from the format and the geometry".
#[repr(C)]
struct Transfer3d {
    bo_handle: u32,
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

/// `struct drm_virtgpu_execbuffer`.
#[repr(C)]
struct ExecBuffer {
    flags: u32,
    size: u32,
    command: u64,
    bo_handles: u64,
    num_bo_handles: u32,
    fence_fd: i32,
    ring_idx: u32,
    syncobj_stride: u32,
    num_in_syncobjs: u32,
    num_out_syncobjs: u32,
    in_syncobjs: u64,
    out_syncobjs: u64,
}

/// `struct drm_virtgpu_map`.
#[repr(C)]
struct Map {
    offset: u64,
    handle: u32,
    pad: u32,
}

/// `struct drm_virtgpu_3d_wait`.
#[repr(C)]
struct Wait {
    handle: u32,
    flags: u32,
}

fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<i32, i32> {
    match unsafe { minix_std::fs::ioctl(fd, request, arg) } {
        Ok(code) => Ok(code),
        Err(e) => Err(e.0),
    }
}

/// The one command this submits: an inline write of [`IW_BYTES`] bytes over the object's first
/// row. Written out here the way the boot probe writes it, so a disagreement with the renderer is
/// a disagreement this client can see for itself.
fn inline_write_command(resource_id: u32) -> [u8; IW_CMD_LEN] {
    let mut words = [0u32; 12 + IW_PIXELS as usize];
    words[0] = VIRGL_CCMD_RESOURCE_INLINE_WRITE | (VIRGL_OBJECT_NULL << 8) | ((11 + IW_PIXELS) << 16);
    words[1] = resource_id;
    // words[2] is the level and words[3] a usage word no decoder reads.
    words[4] = IW_BYTES as u32;
    words[5] = IW_BYTES as u32;
    // words[6..=8] are the box origin, which is zero.
    words[9] = IW_PIXELS;
    words[10] = 1;
    words[11] = 1;
    for (dword, w) in words[12..].iter_mut().enumerate() {
        let at = dword * 4;
        *w = u32::from_le_bytes([
            host_pattern(at),
            host_pattern(at + 1),
            host_pattern(at + 2),
            host_pattern(at + 3),
        ]);
    }
    let mut out = [0u8; IW_CMD_LEN];
    for (i, w) in words.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// Report `what` and the errno, and fail — the clients' shared shape.
fn fail(what: &[u8], errno: i32) -> i32 {
    write_err(what);
    write_err(Decimal::of(errno as u32).bytes());
    write_err(b"\n");
    1
}

/// A `name=value` field.
fn field(line: &mut [u8], at: &mut usize, name: &[u8], value: u32) {
    append(line, at, name);
    append(line, at, Decimal::of(value).bytes());
}

/// `ok`, or the errno, as a field.
fn outcome(line: &mut [u8], at: &mut usize, name: &[u8], r: Result<i32, i32>) {
    append(line, at, name);
    match r {
        Ok(_) => append(line, at, b"ok"),
        Err(e) => {
            append(line, at, b"err ");
            append(line, at, Decimal::of(e as u32).bytes());
        }
    }
}

/// Put the pattern in, transfer it to the host, run a command that rewrites one row, bring it
/// back, and report how much of the object came back as it should have.
pub fn drmexec(_args: &[&str]) -> i32 {
    let fd = match unsafe { minix_std::fs::open(NODE, minix_std::fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            write_err(b"drmexec: no render node at /dev/dri/renderD128\n");
            return 1;
        }
    };

    let mut create = ResourceCreate {
        target: TARGET_TEXTURE_2D,
        format: FORMAT_B8G8R8X8,
        bind: BIND_TEXTURE,
        width: W,
        height: H,
        depth: 1,
        array_size: 1,
        last_level: 0,
        nr_samples: 0,
        flags: 0,
        bo_handle: 0,
        res_handle: 0,
        size: SIZE as u32,
        stride: W * 4,
    };
    if let Err(e) = ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_RESOURCE_CREATE,
        core::ptr::addr_of_mut!(create) as *mut u8,
    ) {
        return fail(b"drmexec: RESOURCE_CREATE failed, errno ", e);
    }
    let handle = create.bo_handle;

    // The guest's pages, which are also the resource's backing: the pattern goes in through the
    // mapping, and a transfer is what makes the host's copy of the resource match it.
    let mut map = Map {
        offset: 0,
        handle,
        pad: 0,
    };
    if let Err(e) = ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_MAP,
        core::ptr::addr_of_mut!(map) as *mut u8,
    ) {
        return fail(b"drmexec: MAP failed, errno ", e);
    }
    let pages = unsafe {
        minix_std::vmem::mmap_status(
            core::ptr::null_mut(),
            SIZE,
            minix_std::vmem::PROT_READ | minix_std::vmem::PROT_WRITE,
            minix_std::vmem::MAP_SHARED,
            fd,
            map.offset as i64,
        )
    };
    let pages = match pages {
        Ok(p) => p,
        Err(e) => return fail(b"drmexec: mmap failed, errno ", e),
    };
    for i in 0..SIZE {
        unsafe { core::ptr::write_volatile(pages.add(i), pattern(i)) };
    }

    let to_host = Transfer3d {
        bo_handle: handle,
        x: 0,
        y: 0,
        z: 0,
        w: W,
        h: H,
        d: 1,
        level: 0,
        offset: 0,
        stride: 0,
        layer_stride: 0,
    };
    let upload = ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST,
        core::ptr::addr_of!(to_host) as *mut u8,
    );

    // The command, and the one object it names. The array is this process's own memory, which is
    // the point of the two pointers: the node reads the command buffer and the handles out of it.
    let command = inline_write_command(handle);
    let handles = [handle];
    let mut exec = ExecBuffer {
        flags: 0,
        size: command.len() as u32,
        command: command.as_ptr() as u64,
        bo_handles: handles.as_ptr() as u64,
        num_bo_handles: handles.len() as u32,
        fence_fd: -1,
        ring_idx: 0,
        syncobj_stride: 0,
        num_in_syncobjs: 0,
        num_out_syncobjs: 0,
        in_syncobjs: 0,
        out_syncobjs: 0,
    };
    let submit = ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_EXECBUFFER,
        core::ptr::addr_of_mut!(exec) as *mut u8,
    );

    let mut wait = Wait { handle, flags: 0 };
    let waited = ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_WAIT,
        core::ptr::addr_of_mut!(wait) as *mut u8,
    );

    let download = ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST,
        core::ptr::addr_of!(to_host) as *mut u8,
    );

    // Read the two regions off, and keep them apart: the row the command wrote is the evidence
    // that the renderer ran, and the rest is the evidence that it touched nothing else.
    let mut row = 0usize;
    let mut rest = 0usize;
    for i in 0..SIZE {
        let got = unsafe { core::ptr::read_volatile(pages.add(i)) };
        if got == expected(i) {
            if i < IW_BYTES {
                row += 1;
            } else {
                rest += 1;
            }
        }
    }

    let mut line = [0u8; 128];
    let mut at = 0usize;
    append(&mut line, &mut at, b"drmexec:");
    field(&mut line, &mut at, b" h=", handle);
    outcome(&mut line, &mut at, b" up=", upload);
    outcome(&mut line, &mut at, b" exec=", submit);
    outcome(&mut line, &mut at, b" wait=", waited);
    outcome(&mut line, &mut at, b" down=", download);
    field(&mut line, &mut at, b" row=", row as u32);
    field(&mut line, &mut at, b" rest=", rest as u32);
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

    /// Every argument struct against the size its request number carries — the same check the
    /// node makes on its own side, from the other end of the wire.
    #[test]
    fn the_argument_structs_are_the_sizes_their_numbers_carry() {
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE),
            size_of::<ResourceCreate>() as u32
        );
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST),
            size_of::<Transfer3d>() as u32
        );
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST),
            size_of::<Transfer3d>() as u32
        );
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_EXECBUFFER),
            size_of::<ExecBuffer>() as u32
        );
        assert_eq!(arg_size(DRM_IOCTL_VIRTGPU_MAP), size_of::<Map>() as u32);
        assert_eq!(arg_size(DRM_IOCTL_VIRTGPU_WAIT), size_of::<Wait>() as u32);
    }

    /// The command is one fixed shape: twelve dwords of header and fields, then one row of data.
    /// Spelled out rather than derived, so that a client and a renderer that disagree about it
    /// are two statements that differ.
    #[test]
    fn the_command_buffer_is_whole_dwords_and_the_length_the_protocol_fixes() {
        let cmd = inline_write_command(1);
        assert_eq!(cmd.len(), 80);
        assert!(cmd.len().is_multiple_of(4));
    }

    /// The two patterns differ everywhere, which is what makes the first row a discriminator: a
    /// row the command never wrote cannot read as one it did.
    #[test]
    fn the_host_pattern_is_the_guest_patterns_complement() {
        for i in 0..SIZE {
            assert_ne!(pattern(i), host_pattern(i), "index {i}");
        }
    }

    /// The layout of the command's header and fields, which is a third statement of the protocol
    /// this client shares with the renderer.
    #[test]
    fn the_inline_write_command_is_the_layout_the_renderer_reads() {
        let cmd = inline_write_command(7);
        let w = |at: usize| u32::from_le_bytes(cmd[at..at + 4].try_into().unwrap());
        assert_eq!(w(0) & 0xff, VIRGL_CCMD_RESOURCE_INLINE_WRITE);
        assert_eq!((w(0) >> 8) & 0xff, VIRGL_OBJECT_NULL);
        // The length is what the host recomputes the data size from: (len - 11) * 4.
        assert_eq!(w(0) >> 16, 11 + IW_PIXELS);
        assert_eq!(w(4), 7);
        assert_eq!(w(16), IW_BYTES as u32);
        assert_eq!(w(20), IW_BYTES as u32);
        assert_eq!(w(36), IW_PIXELS);
        assert_eq!(w(40), 1);
        assert_eq!(w(44), 1);
        for i in 0..IW_BYTES {
            assert_eq!(cmd[48 + i], host_pattern(i));
        }
    }
}
