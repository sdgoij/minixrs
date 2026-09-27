//! `/bin/drmctx` — the render node's rendering context (`WAYLAND.md` §6.10, stage 3b-4).
//!
//! A context is what everything with GL in it hangs off: a command buffer names one, and so does
//! a transfer through it. It is also the first request that **reads** the caller's memory rather
//! than only writing it — the capset it renders with arrives in an array the node reads through a
//! pointer — so an answer here has exercised a direction the `VERSION`/`GETPARAM` clients did not.
//!
//! The pair of calls is the whole claim: making a context works, and making a second is `EEXIST`
//! rather than a second context. On a device the host gave no GL to, both are `EINVAL`: there is
//! no capset for a context to render with, and that is what a client is meant to be told.

use crate::{Decimal, append, write_err, write_out};

const NODE: &[u8] = b"/dev/dri/renderD128";

/// `_IOWR` from `asm-generic/ioctl.h`.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}

const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_IOCTL_VIRTGPU_CONTEXT_INIT: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x0b, 16);

const VIRTGPU_CONTEXT_PARAM_CAPSET_ID: u64 = 0x0001;
const VIRTGPU_DRM_CAPSET_VIRGL: u64 = 1;

/// `struct drm_virtgpu_context_set_param` — two `u64`s, which is the layout the array's stride
/// depends on.
#[repr(C)]
struct ContextSetParam {
    param: u64,
    value: u64,
}

/// `struct drm_virtgpu_context_init`: how many entries, and where the array is.
#[repr(C)]
struct ContextInit {
    num_params: u32,
    pad: u32,
    ctx_set_params: u64,
}

/// One `CONTEXT_INIT`, with `params` as its array. The array is this process's own memory, which
/// is the point: the node reads it through the pointer rather than being handed the entries.
fn init(fd: i32, params: &[ContextSetParam]) -> Result<(), i32> {
    let mut arg = ContextInit {
        num_params: params.len() as u32,
        pad: 0,
        ctx_set_params: params.as_ptr() as u64,
    };
    match unsafe {
        minix_std::fs::ioctl(
            fd,
            DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
            core::ptr::addr_of_mut!(arg) as *mut u8,
        )
    } {
        Ok(_) => Ok(()),
        Err(e) => Err(e.0),
    }
}

/// A field that is either `ok` or the errno — the two things this request can answer.
fn field(line: &mut [u8], at: &mut usize, label: &[u8], r: Result<(), i32>) {
    append(line, at, label);
    match r {
        Ok(()) => append(line, at, b"ok"),
        Err(e) => {
            append(line, at, b"err ");
            append(line, at, Decimal::of(e as u32).bytes());
        }
    }
}

/// Make a context, then try to make a second, and report both.
pub fn drmctx(_args: &[&str]) -> i32 {
    let fd = match unsafe { minix_std::fs::open(NODE, minix_std::fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            write_err(b"drmctx: no render node at /dev/dri/renderD128\n");
            return 1;
        }
    };

    let params = [ContextSetParam {
        param: VIRTGPU_CONTEXT_PARAM_CAPSET_ID,
        value: VIRTGPU_DRM_CAPSET_VIRGL,
    }];
    let first = init(fd, &params);
    // The same request again: on a device with GL the first one made the context, so this is the
    // `EEXIST` the ABI promises. On a device without GL both fail the same way, and the pair
    // still says which of the two answers this node gives.
    let second = init(fd, &params);

    let mut line = [0u8; 96];
    let mut at = 0usize;
    field(&mut line, &mut at, b"drmctx: init=", first);
    field(&mut line, &mut at, b" again=", second);
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

    #[test]
    fn the_argument_structs_are_the_sizes_their_numbers_carry() {
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_CONTEXT_INIT),
            size_of::<ContextInit>() as u32
        );
        // The array's stride is not in the request number — the request points at it — so it is
        // pinned here, against the sixteen bytes the node reads each entry from.
        assert_eq!(size_of::<ContextSetParam>(), 16);
    }

    /// `drm_virtgpu_context_init`'s two fields are where the node reads them: the count at 0 and
    /// the array's address at 8, with the padding word between them not part of either.
    #[test]
    fn the_context_init_struct_is_the_layout_the_request_reads() {
        let arg = ContextInit {
            num_params: 1,
            pad: 0,
            ctx_set_params: 0x1234_5678,
        };
        let base = &arg as *const ContextInit as usize;
        assert_eq!(&arg.num_params as *const u32 as usize - base, 0);
        assert_eq!(&arg.ctx_set_params as *const u64 as usize - base, 8);
    }
}
