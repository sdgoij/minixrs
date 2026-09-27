//! Virtio-GPU 2D framebuffer backend (virtio device ID 16).
//!
//! The virtio-gpu is the display device on QEMU riscv64/aarch64 `virt`
//! machines (they have no bochs-display / PCI VGA; L1 verified the device
//! slots into the port's existing device-ID virtio scan). This backend
//! implements the 2D command ring — RESOURCE_CREATE_2D →
//! RESOURCE_ATTACH_BACKING → SET_SCANOUT → RESOURCE_FLUSH — with the
//! framebuffer being a contiguous guest-RAM buffer (a server-owned static,
//! page-aligned) whose guest-physical address is exposed through the K3
//! char-device mmap path (`FbDevice.base`).
//!
//! Drawing is plain writes to that buffer; RESOURCE_FLUSH pushes it to the
//! display (explicit-flush semantics, unlike VGA's always-visible LFB), so
//! the server flushes after its boot pattern and consumers issue an
//! FBIOFLUSH after drawing.

use crate::DriverError;
use crate::bus::virtio::{self, VirtioDevice, VirtioPhysBuf};
use crate::video::drm;
use crate::video::fb::{
    BOCHS_DEFAULT_XRES, BOCHS_DEFAULT_YRES, FbArch, FbBitfield, FbDevice, FbFixScreeninfo,
    FbVarScreeninfo,
};
use core::cell::UnsafeCell;

/// Virtio device ID for the GPU (`.refs/minix-3.3.0` virtio spec).
pub const VIRTIO_GPU_DEVICE_ID: u16 = 0x0010;

/// 2D pixel format: B8G8R8X8_UNORM. QEMU maps the virtio-gpu formats to
/// the PIXMAN_BE_* (big-endian) variants, whose memory order on an LE host
/// is the reversed component order: B8G8R8X8 -> BE_b8g8r8x8 reads memory
/// as [B,G,R,X] — exactly the port's XRGB8888 byte order. X8R8G8B8 would
/// map to BE_x8r8g8b8 (memory [X,R,G,B]) and swap R/G on the display.
const VIRTIO_GPU_FORMAT_B8G8R8X8: u32 = 2;

/// Command types (`linux/virtio_gpu.h`).
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;

/// 3D command types — where a virgl context and its capsets live (§6.10, 3a).
const CMD_GET_CAPSET_INFO: u32 = 0x0108;
const CMD_GET_CAPSET: u32 = 0x0109;
const CMD_CTX_CREATE: u32 = 0x0200;
const CMD_CTX_DESTROY: u32 = 0x0201;
const CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
const CMD_RESOURCE_CREATE_3D: u32 = 0x0204;
const CMD_TRANSFER_TO_HOST_3D: u32 = 0x0205;
const CMD_TRANSFER_FROM_HOST_3D: u32 = 0x0206;
/// The one every GL call actually travels in, so a probe that leaves it untried has not
/// touched the path Mesa will use.
const CMD_SUBMIT_3D: u32 = 0x0207;
/// Releasing a resource matters more than it looks: the host refuses a *duplicate*
/// resource id, so a probe that leaked one would break the next 3D user's create.
const CMD_RESOURCE_UNREF: u32 = 0x0102;

/// `VIRTIO_GPU_F_*` feature bits. `VIRGL` is the one a host may not have: without
/// it the device offers no capset and no context, and 3D commands come back as
/// errors rather than doing nothing.
const VIRTIO_GPU_F_VIRGL: u8 = 0;
const VIRTIO_GPU_F_RESOURCE_BLOB: u8 = 3;
const VIRTIO_GPU_F_CONTEXT_INIT: u8 = 4;

/// What a render node asks for. `virgl` and `resource-blob` are acknowledged only
/// if the device offers them (the transport masks by the host word), and `blob` is
/// inert until a command uses it.
///
/// `context-init` is deliberately *not* requested: acknowledging it reinterprets
/// the control header's last word as `ring_idx` instead of padding, and this port
/// has no multi-ring use for it. It is still reported from the device's word.
static GPU_FEATURES: &[virtio::VirtioFeature] = &[
    virtio::VirtioFeature {
        name: "virgl",
        bit: VIRTIO_GPU_F_VIRGL,
        host_support: 0,
        guest_support: 1,
    },
    virtio::VirtioFeature {
        name: "resource-blob",
        bit: VIRTIO_GPU_F_RESOURCE_BLOB,
        host_support: 0,
        guest_support: 1,
    },
    virtio::VirtioFeature {
        name: "context-init",
        bit: VIRTIO_GPU_F_CONTEXT_INIT,
        host_support: 0,
        guest_support: 0,
    },
];

/// Response types.
const RESP_OK_NODATA: u32 = 0x1100;
/// `RESP_OK_DISPLAY_INFO` — nothing here sends `GET_DISPLAY_INFO`, but the success
/// responses are one *enum*, so this member is named to keep the two capset answers
/// below at their published values. Omitting it is the mistake this driver made once:
/// every later member shifts down, and a correct `GET_CAPSET_INFO` answer was then
/// rejected as an error because 0x1101 was expected for it instead of 0x1102.
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const RESP_OK_CAPSET_INFO: u32 = 0x1102;
const RESP_OK_CAPSET: u32 = 0x1103;
const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
const RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
const RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;

/// `struct virtio_gpu_resp_capset_info`: the 24-byte header, then capset_id,
/// capset_max_version, capset_max_size and padding — 40 bytes.
const RESP_CAPSET_INFO_LEN: u32 = 40;
const RESP_CAPSET_INFO_ID: usize = 24;
const RESP_CAPSET_INFO_VERSION: usize = 28;
const RESP_CAPSET_INFO_SIZE: usize = 32;

/// Largest capset blob this driver will ask for. `GET_CAPSET` is answered with the
/// blob in the response, so the driver must supply a buffer of at least
/// `capset_max_size`; a host that reports more than this is left unfetched rather
/// than overrun. (virgl's capset is ~3 KiB.)
const CAPSET_BUF_LEN: usize = 4096;

/// Largest command this driver sends (`CTX_CREATE`, 96 bytes).
const CMD_BUF_LEN: usize = 128;

/// The control queue's command slot.
///
/// Both slots live at module scope rather than inside [`VirtioGpuArch`], which is not
/// a detail of style: a descriptor's address must be a **guest-physical** address the
/// host can map, and the transport derives it from this process's *image* VA→PA
/// delta. A `VirtioGpuArch` built on the stack puts the buffers in the stack region —
/// a different VA with no such translation — and QEMU answers the first command with
/// `virtio: bogus descriptor or out of resources`, which stops the device. Keeping
/// them here also keeps the struct small enough to hold in a register-starved driver.
struct GpuCmdCell(UnsafeCell<[u8; CMD_BUF_LEN]>);
unsafe impl Sync for GpuCmdCell {}
impl GpuCmdCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; CMD_BUF_LEN]))
    }
    fn get(&self) -> *mut [u8; CMD_BUF_LEN] {
        self.0.get()
    }
}
static GPU_CMD: GpuCmdCell = GpuCmdCell::new();

/// The control queue's response slot, sized for a capset blob.
struct GpuRespCell(UnsafeCell<[u8; CAPSET_BUF_LEN]>);
unsafe impl Sync for GpuRespCell {}
impl GpuRespCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; CAPSET_BUF_LEN]))
    }
    fn get(&self) -> *mut [u8; CAPSET_BUF_LEN] {
        self.0.get()
    }
}
static GPU_RESP: GpuRespCell = GpuRespCell::new();

/// Geometry of the texture the 3D probe round-trips: 64×64, 4 bytes per pixel.
const PROBE_TEX_W: u32 = 64;
const PROBE_TEX_H: u32 = 64;

/// Bytes of that texture, and so of the transfer that carries the whole of it. A
/// page multiple, so the backing is a whole number of pages.
pub const PROBE_TEX_BYTES: u32 = PROBE_TEX_W * PROBE_TEX_H * 4;

/// `PIPE_TEXTURE_2D` — the Gallium target a texture resource carries.
const PIPE_TEXTURE_2D: u32 = 2;

/// `VIRGL_FORMAT_B8G8R8A8_UNORM`. Note this is a *virgl* format, not the
/// `VIRTIO_GPU_FORMAT_*` space the 2D commands use: the host hands a 3D resource's
/// `format` straight to the renderer, while a 2D one is translated. The two spaces
/// coincide only for the first few values, and this is one of them.
const VIRGL_FORMAT_B8G8R8A8_UNORM: u32 = 1;

/// `VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW` — a texture bind of zero, or
/// one of the buffer binds, is rejected for a texture target.
const VIRGL_BIND_TEXTURE: u32 = (1 << 1) | (1 << 3);

/// `VIRGL_CCMD_RESOURCE_INLINE_WRITE` — the simplest virgl command that makes the *host*
/// put known bytes into a resource: no framebuffer, shader or bound state, just the
/// resource, a box and the data. That is what makes it a good `SUBMIT_3D` probe.
const VIRGL_CCMD_RESOURCE_INLINE_WRITE: u32 = 9;

/// `VIRGL_OBJECT_NULL`: a resource-level command names no object, and the renderer reads
/// only the command and the length out of the header word.
const VIRGL_OBJECT_NULL: u32 = 0;

/// Pixels one `RESOURCE_INLINE_WRITE` writes: a single row. Small because the whole
/// command travels in one descriptor — header, its eleven field dwords and the data — so
/// a texture-sized write would need a command buffer the size of the texture.
const PROBE_IW_PIXELS: u32 = 8;
const PROBE_IW_BYTES: u32 = PROBE_IW_PIXELS * 4;
/// The row is packed, so both strides are its length: the box is one row deep.
const PROBE_IW_STRIDE: u32 = PROBE_IW_BYTES;
const PROBE_IW_LAYER_STRIDE: u32 = PROBE_IW_BYTES;
/// Bytes of the command buffer: a header dword, the eleven field dwords it shares with a
/// transfer command, then the data.
const IW_CMD_LEN: usize = 4 * (12 + PROBE_IW_PIXELS as usize);

/// The inline write has to cover less than the whole texture: the read-back switches what
/// it expects at the edge of the named region, and covering everything would leave nothing
/// to switch on — it would then pass on a write that spilled over the whole texture.
const _: () = assert!(PROBE_IW_BYTES < PROBE_TEX_BYTES);

/// The guest side of the round-trip resource, and the only place the transfers leave
/// evidence. Page-aligned so the whole of it is one `ATTACH_BACKING` range, and at
/// module scope for the same reason the command slots are: a descriptor address has to
/// be a guest-physical address the host can map, which only an image static has.
#[repr(align(4096))]
struct ProbeBufCell(UnsafeCell<[u8; PROBE_TEX_BYTES as usize]>);
unsafe impl Sync for ProbeBufCell {}
impl ProbeBufCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; PROBE_TEX_BYTES as usize]))
    }
    fn get(&self) -> *mut [u8; PROBE_TEX_BYTES as usize] {
        self.0.get()
    }
}
static PROBE_BUF: ProbeBufCell = ProbeBufCell::new();

/// Resource ID of the single scanout resource this backend manages.
const RESOURCE_ID: u32 = 1;

/// Framebuffer geometry (matches `BOCHS_DEFAULT_XRES`/`YRES`, which the
/// wserver/fbterm hardcode).
const XRES: u32 = BOCHS_DEFAULT_XRES;
const YRES: u32 = BOCHS_DEFAULT_YRES;
const FB_SIZE: u64 = (XRES as u64) * (YRES as u64) * 4;

/// `struct virtio_gpu_ctrl_hdr` — 24 bytes: type, flags, fence_id, ctx_id,
/// ring_idx, padding[3]. There is NO resource_id in the header; every
/// command carries its resource reference in its own body fields.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CtrlHdr {
    type_: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    ring_idx: u8,
    padding: [u8; 3],
}

impl CtrlHdr {
    const fn new(type_: u32) -> Self {
        Self {
            type_,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            ring_idx: 0,
            padding: [0; 3],
        }
    }
}

/// `struct virtio_gpu_resource_create_2d` — 44 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ResourceCreate2D {
    hdr: CtrlHdr,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

/// `struct virtio_gpu_mem_entry` — 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MemEntry {
    addr: u64,
    length: u32,
    padding: u32,
}

/// `struct virtio_gpu_resource_attach_backing` with one entry — 48 bytes:
/// hdr, resource_id, nr_entries, then the entries (QEMU reads them from
/// offset 32).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct AttachBacking {
    hdr: CtrlHdr,
    resource_id: u32,
    nr_entries: u32,
    entries: [MemEntry; 1],
}

/// `struct virtio_gpu_rect` — 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// `struct virtio_gpu_set_scanout` — 52 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct SetScanout {
    hdr: CtrlHdr,
    r: Rect,
    scanout_id: u32,
    resource_id: u32,
}

/// `struct virtio_gpu_transfer_to_host_2d` — 56 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct TransferToHost2D {
    hdr: CtrlHdr,
    r: Rect,
    offset: u64,
    resource_id: u32,
    padding: u32,
}

/// `struct virtio_gpu_resource_flush` — 48 bytes: hdr, rect, resource_id,
/// padding.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ResourceFlush {
    hdr: CtrlHdr,
    r: Rect,
    resource_id: u32,
    padding: u32,
}

/// `struct virtio_gpu_get_capset_info` — 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct GetCapsetInfo {
    hdr: CtrlHdr,
    capset_index: u32,
    padding: u32,
}

/// `struct virtio_gpu_get_capset` — 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct GetCapset {
    hdr: CtrlHdr,
    capset_id: u32,
    capset_version: u32,
}

/// `struct virtio_gpu_ctx_create` — 96 bytes: hdr, nlen, context_init, then the
/// 64-byte debug name (QEMU reads exactly `nlen` of it).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CtxCreate {
    hdr: CtrlHdr,
    nlen: u32,
    context_init: u32,
    debug_name: [u8; 64],
}

/// `struct virtio_gpu_ctx_destroy` — 32 bytes: hdr, padding.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CtxDestroy {
    hdr: CtrlHdr,
    padding: u32,
}

/// `struct virtio_gpu_resource_create_3d` — 72 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ResourceCreate3D {
    hdr: CtrlHdr,
    resource_id: u32,
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
    padding: u32,
}

/// `struct virtio_gpu_box` — 24 bytes. The extent fields are spelled for their role in
/// a transfer rather than as `width`/`height`/`depth`, which the resource structs use
/// for a different meaning (the texture's own size).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Box3D {
    x: u32,
    y: u32,
    z: u32,
    w: u32,
    h: u32,
    d: u32,
}

/// `struct virtio_gpu_transfer_host_3d` — 72 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct TransferHost3D {
    hdr: CtrlHdr,
    area: Box3D,
    offset: u64,
    resource_id: u32,
    level: u32,
    stride: u32,
    layer_stride: u32,
}

/// `struct virtio_gpu_resource_unref`, `…_ctx_resource` — 32 bytes: hdr, resource_id,
/// padding. Named for the body they share rather than for one caller, since
/// `RESOURCE_UNREF`, `CTX_ATTACH_RESOURCE` and `CTX_DETACH_RESOURCE` are all this shape.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ResourceRef {
    hdr: CtrlHdr,
    resource_id: u32,
    padding: u32,
}

/// `struct virtio_gpu_cmd_submit` — 32 bytes. `size` counts the command buffer that
/// follows it, which is not a field but the rest of the same out descriptor.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CmdSubmit3D {
    hdr: CtrlHdr,
    size: u32,
    padding: u32,
}

/// What `GET_CAPSET_INFO` says about one capset index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapsetInfo {
    /// The capset the host renders with (virgl is 1 for v1, 2 for v2).
    pub id: u32,
    /// The highest version of it the host speaks.
    pub max_version: u32,
    /// The blob's size, which `GET_CAPSET` must be given room for.
    pub max_size: u32,
}

/// A render node's 3D capability, as the boot report describes it (§6.10, 3a).
///
/// Every field is what the *device* answered, never what a host was assumed to
/// have (D8); all of them false and zero is a machine with no render node at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gpu3d {
    /// A `virtio-gpu` device was found.
    pub device: bool,
    /// It offered `VIRTIO_GPU_F_VIRGL`: without it there is no capset and no context.
    pub virgl: bool,
    /// It offered `VIRTIO_GPU_F_RESOURCE_BLOB` (the path 3b's DRM node needs).
    pub blob: bool,
    /// It offered `VIRTIO_GPU_F_CONTEXT_INIT` (reported, not negotiated).
    pub context_init: bool,
    /// Queues the transport configured — 2 for a device with a cursor queue.
    pub queues: u8,
    /// `GET_CAPSET_INFO` index 0's answer, when it could be asked.
    pub capset: CapsetInfo,
    /// How many capsets the host offered, walked at negotiation. One where the renderer
    /// speaks only virgl v1; a second appears when it also offers virgl2, and the set as a
    /// bitmask is what `VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs` reports.
    pub capset_count: u32,
    /// When that query did not answer as expected: the response type the device gave,
    /// or 0 when nothing came back before the spin budget ran out. Kept because
    /// "capset 0 v0 size 0" reads the same for a refused query and a genuine zero.
    pub capset_rtype: u32,
    /// Bytes of capset blob the host returned, when it was small enough to fetch.
    pub capset_len: u32,
    /// A context was created *and* destroyed — the control path beyond the
    /// read-only queries works.
    pub ctx: bool,
    /// Bytes of the 3D round-trip that came back equal *before* the first difference;
    /// [`PROBE_TEX_BYTES`] is the whole of it, so anything less is a transfer that moved
    /// nothing, too little, or the data reordered.
    ///
    /// This, and not a response type, is the evidence for the transfer path: the host
    /// answers `OK`/`NODATA` to a transfer it refused, because QEMU discards the
    /// renderer's return value, so a transfer that did nothing and one that worked are
    /// indistinguishable from the control queue alone.
    pub xfer_matched: u32,
    /// The same count for the `SUBMIT_3D` half, where the bytes are the *host's*: one
    /// `RESOURCE_INLINE_WRITE` command names a row of the texture, and the read-back must
    /// show that row changed and the rest of the texture untouched. A transfer cannot
    /// stand in for this — it only moves bytes the guest already had.
    pub submit_matched: u32,
}

/// Configure a `virtio-gpu` device's queues before `DRIVER_OK`.
///
/// Queue 0 is the control queue every command goes on. Queue 1 — the cursorq — is
/// configured too even though this driver never submits on it, because a **GL**
/// device polls its cursor queue from a host-side timer whether or not the guest
/// uses it, and a queue that was never configured answers that poll with "bogus
/// descriptor or out of resources". QEMU treats that as a device error, which stops
/// *both* queues: the symptom is a control command that gets no answer at all. A
/// one-queue device is legal, so a missing cursorq is not an error. Returns the
/// number of queues configured.
fn setup_queues(dev: &mut VirtioDevice) -> Result<u8, DriverError> {
    virtio::virtio_alloc_queue(dev, 0).map_err(|_| DriverError::Io)?;
    let cursorq = u8::from(virtio::virtio_alloc_queue(dev, 1).is_ok());
    virtio::virtio_device_ready(dev);
    Ok(1 + cursorq)
}

/// Bytes of a `repr(C)` struct for the command ring.
fn struct_bytes<T>(v: &T) -> &[u8] {
    // Safety: repr(C) structs are plain data; converting to bytes for the
    // virtqueue descriptor is the standard driver pattern (host reads the
    // same layout).
    unsafe { core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>()) }
}

/// The byte the round-trip expects at `i`: a value that depends on the *position*, so a
/// transfer that moved too little, shifted the data by a row, or reordered the channels
/// shows up as a short match rather than as a pass.
fn pattern_byte(i: usize) -> u8 {
    (i.wrapping_mul(31).wrapping_add(7) & 0xff) as u8
}

/// The bytes the probe makes the *host* write, and so the evidence for `SUBMIT_3D`: the
/// guest never puts these in the buffer itself. The complement of [`pattern_byte`], so it
/// differs from the transfer's pattern at every offset.
fn host_pattern_byte(i: usize) -> u8 {
    !pattern_byte(i)
}

/// What the buffer must hold after the inline write and the read-back: the host's pattern
/// over the region the command named, and the transfer's own pattern everywhere else. The
/// switch is the point — a command that wrote past its box makes the tail a mismatch
/// instead of a pass.
fn expected_after_inline_write(i: usize) -> u8 {
    if i < PROBE_IW_BYTES as usize {
        host_pattern_byte(i)
    } else {
        pattern_byte(i)
    }
}

/// One virgl command's header word: 8 bits of command, 8 of object, and 16 of length — in
/// dwords *after* the header (`VIRGL_CMD0`), which is how the renderer advances through
/// the buffer.
fn virgl_cmd(cmd: u32, obj: u32, len_dwords: u32) -> u32 {
    cmd | (obj << 8) | (len_dwords << 16)
}

/// The one-command buffer that makes the host write [`host_pattern_byte`] over the first
/// row of `resource_id`: a `RESOURCE_INLINE_WRITE` whose payload is the eleven dwords it
/// shares with a transfer command, then the data.
fn inline_write_command(resource_id: u32) -> [u8; IW_CMD_LEN] {
    let mut words = [0u32; 12 + PROBE_IW_PIXELS as usize];
    words[0] = virgl_cmd(
        VIRGL_CCMD_RESOURCE_INLINE_WRITE,
        VIRGL_OBJECT_NULL,
        11 + PROBE_IW_PIXELS,
    );
    words[1] = resource_id;
    // words[2] is the level and words[3] a usage word no decoder reads.
    words[4] = PROBE_IW_STRIDE;
    words[5] = PROBE_IW_LAYER_STRIDE;
    // words[6..=8] are the box origin, which is zero.
    words[9] = PROBE_IW_PIXELS;
    words[10] = 1;
    words[11] = 1;
    for (dword, w) in words[12..].iter_mut().enumerate() {
        let at = dword * 4;
        *w = u32::from_le_bytes([
            host_pattern_byte(at),
            host_pattern_byte(at + 1),
            host_pattern_byte(at + 2),
            host_pattern_byte(at + 3),
        ]);
    }
    let mut bytes = [0u8; IW_CMD_LEN];
    for (dst, w) in bytes.chunks_exact_mut(4).zip(words.iter()) {
        dst.copy_from_slice(&w.to_le_bytes());
    }
    bytes
}

/// The driver error a response type means. A 3D command sent to a device that
/// never offered `VIRTIO_GPU_F_VIRGL` is answered with an error response rather
/// than nothing at all, which is what lets a probe report "no 3D" instead of
/// spinning (D7).
fn resp_error(rtype: u32) -> DriverError {
    match rtype {
        RESP_ERR_OUT_OF_MEMORY => DriverError::Busy,
        RESP_ERR_INVALID_SCANOUT_ID | RESP_ERR_INVALID_RESOURCE_ID => DriverError::InvalidArgument,
        _ => DriverError::Io,
    }
}

/// Virtio-GPU framebuffer backend.
///
/// The framebuffer memory is owned by the server (a page-aligned static);
/// the backend is told its image VA via [`VirtioGpuArch::set_fb_va`] before
/// `init`, and derives the guest-physical address from the transport's
/// VA→PA delta.
pub struct VirtioGpuArch {
    /// Framebuffer descriptor for the K3 mmap path — `base` is the
    /// guest-physical address of the buffer (what VFS/VM map).
    pub dev: FbDevice,
    /// Image VA of the server-owned framebuffer buffer (what this process
    /// reads/writes through `FbArch::mem`).
    pub fb_va: u64,
    pub var: FbVarScreeninfo,
    pub fix: FbFixScreeninfo,
    initialized: bool,
    vdev: Option<VirtioDevice>,
    /// Response type of the last command, so a caller can say *why* one did not
    /// answer as expected (0 = nothing arrived before the spin budget).
    pub last_response: u32,
}

impl VirtioGpuArch {
    pub const fn new() -> Self {
        Self {
            dev: FbDevice::new(),
            fb_va: 0,
            var: FbVarScreeninfo::new(),
            fix: FbFixScreeninfo::new(),
            initialized: false,
            vdev: None,
            last_response: 0,
        }
    }

    /// Set the image VA of the server-owned framebuffer buffer.
    pub fn set_fb_va(&mut self, va: u64) {
        self.fb_va = va;
    }

    /// Submit one command: `cmd` is copied into the command slot, the
    /// response slot is supplied writable, then we spin for the used-ring
    /// completion. Returns the response type and the number of bytes the device
    /// wrote.
    fn submit(&mut self, cmd: &[u8]) -> Result<(u32, u32), DriverError> {
        let cmd_slot: &mut [u8; CMD_BUF_LEN] = unsafe { &mut *GPU_CMD.get() };
        if cmd.len() > cmd_slot.len() {
            return Err(DriverError::InvalidArgument);
        }
        cmd_slot[..cmd.len()].copy_from_slice(cmd);
        let cmd_addr = cmd_slot.as_ptr() as u64;
        let resp_addr = GPU_RESP.get() as u64;
        let dev = self.vdev.as_mut().ok_or(DriverError::NotFound)?;
        let bufs = [
            VirtioPhysBuf {
                addr: cmd_addr,
                size: cmd.len() as u32,
                writable: false,
            },
            VirtioPhysBuf {
                addr: resp_addr,
                size: CAPSET_BUF_LEN as u32,
                writable: true,
            },
        ];
        virtio::virtio_to_queue(dev, 0, &bufs, 0).map_err(|_| DriverError::Io)?;
        let mut spins = 0u32;
        let used_len = loop {
            if let Some((_, len)) = virtio::virtio_from_queue(dev, 0) {
                break len;
            }
            spins += 1;
            if spins >= 50_000_000 {
                return Err(DriverError::Busy);
            }
            core::hint::spin_loop();
        };
        self.last_response = self.resp_u32(0);
        Ok((self.last_response, used_len))
    }

    /// Submit `cmd` and require the `NODATA` acknowledgement.
    fn send_cmd(&mut self, cmd: &[u8]) -> Result<(), DriverError> {
        match self.submit(cmd)? {
            (RESP_OK_NODATA, _) => Ok(()),
            (rtype, _) => Err(resp_error(rtype)),
        }
    }

    /// Submit `cmd`, require the response type `want`, and return how many bytes
    /// the device wrote.
    fn send_cmd_expect(&mut self, cmd: &[u8], want: u32) -> Result<u32, DriverError> {
        match self.submit(cmd)? {
            (rtype, len) if rtype == want => Ok(len),
            (rtype, _) => Err(resp_error(rtype)),
        }
    }

    /// One little-endian `u32` from the response slot. Read only after `submit` has
    /// filled it; an offset past the slot yields zero rather than a panic.
    fn resp_u32(&self, off: usize) -> u32 {
        let mut b = [0u8; 4];
        let resp: &[u8; CAPSET_BUF_LEN] = unsafe { &*GPU_RESP.get() };
        if let Some(s) = resp.get(off..off + 4) {
            b.copy_from_slice(s);
        }
        u32::from_le_bytes(b)
    }

    /// Push the guest framebuffer to the display: TRANSFER_TO_HOST_2D copies
    /// the attached guest buffer into the host scanout image, then
    /// RESOURCE_FLUSH makes the change visible.
    fn flush_inner(&mut self) -> Result<(), DriverError> {
        let rect = Rect {
            x: 0,
            y: 0,
            width: XRES,
            height: YRES,
        };
        let transfer = TransferToHost2D {
            hdr: CtrlHdr::new(CMD_TRANSFER_TO_HOST_2D),
            r: rect,
            offset: 0,
            resource_id: RESOURCE_ID,
            padding: 0,
        };
        self.send_cmd(struct_bytes(&transfer))?;
        let cmd = ResourceFlush {
            hdr: CtrlHdr::new(CMD_RESOURCE_FLUSH),
            r: rect,
            resource_id: RESOURCE_ID,
            padding: 0,
        };
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `GET_CAPSET_INFO` for one index: what capset the host renders with, how
    /// high a version it speaks and how large its blob is.
    fn capset_info(&mut self, index: u32) -> Result<CapsetInfo, DriverError> {
        let cmd = GetCapsetInfo {
            hdr: CtrlHdr::new(CMD_GET_CAPSET_INFO),
            capset_index: index,
            padding: 0,
        };
        let len = self.send_cmd_expect(struct_bytes(&cmd), RESP_OK_CAPSET_INFO)?;
        if len < RESP_CAPSET_INFO_LEN {
            return Err(DriverError::Io);
        }
        Ok(CapsetInfo {
            id: self.resp_u32(RESP_CAPSET_INFO_ID),
            max_version: self.resp_u32(RESP_CAPSET_INFO_VERSION),
            max_size: self.resp_u32(RESP_CAPSET_INFO_SIZE),
        })
    }

    /// `GET_CAPSET` for one capset, copied out of the response slot. Returns the blob's
    /// *whole* length, of which at most `out.len()` bytes are written — the split
    /// `DRM_IOCTL_VIRTGPU_GET_CAPS` needs, because a caller may ask with a buffer smaller
    /// than the blob and still has to be told how big it is.
    fn fetch_capset(&mut self, id: u32, version: u32, out: &mut [u8]) -> Result<u32, DriverError> {
        let cmd = GetCapset {
            hdr: CtrlHdr::new(CMD_GET_CAPSET),
            capset_id: id,
            capset_version: version,
        };
        let len = self.send_cmd_expect(struct_bytes(&cmd), RESP_OK_CAPSET)?;
        // The capset data follows the 24-byte control header in the same response, which
        // is why `len` is 24 more than the blob.
        let whole = len.saturating_sub(24);
        let n = (whole as usize).min(out.len());
        let resp: &[u8; CAPSET_BUF_LEN] = unsafe { &*GPU_RESP.get() };
        if let Some(s) = resp.get(24..24 + n) {
            out[..n].copy_from_slice(s);
        }
        Ok(whole)
    }

    /// `GET_CAPSET` for one capset's whole blob, left in the response slot. The blob
    /// travels in the response, so the driver must supply a buffer of `max_size`; a host
    /// that reports more than the slot holds is left unfetched (zero) rather than overrun.
    /// (Returns the blob's length.)
    fn capset_blob(&mut self, info: CapsetInfo) -> Result<u32, DriverError> {
        if (info.max_size as usize) + 24 > CAPSET_BUF_LEN {
            return Ok(0);
        }
        self.fetch_capset(info.id, info.max_version, &mut [])
    }

    /// `CTX_CREATE`: the context the host will run command buffers for. The context
    /// id is the driver's to choose and travels in the header, which is why the
    /// probe can pick one and hand it straight back.
    fn ctx_create(&mut self, ctx_id: u32, name: &[u8]) -> Result<(), DriverError> {
        let mut cmd = CtxCreate {
            hdr: CtrlHdr::new(CMD_CTX_CREATE),
            nlen: 0,
            context_init: 0,
            debug_name: [0u8; 64],
        };
        cmd.hdr.ctx_id = ctx_id;
        let n = name.len().min(cmd.debug_name.len());
        cmd.debug_name[..n].copy_from_slice(&name[..n]);
        // The host reads exactly `nlen` bytes of the name, so it counts what was
        // copied rather than the field's width.
        cmd.nlen = n as u32;
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `CTX_DESTROY`: release a context the host is holding for us.
    fn ctx_destroy(&mut self, ctx_id: u32) -> Result<(), DriverError> {
        let mut cmd = CtxDestroy {
            hdr: CtrlHdr::new(CMD_CTX_DESTROY),
            padding: 0,
        };
        cmd.hdr.ctx_id = ctx_id;
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `RESOURCE_ATTACH_BACKING` with a single guest range: one `MemEntry` covering
    /// `[pa, pa + len)`. The host maps it, and for as long as the resource lives its guest
    /// side *is* those pages — which is what gives the transfers below somewhere to read
    /// from and write to.
    fn attach_backing(&mut self, resource_id: u32, pa: u64, len: u32) -> Result<(), DriverError> {
        let cmd = AttachBacking {
            hdr: CtrlHdr::new(CMD_RESOURCE_ATTACH_BACKING),
            resource_id,
            nr_entries: 1,
            entries: [MemEntry {
                addr: pa,
                length: len,
                padding: 0,
            }],
        };
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `RESOURCE_CREATE_3D`: a resource whose contents are a *host-side* texture rather
    /// than the guest's pages. That distinction is the whole point of the round-trip: a
    /// resource is only transferred to and from, where a resource that merely *is* guest
    /// memory would copy itself and prove nothing.
    fn resource_create_3d(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
        format: u32,
        bind: u32,
    ) -> Result<(), DriverError> {
        let cmd = ResourceCreate3D {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_3D),
            resource_id,
            target: PIPE_TEXTURE_2D,
            format,
            bind,
            width,
            height,
            depth: 1,
            array_size: 1,
            last_level: 0,
            nr_samples: 0,
            flags: 0,
            padding: 0,
        };
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `CTX_ATTACH_RESOURCE`: put a resource in a context's table, so a command naming
    /// that context may use it. A transfer through a context id needs this; one through
    /// context 0 does not, which is why the probe uses a real one — it is the path the
    /// DRM node will take, and an unattached resource fails *silently*.
    fn ctx_attach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result<(), DriverError> {
        let mut cmd = ResourceRef {
            hdr: CtrlHdr::new(CMD_CTX_ATTACH_RESOURCE),
            resource_id,
            padding: 0,
        };
        cmd.hdr.ctx_id = ctx_id;
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `RESOURCE_UNREF`: drop the resource and the host's copy of it. Duplicate resource
    /// ids are refused, so a create that is not paired with this outlives this driver.
    fn resource_unref(&mut self, resource_id: u32) -> Result<(), DriverError> {
        let cmd = ResourceRef {
            hdr: CtrlHdr::new(CMD_RESOURCE_UNREF),
            resource_id,
            padding: 0,
        };
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `TRANSFER_TO_HOST_3D` / `TRANSFER_FROM_HOST_3D`: copy the whole of the resource's
    /// attached backing to or from the host-side texture.
    ///
    /// `stride` and `layer_stride` are left zero, which is the protocol's "derive them
    /// from the format and geometry". Both directions are answered `NODATA` whether or
    /// not the host accepted the move, so only the bytes can tell a caller which happened.
    fn transfer_3d(
        &mut self,
        to_host: bool,
        ctx_id: u32,
        resource_id: u32,
        width: u32,
        height: u32,
    ) -> Result<(), DriverError> {
        let mut cmd = TransferHost3D {
            hdr: CtrlHdr::new(if to_host {
                CMD_TRANSFER_TO_HOST_3D
            } else {
                CMD_TRANSFER_FROM_HOST_3D
            }),
            area: Box3D {
                x: 0,
                y: 0,
                z: 0,
                w: width,
                h: height,
                d: 1,
            },
            offset: 0,
            resource_id,
            level: 0,
            stride: 0,
            layer_stride: 0,
        };
        cmd.hdr.ctx_id = ctx_id;
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `SUBMIT_3D`: hand a context a virgl command buffer. The buffer travels *after* the
    /// 32-byte command in the same out descriptor — that is where the host looks for it
    /// (`iov_to_buf` from `sizeof(struct virtio_gpu_cmd_submit)`), not in a descriptor of
    /// its own — which is why this builds one buffer and sends it as one.
    fn submit_3d(&mut self, ctx_id: u32, buffer: &[u8]) -> Result<(), DriverError> {
        let mut cmd = CmdSubmit3D {
            hdr: CtrlHdr::new(CMD_SUBMIT_3D),
            size: buffer.len() as u32,
            padding: 0,
        };
        cmd.hdr.ctx_id = ctx_id;
        let mut out = [0u8; CMD_BUF_LEN];
        let head = struct_bytes(&cmd);
        if head.len() + buffer.len() > out.len() {
            return Err(DriverError::InvalidArgument);
        }
        out[..head.len()].copy_from_slice(head);
        out[head.len()..head.len() + buffer.len()].copy_from_slice(buffer);
        self.send_cmd(&out[..head.len() + buffer.len()])
    }

    /// The `SUBMIT_3D` half of the round trip: have the *host* write a pattern over one row
    /// of the texture with a single inline write, then read the texture back and report how
    /// far it matched the expectation — that row changed, the rest of the texture not.
    ///
    /// A transfer cannot stand in for this. It only ever moves bytes the guest already
    /// had, so it would agree with itself whether or not the renderer ran a command.
    fn submit_round_trip(&mut self, ctx_id: u32, resource_id: u32) -> Result<u32, DriverError> {
        let command = inline_write_command(resource_id);
        self.submit_3d(ctx_id, &command)?;
        let buf: &mut [u8; PROBE_TEX_BYTES as usize] = unsafe { &mut *PROBE_BUF.get() };
        for b in buf.iter_mut() {
            *b = 0x5A;
        }
        self.transfer_3d(false, ctx_id, resource_id, PROBE_TEX_W, PROBE_TEX_H)?;
        Ok(buf
            .iter()
            .enumerate()
            .take_while(|(i, b)| **b == expected_after_inline_write(*i))
            .count() as u32)
    }

    /// Upload a known pattern into a fresh host texture, overwrite the buffer that backs
    /// it, and read the texture back; then make the host write a second pattern with a
    /// `SUBMIT_3D` command and read *that* back. Returns how far each came back equal.
    ///
    /// The overwrite before each read-back is what makes these evidence rather than
    /// ceremony: the resource is guest-backed, so a host that ignored a transfer or a
    /// command would leave the previous contents in the buffer and a test without the
    /// overwrite would pass on a path that does nothing at all.
    ///
    /// The resource is handed back before this returns, and its fate is part of the
    /// verdict: a leaked id is refused by the next create, so a round-trip that could not
    /// clean up is reported as no round-trip rather than as a pass.
    fn round_trip(&mut self, ctx_id: u32) -> Result<(u32, u32), DriverError> {
        // 2, not the scanout resource's 1, so a device that also presented something
        // cannot have the two confused for one another.
        const RES: u32 = 2;
        let buf: &mut [u8; PROBE_TEX_BYTES as usize] = unsafe { &mut *PROBE_BUF.get() };
        // The buffer is this process's own image, so the same VA→PA delta the transport
        // derives descriptor addresses from is what the mem entry needs.
        let pa = buf.as_ptr() as u64 + virtio::virtio_phys_delta() as u64;
        for (i, b) in buf.iter_mut().enumerate() {
            *b = pattern_byte(i);
        }
        self.resource_create_3d(
            RES,
            PROBE_TEX_W,
            PROBE_TEX_H,
            VIRGL_FORMAT_B8G8R8A8_UNORM,
            VIRGL_BIND_TEXTURE,
        )?;
        self.attach_backing(RES, pa, PROBE_TEX_BYTES)?;
        self.ctx_attach_resource(ctx_id, RES)?;
        self.transfer_3d(true, ctx_id, RES, PROBE_TEX_W, PROBE_TEX_H)?;
        for b in buf.iter_mut() {
            *b = 0xA5;
        }
        self.transfer_3d(false, ctx_id, RES, PROBE_TEX_W, PROBE_TEX_H)?;
        let matched = buf
            .iter()
            .enumerate()
            .take_while(|(i, b)| **b == pattern_byte(*i))
            .count() as u32;
        // The texture now holds the pattern, which is what the inline write has to change
        // one row of and leave alone everywhere else.
        let submitted = self.submit_round_trip(ctx_id, RES)?;
        self.resource_unref(RES).map(|()| (matched, submitted))
    }

    fn screen_info(&self) -> FbVarScreeninfo {
        FbVarScreeninfo {
            xres: XRES,
            yres: YRES,
            xres_virtual: XRES,
            yres_virtual: YRES,
            xoffset: 0,
            yoffset: 0,
            bits_per_pixel: 32,
            red: FbBitfield {
                offset: 16,
                length: 8,
                msb_right: 0,
            },
            green: FbBitfield {
                offset: 8,
                length: 8,
                msb_right: 0,
            },
            blue: FbBitfield {
                offset: 0,
                length: 8,
                msb_right: 0,
            },
            transp: FbBitfield {
                offset: 24,
                length: 8,
                msb_right: 0,
            },
            reserved: [0; 10],
        }
    }
}

/// The most capsets a `virtio-gpu` host may offer before this driver stops asking. Two are
/// in use (virgl, virgl2) and the kernel allows eight; the bound exists because the host
/// answers "no more" with a zero id rather than with an error.
const MAX_CAPSETS: usize = 8;

/// A render node with its device held open — what the DRM ABI answers from (§6.10, stage
/// 3b).
///
/// It *owns* the device rather than borrowing it: a virtio device is driven by one process
/// at a time, so whoever holds this is the only thing that may talk to it, and that is
/// also why [`probe_render_node`] hands it on rather than dropping it.
pub struct DrmNode {
    dev: VirtioGpuArch,
    /// What `GET_CAPSET_INFO` said, index after index, until one answered with id 0.
    capsets: [CapsetInfo; MAX_CAPSETS],
    count: usize,
    /// The feature word, as `GETPARAM` reports it.
    blob: bool,
    context_init: bool,
}

impl DrmNode {
    /// The capset with this id, if the host offers one.
    fn capset(&self, id: u32) -> Option<CapsetInfo> {
        self.capsets[..self.count]
            .iter()
            .copied()
            .find(|c| c.id == id)
    }

    /// `VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs`: a bitmask with bit `id - 1` set per capset,
    /// which is how the host's set is reported and how Mesa chooses which to ask for.
    fn capset_mask(&self) -> u64 {
        let mut mask = 0u64;
        for c in &self.capsets[..self.count] {
            if (1..=64).contains(&c.id) {
                mask |= 1u64 << (c.id - 1);
            }
        }
        mask
    }
}

impl drm::RenderNode for DrmNode {
    fn version(&self) -> drm::DrmVersion {
        drm::DrmVersion {
            major: 1,
            minor: 0,
            patchlevel: 0,
            name: "virtio_gpu",
            date: "20260927",
            desc: "minixrs virtio-gpu render node",
        }
    }

    fn get_cap(&mut self, capability: u64) -> Result<u64, i32> {
        // A render node has neither: dumb buffers belong to the mode-setting path, and
        // PRIME is dma-buf sharing, which this port does not have yet.
        match capability {
            drm::DRM_CAP_DUMB_BUFFER | drm::DRM_CAP_PRIME => Ok(0),
            _ => Err(drm::EINVAL),
        }
    }

    fn getparam(&mut self, param: u64) -> Result<i32, i32> {
        match param {
            // A node only exists where the host offered VIRGL, so this is 1 by
            // construction rather than by asking again.
            drm::VIRTGPU_PARAM_3D_FEATURES => Ok(1),
            drm::VIRTGPU_PARAM_CAPSET_QUERY_FIX => Ok(1),
            drm::VIRTGPU_PARAM_RESOURCE_BLOB => Ok(self.blob as i32),
            // Host blobs are not mappable into the guest yet, and there is one virtio
            // device, so nothing to share across devices.
            drm::VIRTGPU_PARAM_HOST_VISIBLE | drm::VIRTGPU_PARAM_CROSS_DEVICE => Ok(0),
            drm::VIRTGPU_PARAM_CONTEXT_INIT => Ok(self.context_init as i32),
            drm::VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS => Ok(self.capset_mask() as i32),
            _ => Err(drm::EINVAL),
        }
    }

    fn get_caps(&mut self, capset_id: u32, version: u32, out: &mut [u8]) -> Result<usize, i32> {
        let Some(info) = self.capset(capset_id) else {
            return Err(drm::EINVAL);
        };
        // A version above the host's is refused here rather than passed on: the host
        // answers an over-high version with an error, and a caller asking with one has
        // plainly not read `GET_CAPSET_INFO` first.
        if version > info.max_version {
            return Err(drm::EINVAL);
        }
        self.dev
            .fetch_capset(capset_id, version, out)
            .map(|n| n as usize)
            .map_err(|_| drm::EINVAL)
    }
}

/// Negotiate the render node and take it (§6.10, stage 3b): the features, the control
/// queues, and the capsets the host offers.
///
/// `out` receives what the negotiation learned whether or not a node comes back, because
/// "there is no 3D here" is itself an answer a caller has to be able to report (D7).
fn open_render_node(out: &mut Gpu3d) -> Option<DrmNode> {
    let Ok(mut dev) = virtio::virtio_probe(VIRTIO_GPU_DEVICE_ID, "virtio-gpu-3d", GPU_FEATURES, 0)
    else {
        return None;
    };
    out.device = true;
    out.virgl = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_VIRGL);
    out.blob = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_RESOURCE_BLOB);
    out.context_init = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_CONTEXT_INIT);

    // Without VIRGL there is nothing to ask: the capset and context commands are
    // answered with errors, so the report stops here with the features it saw.
    if !out.virgl {
        return None;
    }
    let Ok(queues) = setup_queues(&mut dev) else {
        return None;
    };
    out.queues = queues;

    let mut node = DrmNode {
        dev: VirtioGpuArch::new(),
        capsets: [CapsetInfo::default(); MAX_CAPSETS],
        count: 0,
        blob: out.blob,
        context_init: out.context_init,
    };
    node.dev.vdev = Some(dev);

    // The host lists its capsets by index and answers past the last one with a zero id
    // instead of an error, so this walks until then — bounded, in case a host never does.
    while node.count < MAX_CAPSETS {
        match node.dev.capset_info(node.count as u32) {
            Ok(info) if info.id != 0 => {
                node.capsets[node.count] = info;
                node.count += 1;
            }
            Ok(_) => break,
            Err(_) => {
                // Only worth reporting when nothing was learnt: one refused query after a
                // good first one is a host saying "that is all", badly.
                if node.count == 0 {
                    out.capset_rtype = node.dev.last_response;
                }
                break;
            }
        }
    }
    if node.count == 0 {
        return None;
    }
    out.capset = node.capsets[0];
    out.capset_count = node.count as u32;
    Some(node)
}

/// Probe for a `virtio-gpu` **render node** (§6.10, stages 3a and 3b): negotiate the 3D
/// features, walk the capsets the host offers, fetch the first one's blob, create and
/// destroy a context, and round-trip a texture through it both ways — by transfer, and by
/// a submitted command. Scanout is never touched — the render device and the output are
/// separate objects (D3), so this is what runs where the display is some other device,
/// which on x86 is `bochs-display`.
///
/// Nothing here fails the caller. A host with no GL offers no `VIRTIO_GPU_F_VIRGL`,
/// and a guest without GL still has to boot (D7), so an absent or non-GL device is
/// reported, not raised. Every field of the answer is what the device said, never
/// what a host was assumed to have (D8).
///
/// The device comes *back* with the report when there is a node to hand back: the
/// negotiation happens once, here, and the caller keeps what this returns — a device is
/// driven by one holder, and the DRM ABI (§6.10, 3b) is the thing that answers from it.
pub fn probe_render_node() -> (Gpu3d, Option<DrmNode>) {
    let mut out = Gpu3d::default();
    let Some(mut node) = open_render_node(&mut out) else {
        return (out, None);
    };
    // The blob for the capset the report names: what the host renders with is only useful
    // with its contents.
    if let Ok(len) = node.dev.capset_blob(out.capset) {
        out.capset_len = len;
    }
    // Create is the evidence and destroy keeps the host from holding a context the
    // port will never use, so the flag means both halves worked.
    const PROBE_CTX_ID: u32 = 1;
    let created = node.dev.ctx_create(PROBE_CTX_ID, b"minixrs").is_ok();
    if created {
        // The bytes, not the response types: see `Gpu3d::xfer_matched` and
        // `Gpu3d::submit_matched`.
        if let Ok((xfer, submit)) = node.dev.round_trip(PROBE_CTX_ID) {
            out.xfer_matched = xfer;
            out.submit_matched = submit;
        }
    }
    out.ctx = created && node.dev.ctx_destroy(PROBE_CTX_ID).is_ok();
    (out, Some(node))
}

impl Default for VirtioGpuArch {
    fn default() -> Self {
        Self::new()
    }
}

impl FbArch for VirtioGpuArch {
    fn init(&mut self, _minor: usize) -> Result<(), DriverError> {
        if self.initialized {
            return Ok(());
        }
        if self.fb_va == 0 {
            return Err(DriverError::InvalidArgument);
        }

        let mut dev = virtio::virtio_probe(VIRTIO_GPU_DEVICE_ID, "virtio-gpu", &[], 0)
            .map_err(|_| DriverError::NotFound)?;
        let _ = setup_queues(&mut dev)?;
        self.vdev = Some(dev);

        let fb_pa = self.fb_va.wrapping_add(virtio::virtio_phys_delta() as u64);

        // RESOURCE_CREATE_2D: resource 1, B8G8R8X8, 1024×768.
        let cmd = ResourceCreate2D {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_2D),
            resource_id: RESOURCE_ID,
            format: VIRTIO_GPU_FORMAT_B8G8R8X8,
            width: XRES,
            height: YRES,
        };
        self.send_cmd(struct_bytes(&cmd))?;

        // RESOURCE_ATTACH_BACKING, with one entry covering the whole buffer
        // (page-aligned, page-multiple length).
        self.attach_backing(RESOURCE_ID, fb_pa, FB_SIZE as u32)?;

        // SET_SCANOUT: scanout 0 shows resource 1 at full size.
        let cmd = SetScanout {
            hdr: CtrlHdr::new(CMD_SET_SCANOUT),
            r: Rect {
                x: 0,
                y: 0,
                width: XRES,
                height: YRES,
            },
            scanout_id: 0,
            resource_id: RESOURCE_ID,
        };
        self.send_cmd(struct_bytes(&cmd))?;

        // Initial FLUSH so the (all-zero) buffer is displayed.
        self.flush_inner()?;

        self.dev = FbDevice {
            base: fb_pa,
            size: FB_SIZE,
        };
        self.var = self.screen_info();
        self.fix.line_length = XRES * 4;
        self.initialized = true;
        Ok(())
    }

    fn device(&self, _minor: usize) -> Result<FbDevice, DriverError> {
        if self.dev.size == 0 {
            return Err(DriverError::NotFound);
        }
        Ok(self.dev)
    }

    fn mem(&self, _minor: usize) -> Result<u64, DriverError> {
        if self.fb_va == 0 {
            return Err(DriverError::NotFound);
        }
        Ok(self.fb_va)
    }

    fn var_screeninfo(&self, _minor: usize) -> Result<FbVarScreeninfo, DriverError> {
        Ok(self.var)
    }

    fn set_var_screeninfo(
        &mut self,
        _minor: usize,
        info: &FbVarScreeninfo,
    ) -> Result<(), DriverError> {
        self.var = *info;
        Ok(())
    }

    fn fix_screeninfo(&self, _minor: usize) -> Result<FbFixScreeninfo, DriverError> {
        Ok(self.fix)
    }

    fn pan_display(&mut self, _minor: usize, info: &FbVarScreeninfo) -> Result<(), DriverError> {
        self.var.xoffset = info.xoffset;
        self.var.yoffset = info.yoffset;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.flush_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The node's answers are its `drm::RenderNode` methods, so the trait has to be in
    // scope for the tests to call them; `as _` keeps it from shadowing anything.
    use crate::video::drm::RenderNode as _;

    #[test]
    fn command_struct_sizes_match_spec() {
        // Sizes follow `linux/virtio_gpu.h`: the 24-byte ctrl header has no
        // resource_id (ring_idx + padding[3] instead), so every command's
        // body fields sit 8 bytes earlier than a 32-byte header would put
        // them.
        assert_eq!(core::mem::size_of::<CtrlHdr>(), 24);
        assert_eq!(core::mem::size_of::<ResourceCreate2D>(), 40);
        assert_eq!(core::mem::size_of::<MemEntry>(), 16);
        assert_eq!(core::mem::size_of::<AttachBacking>(), 48);
        assert_eq!(core::mem::size_of::<Rect>(), 16);
        assert_eq!(core::mem::size_of::<SetScanout>(), 48);
        assert_eq!(core::mem::size_of::<TransferToHost2D>(), 56);
        assert_eq!(core::mem::size_of::<ResourceFlush>(), 48);
        assert_eq!(core::mem::size_of::<ResourceCreate3D>(), 72);
        assert_eq!(core::mem::size_of::<Box3D>(), 24);
        assert_eq!(core::mem::size_of::<TransferHost3D>(), 72);
        assert_eq!(core::mem::size_of::<ResourceRef>(), 32);
        assert_eq!(core::mem::size_of::<CmdSubmit3D>(), 32);
    }

    #[test]
    fn protocol_constants() {
        assert_eq!(VIRTIO_GPU_DEVICE_ID, 0x0010);
        assert_eq!(VIRTIO_GPU_FORMAT_B8G8R8X8, 2);
        assert_eq!(CMD_RESOURCE_CREATE_2D, 0x0101);
        assert_eq!(CMD_RESOURCE_ATTACH_BACKING, 0x0106);
        assert_eq!(CMD_SET_SCANOUT, 0x0103);
        assert_eq!(CMD_RESOURCE_FLUSH, 0x0104);
        assert_eq!(CMD_TRANSFER_TO_HOST_2D, 0x0105);
        assert_eq!(CMD_RESOURCE_UNREF, 0x0102);
        assert_eq!(CMD_CTX_ATTACH_RESOURCE, 0x0202);
        assert_eq!(CMD_RESOURCE_CREATE_3D, 0x0204);
        assert_eq!(CMD_TRANSFER_TO_HOST_3D, 0x0205);
        assert_eq!(CMD_TRANSFER_FROM_HOST_3D, 0x0206);
        assert_eq!(CMD_SUBMIT_3D, 0x0207);
        assert_eq!(RESP_OK_NODATA, 0x1100);
        assert_eq!(FB_SIZE, 1024 * 768 * 4);
    }

    #[test]
    fn create_2d_command_bytes() {
        let cmd = ResourceCreate2D {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_2D),
            resource_id: 1,
            format: VIRTIO_GPU_FORMAT_B8G8R8X8,
            width: 1024,
            height: 768,
        };
        let bytes = struct_bytes(&cmd);
        // hdr (24 bytes), then resource_id @24, format @28, width @32,
        // height @36.
        assert_eq!(
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            CMD_RESOURCE_CREATE_2D
        );
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 0); // hdr ring_idx+padding
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            VIRTIO_GPU_FORMAT_B8G8R8X8
        );
        assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 1024);
        assert_eq!(u32::from_le_bytes(bytes[36..40].try_into().unwrap()), 768);
    }

    #[test]
    fn attach_backing_carries_resource_in_body_and_translates_pa() {
        let cmd = AttachBacking {
            hdr: CtrlHdr::new(CMD_RESOURCE_ATTACH_BACKING),
            resource_id: 7,
            nr_entries: 1,
            entries: [MemEntry {
                addr: 0x1234_5000,
                length: 0x300000,
                padding: 0,
            }],
        };
        let bytes = struct_bytes(&cmd);
        // hdr @0..24 (no resource ref), body resource_id @24, nr_entries
        // @28, entry addr @32, length @40.
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), 1);
        assert_eq!(
            u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            0x1234_5000
        );
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            0x0030_0000
        );
    }

    #[test]
    fn set_scanout_and_flush_carry_resource_in_body() {
        let ss = SetScanout {
            hdr: CtrlHdr::new(CMD_SET_SCANOUT),
            r: Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            },
            scanout_id: 0,
            resource_id: 1,
        };
        let b = struct_bytes(&ss);
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_SET_SCANOUT
        );
        assert_eq!(u32::from_le_bytes(b[20..24].try_into().unwrap()), 0); // hdr ring_idx+padding
        // rect @24..40, scanout_id @40, resource_id @44.
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 1);

        let fl = ResourceFlush {
            hdr: CtrlHdr::new(CMD_RESOURCE_FLUSH),
            r: Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            },
            resource_id: 1,
            padding: 0,
        };
        let b = struct_bytes(&fl);
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_RESOURCE_FLUSH
        );
        // rect @24..40, resource_id @40, padding @44.
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 0); // r.x
        assert_eq!(u32::from_le_bytes(b[32..36].try_into().unwrap()), 1024); // r.width
        assert_eq!(u32::from_le_bytes(b[36..40].try_into().unwrap()), 768); // r.height
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 1); // resource_id
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 0);
    }

    #[test]
    fn transfer_to_host_2d_layout() {
        let t = TransferToHost2D {
            hdr: CtrlHdr::new(CMD_TRANSFER_TO_HOST_2D),
            r: Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            },
            offset: 0,
            resource_id: 1,
            padding: 0,
        };
        let b = struct_bytes(&t);
        // rect @24..40, offset @40..48, resource_id @48, padding @52.
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_TRANSFER_TO_HOST_2D
        );
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(b[32..36].try_into().unwrap()), 1024);
        assert_eq!(u32::from_le_bytes(b[36..40].try_into().unwrap()), 768);
        assert_eq!(u64::from_le_bytes(b[40..48].try_into().unwrap()), 0); // offset
        assert_eq!(u32::from_le_bytes(b[48..52].try_into().unwrap()), 1); // resource_id
        assert_eq!(u32::from_le_bytes(b[52..56].try_into().unwrap()), 0);
    }

    #[test]
    fn screen_info_is_1024x768x32() {
        let arch = VirtioGpuArch::new();
        let var = arch.screen_info();
        assert_eq!(var.xres, 1024);
        assert_eq!(var.yres, 768);
        assert_eq!(var.bits_per_pixel, 32);
        assert_eq!(var.red.offset, 16);
        assert_eq!(var.blue.offset, 0);
    }

    #[test]
    fn device_and_mem_require_init_state() {
        let mut arch = VirtioGpuArch::new();
        assert!(arch.device(0).is_err()); // dev.size == 0
        assert!(arch.mem(0).is_err()); // fb_va == 0
        // init refuses to probe without a framebuffer VA (checked before
        // the virtio probe, so this is safe on the host).
        assert!(arch.init(0).is_err());
        arch.set_fb_va(0x1000);
        assert_eq!(arch.mem(0).unwrap(), 0x1000);
        // The probe itself needs real virtio hardware — target-tested.
    }

    #[test]
    fn get_capset_info_layout_is_32_bytes() {
        let cmd = GetCapsetInfo {
            hdr: CtrlHdr::new(CMD_GET_CAPSET_INFO),
            capset_index: 1,
            padding: 0,
        };
        let b = struct_bytes(&cmd);
        assert_eq!(b.len(), 32);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x0108);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 1);
    }

    #[test]
    fn get_capset_layout_carries_id_and_version() {
        let cmd = GetCapset {
            hdr: CtrlHdr::new(CMD_GET_CAPSET),
            capset_id: 1,
            capset_version: 2,
        };
        let b = struct_bytes(&cmd);
        assert_eq!(b.len(), 32);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x0109);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(b[28..32].try_into().unwrap()), 2);
    }

    /// `CTX_CREATE` is 96 bytes with the context id in the *header* (the driver picks
    /// it) and the name length counting only what was copied into the fixed field.
    #[test]
    fn ctx_create_carries_its_context_id_and_name_length() {
        let mut cmd = CtxCreate {
            hdr: CtrlHdr::new(CMD_CTX_CREATE),
            nlen: 0,
            context_init: 0,
            debug_name: [0u8; 64],
        };
        cmd.hdr.ctx_id = 7;
        let name = b"minixrs";
        cmd.debug_name[..name.len()].copy_from_slice(name);
        cmd.nlen = name.len() as u32;

        let b = struct_bytes(&cmd);
        assert_eq!(b.len(), 96);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x0200);
        assert_eq!(u32::from_le_bytes(b[16..20].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(b[28..32].try_into().unwrap()), 0);
        assert_eq!(&b[32..39], name);
    }

    #[test]
    fn ctx_destroy_layout_is_32_bytes() {
        let mut cmd = CtxDestroy {
            hdr: CtrlHdr::new(CMD_CTX_DESTROY),
            padding: 0,
        };
        cmd.hdr.ctx_id = 3;
        let b = struct_bytes(&cmd);
        assert_eq!(b.len(), 32);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x0201);
        assert_eq!(u32::from_le_bytes(b[16..20].try_into().unwrap()), 3);
    }

    /// The response offsets are what `GET_CAPSET_INFO` is read back through, so they
    /// are pinned to the struct the host writes rather than recomputed by hand.
    #[test]
    fn resp_capset_info_offsets_match_the_struct() {
        assert_eq!(RESP_CAPSET_INFO_LEN, 40);
        assert_eq!(
            (
                RESP_CAPSET_INFO_ID,
                RESP_CAPSET_INFO_VERSION,
                RESP_CAPSET_INFO_SIZE
            ),
            (24, 28, 32)
        );
        assert!(CAPSET_BUF_LEN as u32 >= RESP_CAPSET_INFO_LEN);
    }

    /// A 3D error response maps to an error a probe can report, never a wait, and
    /// the success block keeps its published numbering: the members are consecutive,
    /// so a missing one (here `RESP_OK_DISPLAY_INFO`) silently shifts every later one
    /// and a correct answer starts looking like a failure.
    #[test]
    fn a_3d_error_response_maps_to_an_error_not_a_wait() {
        assert_eq!(RESP_OK_NODATA, 0x1100);
        assert_eq!(RESP_OK_DISPLAY_INFO, 0x1101);
        assert_eq!(RESP_OK_CAPSET_INFO, 0x1102);
        assert_eq!(RESP_OK_CAPSET, 0x1103);
        assert_eq!(resp_error(RESP_ERR_OUT_OF_MEMORY), DriverError::Busy);
        assert_eq!(
            resp_error(RESP_ERR_INVALID_RESOURCE_ID),
            DriverError::InvalidArgument
        );
        assert_eq!(resp_error(0x1200), DriverError::Io);
    }

    /// No render node is all-false and zero: the report a machine with no GPU, or a
    /// host with no GL, is supposed to produce (D7/D8).
    #[test]
    fn an_absent_render_node_reports_nothing() {
        let g = Gpu3d::default();
        assert!(!g.device && !g.virgl && !g.blob && !g.context_init && !g.ctx);
        assert_eq!(g.capset, CapsetInfo::default());
        assert_eq!(g.capset_count, 0);
        assert_eq!(g.capset_len, 0);
        assert_eq!(g.xfer_matched, 0);
        assert_eq!(g.submit_matched, 0);
    }

    /// `RESOURCE_CREATE_3D` is 72 bytes, and its fields land where the host reads them.
    /// Its `format` is a *virgl* format, unlike the 2D create's, whose format is one of
    /// the `VIRTIO_GPU_FORMAT_*` values.
    #[test]
    fn resource_create_3d_layout_is_72_bytes() {
        let cmd = ResourceCreate3D {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_3D),
            resource_id: 2,
            target: PIPE_TEXTURE_2D,
            format: VIRGL_FORMAT_B8G8R8A8_UNORM,
            bind: VIRGL_BIND_TEXTURE,
            width: PROBE_TEX_W,
            height: PROBE_TEX_H,
            depth: 1,
            array_size: 1,
            last_level: 0,
            nr_samples: 0,
            flags: 0,
            padding: 0,
        };
        let b = struct_bytes(&cmd);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x0204);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(b[28..32].try_into().unwrap()),
            PIPE_TEXTURE_2D
        );
        assert_eq!(
            u32::from_le_bytes(b[32..36].try_into().unwrap()),
            VIRGL_FORMAT_B8G8R8A8_UNORM
        );
        assert_eq!(
            u32::from_le_bytes(b[36..40].try_into().unwrap()),
            VIRGL_BIND_TEXTURE
        );
        assert_eq!(
            u32::from_le_bytes(b[40..44].try_into().unwrap()),
            PROBE_TEX_W
        );
        assert_eq!(
            u32::from_le_bytes(b[44..48].try_into().unwrap()),
            PROBE_TEX_H
        );
    }

    /// The transfer's box is at 24 and its resource body at 56 — the offsets the round
    /// trip rides on, and the ones a stray field in the middle would silently move.
    #[test]
    fn transfer_host_3d_layout_is_72_bytes() {
        let mut cmd = TransferHost3D {
            hdr: CtrlHdr::new(CMD_TRANSFER_FROM_HOST_3D),
            area: Box3D {
                x: 0,
                y: 0,
                z: 0,
                w: PROBE_TEX_W,
                h: PROBE_TEX_H,
                d: 1,
            },
            offset: 0,
            resource_id: 2,
            level: 0,
            stride: 0,
            layer_stride: 0,
        };
        cmd.hdr.ctx_id = 1;
        let b = struct_bytes(&cmd);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x0206);
        assert_eq!(u32::from_le_bytes(b[16..20].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 0);
        assert_eq!(
            u32::from_le_bytes(b[36..40].try_into().unwrap()),
            PROBE_TEX_W
        );
        assert_eq!(
            u32::from_le_bytes(b[40..44].try_into().unwrap()),
            PROBE_TEX_H
        );
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(b[48..56].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(b[56..60].try_into().unwrap()), 2);
    }

    /// `CTX_ATTACH_RESOURCE` and `RESOURCE_UNREF` share a 32-byte body: the resource at 24
    /// and the context, where there is one, in the header.
    #[test]
    fn resource_ref_layout_is_32_bytes() {
        let mut attach = ResourceRef {
            hdr: CtrlHdr::new(CMD_CTX_ATTACH_RESOURCE),
            resource_id: 2,
            padding: 0,
        };
        attach.hdr.ctx_id = 1;
        let b = struct_bytes(&attach);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x0202);
        assert_eq!(u32::from_le_bytes(b[16..20].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 2);

        let unref = ResourceRef {
            hdr: CtrlHdr::new(CMD_RESOURCE_UNREF),
            resource_id: 2,
            padding: 0,
        };
        let b = struct_bytes(&unref);
        assert_eq!(b.len(), 32);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x0102);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 2);
    }

    /// The round-trip buffer has to be one `ATTACH_BACKING` range, which the host maps as
    /// a whole: a page-aligned address and a page-multiple length are what let a single
    /// mem entry cover it.
    #[test]
    fn the_round_trip_buffer_is_a_whole_number_of_pages() {
        assert_eq!(PROBE_TEX_BYTES % 4096, 0);
        let buf: &[u8; PROBE_TEX_BYTES as usize] = unsafe { &*PROBE_BUF.get() };
        assert_eq!(buf.as_ptr() as usize % 4096, 0);
        assert_eq!(buf.len() as u32, PROBE_TEX_BYTES);
    }

    /// The pattern depends on position, and differs between the four bytes of a pixel, so
    /// a transfer that reordered the channels — or shifted the data by a row — comes back
    /// as a short match rather than as a pass.
    ///
    /// Its *first* byte must not be the value the buffer is scribbled with, because the
    /// count is a leading run: an ignored transfer leaves the buffer scribbled, and if that
    /// value happened to match at offset 0 the report would read as a round trip that got
    /// cut short rather than as one that never happened. (Further in, the value is in the
    /// pattern like every other byte is — 16 KiB covers all 256 of them many times.)
    #[test]
    fn the_round_trip_pattern_is_positional() {
        assert_ne!(pattern_byte(0), 0xA5);
        assert_ne!(host_pattern_byte(0), 0x5A);
        for i in 0..PROBE_TEX_BYTES as usize {
            assert_ne!(pattern_byte(i), pattern_byte(i + 1));
            assert_ne!(host_pattern_byte(i), pattern_byte(i));
        }
        assert_ne!(pattern_byte(0), pattern_byte(2));
        assert_ne!(pattern_byte(1), pattern_byte(3));
    }

    /// The `SUBMIT_3D` command buffer: one `RESOURCE_INLINE_WRITE` whose header carries
    /// its length in dwords *after* it, whose fields sit where the layout every transfer
    /// command also uses puts them, and whose data starts at dword 12 — the offsets the
    /// host reads to work out what to write and where.
    #[test]
    fn inline_write_command_layout() {
        let cmd = inline_write_command(2);
        assert_eq!(cmd.len(), IW_CMD_LEN);
        let w = |at: usize| u32::from_le_bytes(cmd[at..at + 4].try_into().unwrap());
        let header = w(0);
        assert_eq!(header & 0xff, VIRGL_CCMD_RESOURCE_INLINE_WRITE);
        assert_eq!((header >> 8) & 0xff, VIRGL_OBJECT_NULL);
        // The length is what the host recomputes the data size from: (len - 11) * 4.
        assert_eq!(header >> 16, 11 + PROBE_IW_PIXELS);
        assert_eq!(w(4), 2);
        assert_eq!(w(16), PROBE_IW_STRIDE);
        assert_eq!(w(20), PROBE_IW_LAYER_STRIDE);
        assert_eq!(w(36), PROBE_IW_PIXELS);
        assert_eq!(w(40), 1);
        assert_eq!(w(44), 1);
        for i in 0..PROBE_IW_BYTES as usize {
            assert_eq!(cmd[48 + i], host_pattern_byte(i));
        }
    }

    /// The submit command — its 32-byte header and the whole command buffer after it in
    /// the same descriptor — has to fit the slot every command is copied through, or
    /// `submit_3d` refuses it rather than truncating it.
    #[test]
    fn the_submit_command_fits_the_command_slot() {
        assert!(core::mem::size_of::<CmdSubmit3D>() + IW_CMD_LEN <= CMD_BUF_LEN);
    }

    /// The expectation after the inline write switches exactly at the region the command
    /// named, which is what makes the read-back reject a write that spilled past its box
    /// as well as one that never happened.
    #[test]
    fn the_inline_write_expectation_switches_at_the_named_region() {
        let last = PROBE_IW_BYTES as usize - 1;
        assert_eq!(expected_after_inline_write(0), host_pattern_byte(0));
        assert_eq!(expected_after_inline_write(last), host_pattern_byte(last));
        assert_eq!(
            expected_after_inline_write(PROBE_IW_BYTES as usize),
            pattern_byte(PROBE_IW_BYTES as usize)
        );
    }

    /// A node with a fabricated capset list. The ABI answers that need no device are
    /// exactly the ones worth pinning — they are what a client reads to decide the device
    /// is usable — and this is how they are reached without one.
    fn drm_node(capsets: &[CapsetInfo], blob: bool, context_init: bool) -> DrmNode {
        let mut node = DrmNode {
            dev: VirtioGpuArch::new(),
            capsets: [CapsetInfo::default(); MAX_CAPSETS],
            count: capsets.len(),
            blob,
            context_init,
        };
        node.capsets[..capsets.len()].copy_from_slice(capsets);
        node
    }

    /// The capset mask is `bit id - 1` per capset, which is how the host's set is reported.
    /// `3D_FEATURES` is 1 by construction: a node only exists where the host offered VIRGL.
    #[test]
    fn getparam_reports_the_features_and_the_capset_mask() {
        let virgl = CapsetInfo {
            id: 1,
            max_version: 1,
            max_size: 308,
        };
        let virgl2 = CapsetInfo {
            id: 2,
            max_version: 2,
            max_size: 400,
        };
        let mut node = drm_node(&[virgl], false, true);
        assert_eq!(node.getparam(drm::VIRTGPU_PARAM_3D_FEATURES), Ok(1));
        assert_eq!(node.getparam(drm::VIRTGPU_PARAM_CAPSET_QUERY_FIX), Ok(1));
        assert_eq!(node.getparam(drm::VIRTGPU_PARAM_RESOURCE_BLOB), Ok(0));
        assert_eq!(node.getparam(drm::VIRTGPU_PARAM_HOST_VISIBLE), Ok(0));
        assert_eq!(node.getparam(drm::VIRTGPU_PARAM_CONTEXT_INIT), Ok(1));
        assert_eq!(
            node.getparam(drm::VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS),
            Ok(0b1)
        );
        // A parameter the host does not implement is an error rather than a zero: a caller
        // that reads 0 as "not capable" would otherwise take a refusal for an answer.
        assert_eq!(node.getparam(0x99), Err(drm::EINVAL));

        // Both capsets, and the second id in the second bit.
        let mut both = drm_node(&[virgl, virgl2], true, false);
        assert_eq!(
            both.getparam(drm::VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS),
            Ok(0b11)
        );
        assert_eq!(both.getparam(drm::VIRTGPU_PARAM_RESOURCE_BLOB), Ok(1));
        assert_eq!(both.getparam(drm::VIRTGPU_PARAM_CONTEXT_INIT), Ok(0));

        let third = CapsetInfo {
            id: 3,
            max_version: 1,
            max_size: 1,
        };
        let mut three = drm_node(&[virgl, virgl2, third], false, false);
        assert_eq!(
            three.getparam(drm::VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS),
            Ok(0b111)
        );
    }

    /// A render node has neither dumb buffers nor PRIME, and anything else is refused
    /// rather than answered 0, so a caller can tell "not capable" from "not asked about".
    #[test]
    fn get_cap_answers_only_the_two_it_knows() {
        let mut node = drm_node(&[], false, false);
        assert_eq!(node.get_cap(drm::DRM_CAP_DUMB_BUFFER), Ok(0));
        assert_eq!(node.get_cap(drm::DRM_CAP_PRIME), Ok(0));
        assert_eq!(node.get_cap(0x99), Err(drm::EINVAL));
    }

    /// The two refusals `GET_CAPS` makes before it would touch the device: a capset the
    /// host does not have, and a version above the one it does. Both are `EINVAL`, which is
    /// what the host answers a query it cannot serve, so a caller sees one behaviour
    /// whichever side refuses.
    #[test]
    fn get_caps_refuses_an_unknown_capset_or_version() {
        let mut node = drm_node(
            &[CapsetInfo {
                id: 1,
                max_version: 1,
                max_size: 308,
            }],
            false,
            false,
        );
        let mut out = [0u8; 64];
        assert_eq!(node.get_caps(2, 1, &mut out), Err(drm::EINVAL));
        assert_eq!(node.get_caps(1, 2, &mut out), Err(drm::EINVAL));
    }

    /// The version string is the kernel driver's name — what a client logs, and what some
    /// of them match on — and the request's protocol is what decides how much of it a
    /// caller gets.
    #[test]
    fn the_version_names_the_driver() {
        let node = drm_node(&[], false, false);
        let v = node.version();
        assert_eq!(v.name, "virtio_gpu");
        assert!(!v.date.is_empty() && !v.desc.is_empty());
    }
}
