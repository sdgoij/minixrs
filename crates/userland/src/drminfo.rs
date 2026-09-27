//! `/bin/drminfo` — what the DRM render node says it is (`WAYLAND.md` §6.10, stage 3b).
//!
//! This is also the second statement of the render node's ABI, and deliberately not the
//! one the server implements: the request numbers and the three argument structs below are
//! written out here the way a C client writes them. That is what makes this program and
//! `drivers::video::drm` agreeing into evidence rather than a tautology — a number either
//! of them computes wrongly is a request the node answers `ENOTTY` to, and the gate goes
//! red. (The hand-written C client of 3b-4 is the third statement.)
//!
//! It reports rather than judges: every answer it gets — including the ones that say there
//! is no 3D on this host — is printed, so a boot with no GL shows what a client *sees*
//! rather than only that it saw nothing.

use crate::{Decimal as Dec, append, write_err, write_out};

/// The render node. A render node is what a client that draws without a display uses, and
/// DRM numbers them from 128 so a `cardN` and a `renderDN` never collide.
const NODE: &[u8] = b"/dev/dri/renderD128";

/// `_IOWR` from `asm-generic/ioctl.h`.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}

const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_IOCTL_VERSION: u32 = iowr(DRM_IOCTL_BASE, 0x00, 64);
const DRM_IOCTL_VIRTGPU_GETPARAM: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x03, 16);
const DRM_IOCTL_VIRTGPU_GET_CAPS: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x09, 24);

const VIRTGPU_PARAM_3D_FEATURES: u64 = 1;
const VIRTGPU_PARAM_RESOURCE_BLOB: u64 = 3;
const VIRTGPU_PARAM_HOST_VISIBLE: u64 = 4;
const VIRTGPU_PARAM_CONTEXT_INIT: u64 = 6;
const VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS: u64 = 7;
const VIRTGPU_DRM_CAPSET_VIRGL: u32 = 1;

/// `struct drm_version`.
#[repr(C)]
struct Version {
    major: i32,
    minor: i32,
    patchlevel: i32,
    name_len: usize,
    name: *mut u8,
    date_len: usize,
    date: *mut u8,
    desc_len: usize,
    desc: *mut u8,
}

impl Version {
    const fn empty() -> Self {
        Self {
            major: 0,
            minor: 0,
            patchlevel: 0,
            name_len: 0,
            name: core::ptr::null_mut(),
            date_len: 0,
            date: core::ptr::null_mut(),
            desc_len: 0,
            desc: core::ptr::null_mut(),
        }
    }
}

/// `struct drm_virtgpu_getparam`: the parameter, then the pointer its value goes through.
#[repr(C)]
struct Getparam {
    param: u64,
    value: *mut i32,
}

/// `struct drm_virtgpu_get_caps`: which capset, and where its blob should go.
#[repr(C)]
struct GetCaps {
    cap_set_id: u32,
    cap_set_ver: u32,
    addr: *mut u8,
    size: u32,
    pad: u32,
}

/// The longest driver string this will read. The lengths come from the driver, so they
/// need a bound — and `virtio_gpu` and its description are far inside it.
const FIELD: usize = 32;

/// The buffer `GET_CAPS` is offered. The capset is about 300 bytes (virgl v1 is 308), and
/// the request reports the length it *copied*, not the blob's, so a client that offers
/// less than the blob gets less and cannot tell by how much.
const CAPS_BUF: usize = 1024;

/// An ioctl whose result is a `Result` rather than an errno in a buffer: `MinixErr` keeps
/// the code positive, so `errno` here reads as the number a C client would see.
fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<i32, i32> {
    match unsafe { minix_std::fs::ioctl(fd, request, arg) } {
        Ok(code) => Ok(code),
        Err(e) => Err(e.0),
    }
}

/// `DRM_IOCTL_VIRTGPU_GETPARAM` for one parameter: its value, or the errno. (`value` is a
/// user pointer in the ABI, and the value behind it is an `int`.)
fn getparam(fd: i32, param: u64) -> Result<i32, i32> {
    let mut value: i32 = 0;
    let mut arg = Getparam {
        param,
        value: &mut value,
    };
    ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_GETPARAM,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )?;
    Ok(value)
}

/// Append a written `Ok`/`Err` pair as two fields: the value and the errno. Both are
/// printed because a length and an errno are both small numbers, and a line that printed
/// only one could not be read.
fn append_result(line: &mut [u8], at: &mut usize, label: &[u8], r: Result<i32, i32>) {
    append(line, at, label);
    let (value, errno) = match r {
        Ok(v) => (v as u32, 0u32),
        Err(e) => (0, e as u32),
    };
    append(line, at, Dec::of(value).bytes());
    append(line, at, b" err ");
    append(line, at, Dec::of(errno).bytes());
}

/// Query the render node and report what it said, in one line.
pub fn drminfo(_args: &[&str]) -> i32 {
    let fd = match unsafe { minix_std::fs::open(NODE, minix_std::fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            write_err(b"drminfo: no render node at /dev/dri/renderD128\n");
            return 1;
        }
    };

    // `DRM_IOCTL_VERSION`'s lengths are in *and* out: the first call, with no buffers, is
    // how a client learns how much to allocate, and the driver reports the string's whole
    // length whether or not a buffer was given. Skipping this call is how a client ends up
    // with truncated strings and no way to notice.
    let mut arg = Version::empty();
    if let Err(errno) = ioctl(
        fd,
        DRM_IOCTL_VERSION,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    ) {
        write_err(b"drminfo: DRM_IOCTL_VERSION failed, errno ");
        write_err(Dec::of(errno as u32).bytes());
        write_err(b"\n");
        return 1;
    }
    let mut name = [0u8; FIELD];
    let mut date = [0u8; FIELD];
    let mut desc = [0u8; FIELD];
    let want = |len: usize| len.min(FIELD);
    let mut arg = Version {
        major: 0,
        minor: 0,
        patchlevel: 0,
        name_len: want(arg.name_len),
        name: name.as_mut_ptr(),
        date_len: want(arg.date_len),
        date: date.as_mut_ptr(),
        desc_len: want(arg.desc_len),
        desc: desc.as_mut_ptr(),
    };
    if let Err(errno) = ioctl(
        fd,
        DRM_IOCTL_VERSION,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    ) {
        write_err(b"drminfo: DRM_IOCTL_VERSION failed, errno ");
        write_err(Dec::of(errno as u32).bytes());
        write_err(b"\n");
        return 1;
    }
    let name = &name[..arg.name_len.min(FIELD)];

    let three_d = getparam(fd, VIRTGPU_PARAM_3D_FEATURES);
    let blob = getparam(fd, VIRTGPU_PARAM_RESOURCE_BLOB);
    let host = getparam(fd, VIRTGPU_PARAM_HOST_VISIBLE);
    let ctx = getparam(fd, VIRTGPU_PARAM_CONTEXT_INIT);
    let mask = getparam(fd, VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS);

    // `GET_CAPS` is asked for virgl v1, the one capset every virgl host has: whether the
    // blob arrives is the whole question a client has about the node. The request reports
    // nothing back but the copy — the blob's own length is not an out field — so what a
    // client can say is only whether it got one.
    let mut caps = [0u8; CAPS_BUF];
    let mut arg = GetCaps {
        cap_set_id: VIRTGPU_DRM_CAPSET_VIRGL,
        cap_set_ver: 1,
        addr: caps.as_mut_ptr(),
        size: CAPS_BUF as u32,
        pad: 0,
    };
    let caps_result = ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_GET_CAPS,
        core::ptr::addr_of_mut!(arg) as *mut u8,
    )
    .map(|_| ());

    let mut line = [0u8; 128];
    let mut at = 0usize;
    append(&mut line, &mut at, b"drminfo: ");
    append(&mut line, &mut at, name);
    append_result(&mut line, &mut at, b" 3d=", three_d);
    append_result(&mut line, &mut at, b" blob=", blob);
    append_result(&mut line, &mut at, b" ctx=", ctx);
    append_result(&mut line, &mut at, b" host=", host);
    append_result(&mut line, &mut at, b" mask=", mask);
    append(&mut line, &mut at, b" caps=");
    match caps_result {
        Ok(()) => append(&mut line, &mut at, b"ok"),
        Err(errno) => {
            append(&mut line, &mut at, b"err ");
            append(&mut line, &mut at, Dec::of(errno as u32).bytes());
        }
    }
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

    /// The three declarations above against the sizes the request numbers carry — the
    /// same check `drm.rs` makes on its own side, from the other end of the wire. A struct
    /// that grew or lost a field would make every request a number the node does not serve,
    /// and this is where that is noticed without a boot.
    #[test]
    fn the_argument_structs_are_the_sizes_their_numbers_carry() {
        assert_eq!(arg_size(DRM_IOCTL_VERSION), size_of::<Version>() as u32);
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_GETPARAM),
            size_of::<Getparam>() as u32
        );
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_GET_CAPS),
            size_of::<GetCaps>() as u32
        );
    }

    /// `drm_version`'s field order is its own: each string is its length *then* its
    /// pointer, and the three ints come first. Getting this wrong would read a length as a
    /// pointer.
    #[test]
    fn the_version_struct_is_the_layout_the_request_reads() {
        assert_eq!(size_of::<Version>(), 64);
        let v = Version::empty();
        let base = &v as *const Version as usize;
        assert_eq!(&v.name_len as *const usize as usize - base, 16);
        assert_eq!(&v.date_len as *const usize as usize - base, 32);
        assert_eq!(&v.desc_len as *const usize as usize - base, 48);
    }

    /// The decimal helper, including the zero case it would otherwise print nothing for.
    #[test]
    fn decimal_helper() {
        assert_eq!(Dec::of(0).bytes(), b"0");
        assert_eq!(Dec::of(7).bytes(), b"7");
        assert_eq!(Dec::of(308).bytes(), b"308");
        assert_eq!(Dec::of(4294967295).bytes(), b"4294967295");
    }
}
