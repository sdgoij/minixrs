//! The DRM uapi envelope for the render node (§6.10, stage 3b).
//!
//! A DRM render node is a *client* interface. Mesa — and libdrm under it — opens
//! `/dev/dri/renderD128` and speaks `ioctl`s at it, so the guest side of the accelerated
//! path is this ABI, not GL. Nothing here draws: this module is the ioctl encoding, the
//! request numbers and the argument structs, plus the decoding and encoding of the
//! requests that are answerable without the renderer's help.
//!
//! It is kept apart from the `virtio-gpu` transport for one reason: the ABI is the part
//! whose mistakes are found *last*. A wrong struct offset or a missing request number
//! does not fail here — it makes Mesa decide the device is unusable, inside a program we
//! do not yet have. So the numbers and layouts are pinned by host tests against the
//! uapi, and the requests are answered through [`RenderNode`] (the device) and
//! [`UserBuffers`] (the pointed-to buffers a struct carries) so that the decoders can be
//! exercised with neither a device nor a client process.
//!
//! Sources: `include/uapi/drm/drm.h`, `include/uapi/drm/virtgpu_drm.h` and the
//! `virtio_gpu_getparam`/`virtio_gpu_get_caps` ioctl handlers in
//! `drivers/gpu/drm/virtio/virtgpu_ioctl.c`. The semantics that are easy to get wrong
//! and are pinned by tests below: `drm_version`'s length fields report the driver's
//! *whole* string length while copying only what fits the caller's buffer, `getparam`
//! writes an **int** through a user pointer rather than into the struct, and `get_caps`
//! copies `min(the caller's size, the host's capset size)` and writes nothing back.
//!
//! Each argument struct arrives here with the stage that handles it; the request numbers
//! for the whole virtgpu set are already listed, because they are the ABI a C client has
//! to agree with.

/// `ENOTTY` — the request is not one this node serves. What Linux answers an ioctl whose
/// number is not in the driver's table, including one whose number differs only in the
/// size it encodes.
pub const ENOTTY: i32 = -25;
/// `EINVAL` — a malformed request, or a well-formed one about something that does not
/// exist (an unknown capset, an unknown parameter).
pub const EINVAL: i32 = -22;
/// `ENOSYS` — the node has no capsets at all, i.e. no GL on this host.
pub const ENOSYS: i32 = -38;
/// `EFAULT` — a user buffer could not be reached.
pub const EFAULT: i32 = -14;

/// `asm-generic/ioctl.h` field shifts.
const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = 8;
const IOC_SIZESHIFT: u32 = 16;
const IOC_DIRSHIFT: u32 = 30;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
/// The size field's width: 14 bits, which is why a request number and its argument
/// struct's size cannot be separated.
const IOC_SIZEMASK: u32 = 0x3fff;

/// `DRM_IOCTL_BASE` — every DRM request's type is `'d'`.
const DRM_IOCTL_BASE: u32 = b'd' as u32;
/// `DRM_COMMAND_BASE`: where the driver-private request numbers begin.
pub const DRM_COMMAND_BASE: u32 = 0x40;

/// `_IOC`.
const fn ioc(dir: u32, type_: u32, nr: u32, size: u32) -> u32 {
    (dir << IOC_DIRSHIFT) | (type_ << IOC_TYPESHIFT) | (size << IOC_SIZESHIFT) | (nr << IOC_NRSHIFT)
}

/// `_IOWR(name, ptr)`: the argument carries data in and out.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    ioc(IOC_READ | IOC_WRITE, type_, nr, size)
}

/// `_IOW`: the argument carries data in only.
const fn iow(type_: u32, nr: u32, size: u32) -> u32 {
    ioc(IOC_WRITE, type_, nr, size)
}

/// The argument size encoded in a request number. [`dispatch`] checks it before it reads
/// any field, which is what makes the field offsets safe by construction.
pub const fn arg_size(request: u32) -> u32 {
    (request >> IOC_SIZESHIFT) & IOC_SIZEMASK
}

/// Whether the argument carries data *in* (`_IOC_WRITE`): the caller's ioctl copies it in
/// before the request is answered.
pub const fn carries_in(request: u32) -> bool {
    (request >> IOC_DIRSHIFT) & IOC_WRITE != 0
}

/// Whether the argument carries data *out* (`_IOC_READ`): the answer is copied back.
pub const fn carries_out(request: u32) -> bool {
    (request >> IOC_DIRSHIFT) & IOC_READ != 0
}

/// `DRM_IOCTL_VERSION`, `struct drm_version`.
pub const DRM_IOCTL_VERSION: u32 = iowr(DRM_IOCTL_BASE, 0x00, 64);
/// `DRM_IOCTL_GEM_CLOSE`, `struct drm_gem_close`.
pub const DRM_IOCTL_GEM_CLOSE: u32 = iow(DRM_IOCTL_BASE, 0x09, 8);
/// `DRM_IOCTL_GET_CAP`, `struct drm_get_cap`.
pub const DRM_IOCTL_GET_CAP: u32 = iowr(DRM_IOCTL_BASE, 0x0c, 16);

/// Sizes of the virtgpu argument structs, which is what a request number carries.
const SZ_MAP: u32 = 16; // struct drm_virtgpu_map
const SZ_EXECBUFFER: u32 = 64; // struct drm_virtgpu_execbuffer
const SZ_GETPARAM: u32 = 16; // struct drm_virtgpu_getparam
const SZ_RESOURCE_CREATE: u32 = 56; // struct drm_virtgpu_resource_create
const SZ_RESOURCE_INFO: u32 = 16; // struct drm_virtgpu_resource_info
const SZ_3D_TRANSFER: u32 = 44; // struct drm_virtgpu_3d_transfer_{to,from}_host
const SZ_3D_WAIT: u32 = 8; // struct drm_virtgpu_3d_wait
const SZ_GET_CAPS: u32 = 24; // struct drm_virtgpu_get_caps
const SZ_RESOURCE_CREATE_BLOB: u32 = 56; // struct drm_virtgpu_resource_create_blob
const SZ_CONTEXT_INIT: u32 = 16; // struct drm_virtgpu_context_init

/// `DRM_IOCTL_VIRTGPU_MAP`.
pub const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x01, SZ_MAP);
/// `DRM_IOCTL_VIRTGPU_EXECBUFFER`.
pub const DRM_IOCTL_VIRTGPU_EXECBUFFER: u32 =
    iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x02, SZ_EXECBUFFER);
/// `DRM_IOCTL_VIRTGPU_GETPARAM`.
pub const DRM_IOCTL_VIRTGPU_GETPARAM: u32 =
    iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x03, SZ_GETPARAM);
/// `DRM_IOCTL_VIRTGPU_RESOURCE_CREATE`.
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 =
    iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x04, SZ_RESOURCE_CREATE);
/// `DRM_IOCTL_VIRTGPU_RESOURCE_INFO`.
pub const DRM_IOCTL_VIRTGPU_RESOURCE_INFO: u32 =
    iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x05, SZ_RESOURCE_INFO);
/// `DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST`.
pub const DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST: u32 =
    iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x06, SZ_3D_TRANSFER);
/// `DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST`.
pub const DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST: u32 =
    iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x07, SZ_3D_TRANSFER);
/// `DRM_IOCTL_VIRTGPU_WAIT`.
pub const DRM_IOCTL_VIRTGPU_WAIT: u32 = iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x08, SZ_3D_WAIT);
/// `DRM_IOCTL_VIRTGPU_GET_CAPS`.
pub const DRM_IOCTL_VIRTGPU_GET_CAPS: u32 =
    iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x09, SZ_GET_CAPS);
/// `DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB`.
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB: u32 = iowr(
    DRM_IOCTL_BASE,
    DRM_COMMAND_BASE + 0x0a,
    SZ_RESOURCE_CREATE_BLOB,
);
/// `DRM_IOCTL_VIRTGPU_CONTEXT_INIT`.
pub const DRM_IOCTL_VIRTGPU_CONTEXT_INIT: u32 =
    iowr(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x0b, SZ_CONTEXT_INIT);

/// `VIRTGPU_PARAM_3D_FEATURES` — whether the host has GL at all.
pub const VIRTGPU_PARAM_3D_FEATURES: u64 = 1;
/// `VIRTGPU_PARAM_CAPSET_QUERY_FIX` — the host gets the capset query right. Always 1 here:
/// this port's own capset path was fixed by construction (see `virtio_gpu.rs`'s
/// `RESP_OK_DISPLAY_INFO` note for the same class of bug on the host side).
pub const VIRTGPU_PARAM_CAPSET_QUERY_FIX: u64 = 2;
/// `VIRTGPU_PARAM_RESOURCE_BLOB` — `DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB` is available.
pub const VIRTGPU_PARAM_RESOURCE_BLOB: u64 = 3;
/// `VIRTGPU_PARAM_HOST_VISIBLE` — host blobs can be mapped into the guest.
pub const VIRTGPU_PARAM_HOST_VISIBLE: u64 = 4;
/// `VIRTGPU_PARAM_CROSS_DEVICE` — resources can be shared across virtio devices.
pub const VIRTGPU_PARAM_CROSS_DEVICE: u64 = 5;
/// `VIRTGPU_PARAM_CONTEXT_INIT` — `DRM_IOCTL_VIRTGPU_CONTEXT_INIT` is available.
pub const VIRTGPU_PARAM_CONTEXT_INIT: u64 = 6;
/// `VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs` — a bitmask of the capsets the host offers, bit
/// `id - 1` per id (so virgl is bit 0).
pub const VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS: u64 = 7;

/// `VIRTGPU_DRM_CAPSET_VIRGL`.
pub const VIRTGPU_DRM_CAPSET_VIRGL: u32 = 1;
/// `VIRTGPU_DRM_CAPSET_VIRGL2`.
pub const VIRTGPU_DRM_CAPSET_VIRGL2: u32 = 2;

/// `DRM_CAP_DUMB_BUFFER`: the mode-setting dumb-buffer ioctl. A render node has none.
pub const DRM_CAP_DUMB_BUFFER: u64 = 0x1;
/// `DRM_CAP_PRIME`: dma-buf import/export. Not yet.
pub const DRM_CAP_PRIME: u64 = 0x5;

/// The most a single `GET_CAPS` will fetch: the capset blob is ~3 KiB (virgl v1 is 308
/// bytes), and the copy is driven from a scratch buffer of this size because the device
/// hands over a slice, not the caller's memory.
const GET_CAPS_SCRATCH: usize = 4096;

/// The driver's identity, what `DRM_IOCTL_VERSION` reports. Strings, not lengths, because
/// the request's protocol is what decides how much of each the caller gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrmVersion {
    pub major: u32,
    pub minor: u32,
    pub patchlevel: u32,
    pub name: &'static str,
    pub date: &'static str,
    pub desc: &'static str,
}

/// The device behind the ABI.
///
/// `get_caps` takes a slice rather than a user address because the device's job is to
/// produce the blob; getting it into the caller's memory is [`UserBuffers`]' business,
/// which is what keeps this trait testable without a client.
pub trait RenderNode {
    /// `DRM_IOCTL_VERSION`.
    fn version(&self) -> DrmVersion;

    /// `DRM_IOCTL_GET_CAP`: the value of a generic DRM capability, or `EINVAL` for one
    /// this node does not know.
    fn get_cap(&mut self, capability: u64) -> Result<u64, i32>;

    /// `DRM_IOCTL_VIRTGPU_GETPARAM`: the value of a virtgpu parameter, or `EINVAL` for one
    /// it does not know.
    fn getparam(&mut self, param: u64) -> Result<i32, i32>;

    /// `DRM_IOCTL_VIRTGPU_GET_CAPS`: fill `out` with the front of the capset blob and
    /// return the blob's *whole* length, or `ENOSYS` when there are no capsets (no GL on
    /// the host) and `EINVAL` for an id or version the host does not have.
    fn get_caps(&mut self, capset_id: u32, version: u32, out: &mut [u8]) -> Result<usize, i32>;
}

/// The pointed-to buffers a DRM argument struct carries.
///
/// The structs hold *user* addresses (`drm_version.name`, `drm_virtgpu_getparam.value`,
/// `drm_virtgpu_get_caps.addr`), which a real node reaches with `safecopy_to`. Keeping
/// that behind a trait is what lets the decoders be tested without a user process, and it
/// is also the only place a request can fail with `EFAULT`.
pub trait UserBuffers {
    /// Copy `src` to the user address `addr`.
    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), i32>;
}

/// The `u32` at `off`, or 0 when the argument is short. [`dispatch`] checks the encoded
/// size first, so a short argument cannot get this far.
fn rd_u32(arg: &[u8], off: usize) -> u32 {
    match arg.get(off..off + 4) {
        Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        None => 0,
    }
}

/// The `u64` at `off`, or 0 when the argument is short.
fn rd_u64(arg: &[u8], off: usize) -> u64 {
    match arg.get(off..off + 8) {
        Some(s) => u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]),
        None => 0,
    }
}

/// Store `v` as a `u32` at `off`. The offset is one of this module's own constants and
/// [`dispatch`] has already checked the argument is at least the size the request number
/// encodes, so the arm that would drop the store is unreachable.
fn wr_u32(arg: &mut [u8], off: usize, v: u32) {
    if let Some(s) = arg.get_mut(off..off + 4) {
        s.copy_from_slice(&v.to_le_bytes());
    }
}

/// Store `v` as a `u64` at `off`.
fn wr_u64(arg: &mut [u8], off: usize, v: u64) {
    if let Some(s) = arg.get_mut(off..off + 8) {
        s.copy_from_slice(&v.to_le_bytes());
    }
}

/// `drm_copy_field`: hand the caller `min(value.len(), the length it passed)` bytes of
/// `value` and report `value`'s whole length.
///
/// The split is the whole point of the request: libdrm calls `DRM_IOCTL_VERSION` twice,
/// the first time with a null pointer and a zero length, and learns from the returned
/// lengths how much to allocate. Reporting the *copied* length instead would leave it
/// allocating nothing. A null pointer with a positive length copies nothing but still
/// reports the length, which is what the host does.
fn copy_field(
    users: &mut impl UserBuffers,
    arg: &mut [u8],
    len_off: usize,
    ptr_off: usize,
    value: &str,
) -> Result<(), i32> {
    let want = rd_u64(arg, len_off) as usize;
    let addr = rd_u64(arg, ptr_off);
    let bytes = value.as_bytes();
    let n = bytes.len().min(want);
    if n > 0 && addr != 0 {
        users.write(addr, &bytes[..n])?;
    }
    wr_u64(arg, len_off, bytes.len() as u64);
    Ok(())
}

/// Whether `arg` is at least as long as the size the request's number encodes.
///
/// Every field offset below relies on this having been checked, which is why each arm
/// checks it rather than the guard sitting in front of the match: a request this node does
/// not serve is `ENOTTY` whatever size it carries, as it is on the host, and only a
/// request it *does* serve can be too short.
fn fits(arg: &[u8], request: u32) -> bool {
    (arg.len() as u32) >= arg_size(request)
}

/// Answer one ioctl. `request` is the number as the client sent it and `arg` is the
/// argument struct, already copied in; whatever `dispatch` writes into `arg` is what the
/// caller's ioctl sees afterwards. The return is the ioctl's result — 0 or a negative
/// errno, as a driver replies.
pub fn dispatch(
    node: &mut impl RenderNode,
    users: &mut impl UserBuffers,
    request: u32,
    arg: &mut [u8],
) -> i32 {
    match request {
        DRM_IOCTL_VERSION => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let v = node.version();
            wr_u32(arg, 0, v.major);
            wr_u32(arg, 4, v.minor);
            wr_u32(arg, 8, v.patchlevel);
            // struct drm_version packs each string as its length *then* its pointer.
            let r = copy_field(users, arg, 16, 24, v.name)
                .and_then(|()| copy_field(users, arg, 32, 40, v.date))
                .and_then(|()| copy_field(users, arg, 48, 56, v.desc));
            match r {
                Ok(()) => 0,
                Err(e) => e,
            }
        }
        DRM_IOCTL_GET_CAP => {
            if !fits(arg, request) {
                return EINVAL;
            }
            // The value field is a plain out field here, and it is zeroed first so that a
            // capability the node knows but does not have reads as 0 (as the host does).
            let capability = rd_u64(arg, 0);
            wr_u64(arg, 8, 0);
            match node.get_cap(capability) {
                Ok(value) => {
                    wr_u64(arg, 8, value);
                    0
                }
                Err(e) => e,
            }
        }
        DRM_IOCTL_VIRTGPU_GETPARAM => {
            if !fits(arg, request) {
                return EINVAL;
            }
            // `drm_virtgpu_getparam.value` is a *user pointer*, and the value written
            // through it is an `int` — four bytes, not the eight the field is wide.
            let param = rd_u64(arg, 0);
            let addr = rd_u64(arg, 8);
            match node.getparam(param) {
                Ok(value) => match users.write(addr, &value.to_le_bytes()) {
                    Ok(()) => 0,
                    Err(e) => e,
                },
                Err(e) => e,
            }
        }
        DRM_IOCTL_VIRTGPU_GET_CAPS => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let capset_id = rd_u32(arg, 0);
            let version = rd_u32(arg, 4);
            let addr = rd_u64(arg, 8);
            let size = rd_u32(arg, 16);
            if size == 0 {
                return EINVAL;
            }
            let want = (size as usize).min(GET_CAPS_SCRATCH);
            let mut blob = [0u8; GET_CAPS_SCRATCH];
            match node.get_caps(capset_id, version, &mut blob[..want]) {
                // The copy is the smaller of the caller's buffer and the blob, which is
                // what the host does; the struct has no out field to report the rest.
                Ok(whole) => {
                    let n = whole.min(want);
                    if n > 0 {
                        match users.write(addr, &blob[..n]) {
                            Ok(()) => 0,
                            Err(e) => e,
                        }
                    } else {
                        0
                    }
                }
                Err(e) => e,
            }
        }
        _ => ENOTTY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device that answers everything, so the tests exercise the *encoding* rather than
    /// the device.
    struct FakeNode;

    const NAME: &str = "virtio_gpu";
    const DATE: &str = "20260101";
    const DESC: &str = "minixrs virtio-gpu render node";

    impl RenderNode for FakeNode {
        fn version(&self) -> DrmVersion {
            DrmVersion {
                major: 1,
                minor: 2,
                patchlevel: 3,
                name: NAME,
                date: DATE,
                desc: DESC,
            }
        }
        fn get_cap(&mut self, capability: u64) -> Result<u64, i32> {
            match capability {
                DRM_CAP_DUMB_BUFFER => Ok(0),
                DRM_CAP_PRIME => Ok(0),
                _ => Err(EINVAL),
            }
        }
        fn getparam(&mut self, param: u64) -> Result<i32, i32> {
            match param {
                VIRTGPU_PARAM_3D_FEATURES => Ok(1),
                VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS => Ok(0b1),
                _ => Err(EINVAL),
            }
        }
        fn get_caps(&mut self, capset_id: u32, version: u32, out: &mut [u8]) -> Result<usize, i32> {
            const BLOB: &[u8] = b"virgl-capset-blob";
            if capset_id != VIRTGPU_DRM_CAPSET_VIRGL || version > 1 {
                return Err(EINVAL);
            }
            let n = BLOB.len().min(out.len());
            out[..n].copy_from_slice(&BLOB[..n]);
            Ok(BLOB.len())
        }
    }

    /// A client's memory, as far as these requests are concerned: a few named buffers, so
    /// that a copy is checked *where it landed* rather than only that it happened. This is
    /// what `safecopy_to` is in the real node.
    struct FakeUser {
        slots: [(u64, [u8; 64], usize); 8],
        count: usize,
        fail: bool,
    }

    impl FakeUser {
        fn new() -> Self {
            Self {
                slots: [(0, [0u8; 64], 0); 8],
                count: 0,
                fail: false,
            }
        }

        /// What was copied to `addr`, if anything was.
        fn at(&self, addr: u64) -> Option<&[u8]> {
            self.slots[..self.count]
                .iter()
                .find(|(a, _, _)| *a == addr)
                .map(|(_, buf, n)| &buf[..*n])
        }

        /// How many copies were made, which is how "nothing was copied" is said.
        fn writes(&self) -> usize {
            self.count
        }
    }

    impl UserBuffers for FakeUser {
        fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), i32> {
            if self.fail {
                return Err(EFAULT);
            }
            let n = src.len().min(64);
            if let Some(slot) = self.slots.get_mut(self.count) {
                slot.0 = addr;
                slot.1[..n].copy_from_slice(&src[..n]);
                slot.2 = n;
                self.count += 1;
            }
            Ok(())
        }
    }

    /// A `struct drm_version` with nothing in it: the state libdrm's first call leaves it
    /// in, so that a test can say what the request writes back.
    fn version_arg() -> [u8; 64] {
        [0u8; 64]
    }

    /// A `struct drm_virtgpu_getparam`: the parameter, then the pointer its value goes
    /// through.
    fn getparam_arg(param: u64, value_addr: u64) -> [u8; 16] {
        let mut arg = [0u8; 16];
        wr_u64(&mut arg, 0, param);
        wr_u64(&mut arg, 8, value_addr);
        arg
    }

    /// A `struct drm_virtgpu_get_caps`: id, version, the buffer's address and its size.
    fn get_caps_arg(id: u32, version: u32, addr: u64, size: u32) -> [u8; 24] {
        let mut arg = [0u8; 24];
        wr_u32(&mut arg, 0, id);
        wr_u32(&mut arg, 4, version);
        wr_u64(&mut arg, 8, addr);
        wr_u32(&mut arg, 16, size);
        arg
    }

    /// The request numbers, spelled out: a client computes these with `_IOWR`/`_IOW`, so
    /// any disagreement is a number the client asks about and never gets an answer to.
    #[test]
    fn the_request_numbers_are_the_uapi_ones() {
        // The DRM core's, one of each direction: `_IOWR('d', 0x00, struct drm_version)`,
        // `_IOW('d', 0x09, struct drm_gem_close)`, `_IOWR('d', 0x0c, struct drm_get_cap)`.
        assert_eq!(DRM_IOCTL_VERSION, 0xC040_6400);
        assert_eq!(DRM_IOCTL_GEM_CLOSE, 0x4008_6409);
        assert_eq!(DRM_IOCTL_GET_CAP, 0xC010_640C);

        // The virtgpu set: `_IOWR('d', 0x40 + n, sizeof(struct))`, so each number's low
        // byte is its request id and its size field is the argument struct's size.
        assert_eq!(DRM_IOCTL_VIRTGPU_MAP, 0xC010_6441);
        assert_eq!(DRM_IOCTL_VIRTGPU_EXECBUFFER, 0xC040_6442);
        assert_eq!(DRM_IOCTL_VIRTGPU_GETPARAM, 0xC010_6443);
        assert_eq!(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE, 0xC038_6444);
        assert_eq!(DRM_IOCTL_VIRTGPU_RESOURCE_INFO, 0xC010_6445);
        assert_eq!(DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST, 0xC02C_6446);
        assert_eq!(DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST, 0xC02C_6447);
        assert_eq!(DRM_IOCTL_VIRTGPU_WAIT, 0xC008_6448);
        assert_eq!(DRM_IOCTL_VIRTGPU_GET_CAPS, 0xC018_6449);
        assert_eq!(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, 0xC038_644A);
        assert_eq!(DRM_IOCTL_VIRTGPU_CONTEXT_INIT, 0xC010_644B);

        for r in [
            DRM_IOCTL_VERSION,
            DRM_IOCTL_GET_CAP,
            DRM_IOCTL_VIRTGPU_GETPARAM,
            DRM_IOCTL_VIRTGPU_GET_CAPS,
        ] {
            assert_eq!((r >> 8) & 0xff, DRM_IOCTL_BASE);
            assert_eq!(r >> 30, IOC_READ | IOC_WRITE);
        }
        assert_eq!(DRM_IOCTL_GEM_CLOSE >> 30, IOC_WRITE);
    }

    /// The direction bits, which decide whether an argument is copied in, out, or both.
    /// `DRM_IOWR` is both (nearly all of these), `DRM_IOW` in only (`GEM_CLOSE`).
    #[test]
    fn the_direction_bits_say_which_way_the_argument_travels() {
        for request in [
            DRM_IOCTL_VERSION,
            DRM_IOCTL_GET_CAP,
            DRM_IOCTL_VIRTGPU_GETPARAM,
            DRM_IOCTL_VIRTGPU_GET_CAPS,
            DRM_IOCTL_VIRTGPU_EXECBUFFER,
        ] {
            assert!(carries_in(request), "{request:#x} must carry in");
            assert!(carries_out(request), "{request:#x} must carry out");
        }
        assert!(carries_in(DRM_IOCTL_GEM_CLOSE));
        assert!(!carries_out(DRM_IOCTL_GEM_CLOSE));
    }

    /// `getparam`'s value field is a user *pointer* and the value behind it is an `int`,
    /// not the eight bytes the field is wide: writing eight would overwrite whatever the
    /// caller keeps next to it.
    #[test]
    fn getparam_writes_an_int_through_the_users_pointer() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = getparam_arg(VIRTGPU_PARAM_3D_FEATURES, 0x4000);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GETPARAM, &mut arg),
            0
        );
        assert_eq!(users.at(0x4000), Some(&[1u8, 0, 0, 0][..]));

        let mut arg = getparam_arg(VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS, 0x4010);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GETPARAM, &mut arg),
            0
        );
        assert_eq!(users.at(0x4010), Some(&[0b1u8, 0, 0, 0][..]));
    }

    /// A parameter the node does not know is an error, and nothing is written: a caller
    /// that ignored the return code would otherwise read whatever it had left there.
    #[test]
    fn an_unknown_param_is_einval_and_writes_nothing() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = getparam_arg(VIRTGPU_PARAM_CONTEXT_INIT, 0x4000);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GETPARAM, &mut arg),
            EINVAL
        );
        assert_eq!(users.writes(), 0);
    }

    /// The version request's split, which is what libdrm's two calls depend on: lengths
    /// come back full whether or not a buffer was given, and a short buffer is filled
    /// with the front of the string rather than refused.
    #[test]
    fn version_reports_full_lengths_and_copies_what_fits() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();

        // Call one: no buffers at all, which is how the lengths are learnt.
        let mut arg = version_arg();
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VERSION, &mut arg),
            0
        );
        assert_eq!(rd_u32(&arg, 0), 1);
        assert_eq!(rd_u32(&arg, 4), 2);
        assert_eq!(rd_u32(&arg, 8), 3);
        assert_eq!(rd_u64(&arg, 16), NAME.len() as u64);
        assert_eq!(rd_u64(&arg, 32), DATE.len() as u64);
        assert_eq!(rd_u64(&arg, 48), DESC.len() as u64);
        assert_eq!(users.writes(), 0);

        // Call two: buffers sized from those lengths.
        let mut arg = version_arg();
        wr_u64(&mut arg, 16, 64);
        wr_u64(&mut arg, 24, 0x1000);
        wr_u64(&mut arg, 32, 64);
        wr_u64(&mut arg, 40, 0x2000);
        wr_u64(&mut arg, 48, 64);
        wr_u64(&mut arg, 56, 0x3000);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VERSION, &mut arg),
            0
        );
        assert_eq!(users.at(0x1000), Some(NAME.as_bytes()));
        assert_eq!(users.at(0x2000), Some(DATE.as_bytes()));
        assert_eq!(users.at(0x3000), Some(DESC.as_bytes()));
        assert_eq!(rd_u64(&arg, 16), NAME.len() as u64);

        // A short buffer: the front of the name, and the *whole* length still reported.
        let mut arg = version_arg();
        wr_u64(&mut arg, 16, 4);
        wr_u64(&mut arg, 24, 0x1100);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VERSION, &mut arg),
            0
        );
        assert_eq!(users.at(0x1100), Some(&NAME.as_bytes()[..4]));
        assert_eq!(rd_u64(&arg, 16), NAME.len() as u64);

        // A pointer with no buffer: nothing copied, length still reported.
        let before = users.writes();
        let mut arg = version_arg();
        wr_u64(&mut arg, 16, 64);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VERSION, &mut arg),
            0
        );
        assert_eq!(users.writes(), before);
        assert_eq!(rd_u64(&arg, 16), NAME.len() as u64);
    }

    /// A copy that cannot be made is `EFAULT`, the one error the request itself can raise.
    #[test]
    fn a_failed_copy_is_efault() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        users.fail = true;
        let mut arg = version_arg();
        wr_u64(&mut arg, 16, 64);
        wr_u64(&mut arg, 24, 0x1000);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VERSION, &mut arg),
            EFAULT
        );
    }

    /// `get_cap`'s value is an out field *in* the struct, and it is zeroed before the
    /// answer so that a capability the node knows but does not have reads as 0 — while one
    /// it does not know is an error, as the host has it.
    #[test]
    fn get_cap_zeroes_the_value_before_it_answers() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = [0xffu8; 16];
        wr_u64(&mut arg, 0, DRM_CAP_PRIME);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_GET_CAP, &mut arg),
            0
        );
        assert_eq!(rd_u64(&arg, 8), 0);

        let mut arg = [0xffu8; 16];
        wr_u64(&mut arg, 0, 0x99);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_GET_CAP, &mut arg),
            EINVAL
        );
        assert_eq!(rd_u64(&arg, 8), 0);
    }

    /// `get_caps` copies the smaller of the caller's buffer and the blob, and writes
    /// nothing back: the struct's `size` is an input, which is why a caller has to ask
    /// with a buffer it sized from `GETPARAM`'s capset size.
    #[test]
    fn get_caps_copies_the_smaller_of_the_blob_and_the_buffer() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        const BLOB: &[u8] = b"virgl-capset-blob";

        let mut arg = get_caps_arg(VIRTGPU_DRM_CAPSET_VIRGL, 1, 0x5000, 64);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GET_CAPS, &mut arg),
            0
        );
        assert_eq!(users.at(0x5000), Some(BLOB));
        assert_eq!(
            rd_u32(&arg, 16),
            64,
            "size is an input and stays as it was sent"
        );

        // A buffer smaller than the blob gets its front, not an error.
        let mut arg = get_caps_arg(VIRTGPU_DRM_CAPSET_VIRGL, 1, 0x5100, 8);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GET_CAPS, &mut arg),
            0
        );
        assert_eq!(users.at(0x5100), Some(&BLOB[..8]));

        // A zero size is refused outright rather than treated as "copy nothing".
        let mut arg = get_caps_arg(VIRTGPU_DRM_CAPSET_VIRGL, 1, 0x5200, 0);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GET_CAPS, &mut arg),
            EINVAL
        );

        // A capset the host does not have, or a version above it.
        let mut arg = get_caps_arg(VIRTGPU_DRM_CAPSET_VIRGL2, 1, 0x5300, 64);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GET_CAPS, &mut arg),
            EINVAL
        );
        let mut arg = get_caps_arg(VIRTGPU_DRM_CAPSET_VIRGL, 9, 0x5300, 64);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GET_CAPS, &mut arg),
            EINVAL
        );
    }

    /// An unknown request is `ENOTTY` — including one whose number differs from a served
    /// one only in the size it encodes, which is the whole reason the size is part of the
    /// number.
    #[test]
    fn an_unknown_request_is_enotty() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = [0u8; 64];
        assert_eq!(
            dispatch(&mut node, &mut users, 0x1234_5678, &mut arg),
            ENOTTY
        );

        // The same request id with a different size field is a different number, and one
        // this node does not serve: `ENOTTY`, where a served number given a short argument
        // is `EINVAL` (the test below).
        let wrong_size = DRM_IOCTL_VIRTGPU_GETPARAM + (16u32 << IOC_SIZESHIFT);
        assert_eq!(arg_size(wrong_size), 32);
        assert_eq!(
            dispatch(&mut node, &mut users, wrong_size, &mut arg),
            ENOTTY
        );
    }

    /// A request handed an argument shorter than the size its number encodes is refused
    /// before any field is read — the check that makes the offsets safe by construction.
    #[test]
    fn a_short_argument_is_einval() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = [0u8; 4];
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_GETPARAM, &mut arg),
            EINVAL
        );
    }
}
