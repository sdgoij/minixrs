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
    FbVarScreeninfo, ScanRange,
};
use core::cell::UnsafeCell;

/// Virtio device ID for the GPU (`.refs/minix-3.3.0` virtio spec).
pub const VIRTIO_GPU_DEVICE_ID: u16 = 0x0010;

/// The PCI vendor every virtio device presents: Red Hat / virtio.
const VIRTIO_PCI_VENDOR_ID: u16 = 0x1af4;
/// The modern `virtio-gpu-pci` PCI device id, `0x1040 + VIRTIO_GPU_DEVICE_ID`. Mesa's
/// `include/pci_ids/virtio_gpu_pci_ids.h` lists this and the legacy `0x0010`, and names the
/// `virtio_gpu` DRI driver for either, so this is the pair its loader needs to see.
const VIRTIO_GPU_PCI_DEVICE_ID: u16 = 0x1050;

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
/// `RESOURCE_CREATE_BLOB`. Unlike every other create, this one carries its own backing: the
/// memory entries follow the command body, so a blob is never followed by an
/// `ATTACH_BACKING` the way a 2D resource is.
const CMD_RESOURCE_CREATE_BLOB: u32 = 0x010c;
/// `SET_SCANOUT_BLOB` — the scanout command for a scanout that is a host blob. A GL
/// device has no classic scanout: it never turns a 2D resource into an EGL image, so
/// `SET_SCANOUT` is refused there and this is the only command that works.
const CMD_SET_SCANOUT_BLOB: u32 = 0x010d;
/// `RESOURCE_MAP_BLOB` — map a host blob into the guest's shared-memory window. It is a
/// member of the 3D command block (0x0208), not of the 2D one.
const CMD_RESOURCE_MAP_BLOB: u32 = 0x0208;
/// `VIRTIO_GPU_RESP_OK_MAP_INFO` — the answer to `RESOURCE_MAP_BLOB`.
const RESP_OK_MAP_INFO: u32 = 0x1106;

/// `VIRTIO_GPU_BLOB_MEM_HOST3D` and `VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE`: a blob whose
/// memory is the host's, in the window the guest maps. A scanout blob has to be this
/// kind — a guest blob names guest pages, which the host has no descriptor to scan out.
const BLOB_MEM_HOST3D: u32 = 0x0002;
const BLOB_FLAG_USE_MAPPABLE: u32 = 0x0001;

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

/// Largest command this driver sends.
///
/// `CTX_CREATE` is 96 bytes, but a `SUBMIT_3D` carries a whole virgl command buffer after its
/// 32-byte header, and Mesa submits shader/state setup of a few KiB for even a first triangle. A
/// slot smaller than the buffer the host is handed makes `submit` answer `EINVAL` and the render
/// silently produce nothing, so the slot is sized for that payload, not for the control commands.
const CMD_BUF_LEN: usize = 16384;

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

/// The one context id this driver uses.
///
/// The node serves a single context (§6.10 D3), and the boot probe uses the same id for its
/// transient one — it destroys what it made, so the id is free by the time a client asks for it.
const CONTEXT_ID: u32 = 1;

/// The context the display's scanout blob is made in.
///
/// A host blob is a *context* object — `virgl_renderer_resource_create_blob` looks the context
/// up and refuses the blob without one — so the display needs a context of its own. It is not
/// [`CONTEXT_ID`], which the render node creates for whatever client asks it for; the two
/// coexist on the one device, as they do under Linux's `virtio_gpu`.
const BLOB_CONTEXT_ID: u32 = 2;

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

/// The resource a scanout of a *client's* buffer is made with, when the output is re-pointed at
/// guest memory this process does not own ([`FbArch::set_scanout_source`]). A virtio-gpu resource
/// takes its backing once, so a new source replaces this one rather than re-attaching.
const EXT_SCANOUT_RESOURCE_ID: u32 = 2;

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

/// `struct virtio_gpu_resource_create_blob` with one memory entry — 72 bytes: the 40-byte body
/// (hdr, resource_id, blob_mem, blob_flags, nr_entries, blob_id, size) and then the entries,
/// which is the one way this command differs from a create that is followed by its backing.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ResourceCreateBlob {
    hdr: CtrlHdr,
    resource_id: u32,
    blob_mem: u32,
    blob_flags: u32,
    nr_entries: u32,
    blob_id: u64,
    size: u64,
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

/// `struct virtio_gpu_set_scanout_blob` — 96 bytes: the header, the rect and the scanout
/// and resource ids, then the resource's format and geometry and the strides and offsets of
/// its four planes. A host blob is one linear plane, so only the first stride and offset
/// carry anything.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct SetScanoutBlob {
    hdr: CtrlHdr,
    r: Rect,
    scanout_id: u32,
    resource_id: u32,
    width: u32,
    height: u32,
    format: u32,
    padding: u32,
    strides: [u32; 4],
    offsets: [u32; 4],
}

/// `struct virtio_gpu_resource_map_blob` — 40 bytes. `offset` is where in the device's
/// shared-memory window the guest wants the blob mapped; the host answers with the cache
/// kind for that mapping rather than with an address.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ResourceMapBlob {
    hdr: CtrlHdr,
    resource_id: u32,
    padding: u32,
    offset: u64,
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
    /// The resource scanout 0 currently shows: the driver's own buffer, or the client's after
    /// [`VirtioGpuArch::set_scanout_source`] re-pointed it.
    scanout_resource: u32,
    /// Whether `EXT_SCANOUT_RESOURCE_ID` exists and is backed. A resource is created with its
    /// backing, so a new source replaces it instead of re-attaching.
    ext_resource: bool,
    /// Address of the host blob scanout 0 shows, in this process's shared-memory window, or 0
    /// when the scanout is a classic resource. The window is identity-mapped, so this is both
    /// the guest-physical and the virtual address of the blob's first byte.
    blob_va: u64,
    /// Whether the scanout is a host blob rather than a classic resource — the only scanout a
    /// GL device offers. Decided in `init` from what the device carries.
    scanout_blob: bool,
    /// The `skip` of the device `init` chose as the render *node*, when it is not the output
    /// device (§6.10 3d-b). `None` means the output device is also the node — D11 when it has
    /// GL, D7 when no device does — and then the node borrows it rather than probing again.
    node_skip: Option<u16>,
}

/// Build `RESOURCE_ATTACH_BACKING` with one entry per range into `out`, returning its length, or
/// `None` when there are no ranges or they do not fit. Free-standing so the layout is
/// host-testable, and because the entry count varies with the buffer.
fn attach_backing_bytes(resource_id: u32, ranges: &[ScanRange], out: &mut [u8]) -> Option<usize> {
    let need = 32 + ranges.len() * 16;
    if ranges.is_empty() || need > out.len() {
        return None;
    }
    out[..need].fill(0);
    out[0..4].copy_from_slice(&CMD_RESOURCE_ATTACH_BACKING.to_le_bytes());
    out[24..28].copy_from_slice(&resource_id.to_le_bytes());
    out[28..32].copy_from_slice(&(ranges.len() as u32).to_le_bytes());
    for (i, r) in ranges.iter().enumerate() {
        let o = 32 + i * 16;
        out[o..o + 8].copy_from_slice(&r.pa.to_le_bytes());
        out[o + 8..o + 12].copy_from_slice(&r.len.to_le_bytes());
    }
    Some(need)
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
            scanout_resource: RESOURCE_ID,
            ext_resource: false,
            blob_va: 0,
            scanout_blob: false,
            node_skip: None,
        }
    }

    /// Set the image VA of the server-owned framebuffer buffer.
    pub fn set_fb_va(&mut self, va: u64) {
        self.fb_va = va;
    }

    /// Whether the display came up on a host blob rather than a classic resource — the only
    /// scanout a GL device offers. Reported in the boot line so a gate can tell the two apart.
    pub fn scanout_is_blob(&self) -> bool {
        self.scanout_blob
    }

    /// The `skip` of the device `init` chose as the render *node*, when it is not the output
    /// device (§6.10 3d-b). `None` means the output device is also the node — D11 when it has GL,
    /// D7 when no device does — and the node borrows that device rather than probing a second one.
    pub fn node_skip(&self) -> Option<u16> {
        self.node_skip
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
        self.submit_slot(cmd.len())
    }

    /// Submit the first `len` bytes of the command slot as they already stand — for a command
    /// whose body is built in place because its size varies, an `ATTACH_BACKING` entry list.
    fn submit_slot(&mut self, len: usize) -> Result<(u32, u32), DriverError> {
        let cmd_slot: &mut [u8; CMD_BUF_LEN] = unsafe { &mut *GPU_CMD.get() };
        if len > cmd_slot.len() {
            return Err(DriverError::InvalidArgument);
        }
        let cmd_addr = cmd_slot.as_ptr() as u64;
        let resp_addr = GPU_RESP.get() as u64;
        let dev = self.vdev.as_mut().ok_or(DriverError::NotFound)?;
        let bufs = [
            VirtioPhysBuf {
                addr: cmd_addr,
                size: len as u32,
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

    /// Submit the command slot, built in place by a free function, and require `NODATA`.
    fn send_cmd_slot(&mut self, len: usize) -> Result<(), DriverError> {
        match self.submit_slot(len)? {
            (RESP_OK_NODATA, _) => Ok(()),
            (rtype, _) => Err(resp_error(rtype)),
        }
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

    /// `CTX_CREATE`: make a rendering context — the thing the host runs command buffers for.
    /// The context id is the driver's to choose and travels in the header, which is why the
    /// probe can pick one and hand it straight back.
    ///
    /// `flags` is the DRM context request's `context_init` word — on Linux the capset id in its
    /// low bits — which is how the host learns which capset the context renders with. Zero is a
    /// context with no capset, which the host accepts and which renders nothing.
    fn ctx_create(&mut self, ctx_id: u32, flags: u32, name: &[u8]) -> Result<(), DriverError> {
        let mut cmd = CtxCreate {
            hdr: CtrlHdr::new(CMD_CTX_CREATE),
            nlen: 0,
            context_init: flags,
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

    /// `RESOURCE_ATTACH_BACKING` with one entry per range. The entry count is the buffer's page
    /// count — a 1024×768 frame is 768 — so the body is built in the command slot rather than in
    /// a fixed-size struct: at 16 bytes an entry it reaches ~1000 within the slot's 16 KiB.
    fn attach_backing_ranges(
        &mut self,
        resource_id: u32,
        ranges: &[ScanRange],
    ) -> Result<(), DriverError> {
        let slot: &mut [u8; CMD_BUF_LEN] = unsafe { &mut *GPU_CMD.get() };
        let need = attach_backing_bytes(resource_id, ranges, &mut slot[..])
            .ok_or(DriverError::InvalidArgument)?;
        self.send_cmd_slot(need)
    }

    /// `SET_SCANOUT` at scanout 0, pointing it at `resource_id`.
    fn set_scanout_resource(&mut self, resource_id: u32) -> Result<(), DriverError> {
        let cmd = SetScanout {
            hdr: CtrlHdr::new(CMD_SET_SCANOUT),
            r: Rect {
                x: 0,
                y: 0,
                width: XRES,
                height: YRES,
            },
            scanout_id: 0,
            resource_id,
        };
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `RESOURCE_FLUSH` of `resource_id` alone: a resource whose contents *are* guest memory needs
    /// no transfer, only a nudge for the host to re-read the scanout from it.
    fn flush_resource(&mut self, resource_id: u32) -> Result<(), DriverError> {
        let cmd = ResourceFlush {
            hdr: CtrlHdr::new(CMD_RESOURCE_FLUSH),
            r: Rect {
                x: 0,
                y: 0,
                width: XRES,
                height: YRES,
            },
            resource_id,
            padding: 0,
        };
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `RESOURCE_CREATE_BLOB` for a blob whose memory is the guest's: one command carrying the
    /// object, the kind of memory it names and the pages behind it. The entries ride *inside*
    /// this command rather than in an `ATTACH_BACKING` after it, which is the whole difference
    /// between creating a blob and creating a resource and then giving it a backing.
    ///
    /// The blob's length and its backing are the same number here, because this node's objects are
    /// their slot: the host wants a backing at least as large as the blob, and equal is the only
    /// length a slot can honestly claim. `blob_id` is zero because a guest blob names no other
    /// object — the two fields that could say otherwise are refused before this is reached.
    fn resource_create_blob_guest(
        &mut self,
        resource_id: u32,
        blob_mem: u32,
        blob_flags: u32,
        pa: u64,
        len: u32,
    ) -> Result<(), DriverError> {
        let cmd = ResourceCreateBlob {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_BLOB),
            resource_id,
            blob_mem,
            blob_flags,
            nr_entries: 1,
            blob_id: 0,
            size: len as u64,
            entries: [MemEntry {
                addr: pa,
                length: len,
                padding: 0,
            }],
        };
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `RESOURCE_CREATE_BLOB` for a **host** blob: the host allocates the memory and names it
    /// with a descriptor. No entries ride in the command — a host blob's memory is the host's,
    /// not the guest's, which is the whole difference from
    /// [`VirtioGpuArch::resource_create_blob_guest`] — and the object belongs to a *context*, so
    /// the header names the one it is made in.
    fn resource_create_blob_host(&mut self, resource_id: u32, len: u64) -> Result<(), DriverError> {
        let mut cmd = ResourceCreateBlob {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_BLOB),
            resource_id,
            blob_mem: BLOB_MEM_HOST3D,
            blob_flags: BLOB_FLAG_USE_MAPPABLE,
            nr_entries: 0,
            blob_id: 0,
            size: len,
            entries: [MemEntry {
                addr: 0,
                length: 0,
                padding: 0,
            }],
        };
        cmd.hdr.ctx_id = BLOB_CONTEXT_ID;
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `RESOURCE_MAP_BLOB`: ask the host to put `resource_id`'s memory at `offset` in the
    /// device's shared-memory window. The host answers `MAP_INFO`; the mapping's *address* is
    /// the window's base plus this offset, which is what the caller already holds, so only the
    /// success is read here.
    fn resource_map_blob(&mut self, resource_id: u32, offset: u64) -> Result<(), DriverError> {
        let mut cmd = ResourceMapBlob {
            hdr: CtrlHdr::new(CMD_RESOURCE_MAP_BLOB),
            resource_id,
            padding: 0,
            offset,
        };
        cmd.hdr.ctx_id = BLOB_CONTEXT_ID;
        self.send_cmd_expect(struct_bytes(&cmd), RESP_OK_MAP_INFO)?;
        Ok(())
    }

    /// `SET_SCANOUT_BLOB` at scanout 0, pointing it at a host blob. The blob is one linear
    /// plane, so its stride is the row length and the other three planes are empty.
    fn set_scanout_blob(
        &mut self,
        resource_id: u32,
        format: u32,
        stride: u32,
    ) -> Result<(), DriverError> {
        let mut cmd = SetScanoutBlob {
            hdr: CtrlHdr::new(CMD_SET_SCANOUT_BLOB),
            r: Rect {
                x: 0,
                y: 0,
                width: XRES,
                height: YRES,
            },
            scanout_id: 0,
            resource_id,
            width: XRES,
            height: YRES,
            format,
            padding: 0,
            strides: [stride, 0, 0, 0],
            offsets: [0, 0, 0, 0],
        };
        cmd.hdr.ctx_id = BLOB_CONTEXT_ID;
        self.send_cmd(struct_bytes(&cmd))
    }

    /// Push this process's framebuffer into the host blob and nudge the host to re-read it.
    /// The blob's memory *is* the shared-memory window, so this copies into device memory the
    /// host scans out — no `TRANSFER` command, because a transfer has no guest source to name
    /// for a host blob.
    fn flush_blob(&mut self) -> Result<(), DriverError> {
        if self.blob_va == 0 {
            return Err(DriverError::NotFound);
        }
        let n = FB_SIZE as usize;
        // SAFETY: both buffers are `FB_SIZE` bytes at addresses this process may write — the
        // server's own framebuffer, and the host-visible window the PCI BARs are mapped at.
        unsafe {
            core::ptr::copy_nonoverlapping(self.fb_va as *const u8, self.blob_va as *mut u8, n);
            // The host reads the blob memory the copy above just filled, so it has to reach the
            // device before the flush command does.
            crate::hal::mfence();
        }
        self.flush_resource(RESOURCE_ID)
    }

    /// Bring up the display on a host blob: make the context the blob belongs to, create and map
    /// a full-frame blob at offset 0 of the shared-memory window, and point scanout 0 at it.
    /// `window` is the base of that window, which the blob's address is derived from.
    fn init_blob_scanout(&mut self, window: u64) -> Result<(), DriverError> {
        self.ctx_create(BLOB_CONTEXT_ID, 0, b"minixrs-fb")?;
        self.resource_create_blob_host(RESOURCE_ID, FB_SIZE)?;
        self.resource_map_blob(RESOURCE_ID, 0)?;
        self.blob_va = window;
        self.set_scanout_blob(RESOURCE_ID, VIRTIO_GPU_FORMAT_B8G8R8X8, XRES * 4)?;
        self.scanout_blob = true;
        self.flush_blob()?;

        let fb_pa = self.fb_va.wrapping_add(virtio::virtio_phys_delta() as u64);
        self.dev = FbDevice {
            base: fb_pa,
            size: FB_SIZE,
        };
        self.var = self.screen_info();
        self.fix.line_length = XRES * 4;
        self.initialized = true;
        Ok(())
    }

    /// `RESOURCE_CREATE_2D`: a resource whose contents are guest pages — the shape a device
    /// with no GL has, and the one the scanout resource is made with. It carries no target,
    /// so the geometry it can express is a plain 2D surface.
    fn resource_create_2d(
        &mut self,
        resource_id: u32,
        format: u32,
        width: u32,
        height: u32,
    ) -> Result<(), DriverError> {
        let cmd = ResourceCreate2D {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_2D),
            resource_id,
            format,
            width,
            height,
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
        target: u32,
        width: u32,
        height: u32,
        format: u32,
        bind: u32,
    ) -> Result<(), DriverError> {
        let cmd = ResourceCreate3D {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_3D),
            resource_id,
            target,
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

    /// `TRANSFER_TO_HOST_3D` / `TRANSFER_FROM_HOST_3D`: move a box of the resource's contents
    /// between the attached backing and the host-side texture.
    ///
    /// `stride` and `layer_stride` are the caller's, and zero means the protocol's "derive them
    /// from the format and geometry". Both directions are answered `NODATA` whether or not the
    /// host accepted the move, so only the bytes can tell a caller which happened.
    fn transfer_3d(
        &mut self,
        to_host: bool,
        ctx_id: u32,
        spec: &drm::Transfer3d,
    ) -> Result<(), DriverError> {
        let mut cmd = TransferHost3D {
            hdr: CtrlHdr::new(if to_host {
                CMD_TRANSFER_TO_HOST_3D
            } else {
                CMD_TRANSFER_FROM_HOST_3D
            }),
            area: Box3D {
                x: spec.x,
                y: spec.y,
                z: spec.z,
                w: spec.w,
                h: spec.h,
                d: spec.d,
            },
            offset: spec.offset as u64,
            resource_id: spec.resource_id,
            level: spec.level,
            stride: spec.stride,
            layer_stride: spec.layer_stride,
        };
        cmd.hdr.ctx_id = ctx_id;
        self.send_cmd(struct_bytes(&cmd))
    }

    /// `TRANSFER_TO_HOST_2D`: the 2D half of the transfer ABI, which is what a device with no GL
    /// has — the same command the scanout's flush is made of. A 2D resource's storage *is* the
    /// guest's pages, so this is the host's copy for the display rather than a move between two.
    fn transfer_2d(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<(), DriverError> {
        let cmd = TransferToHost2D {
            hdr: CtrlHdr::new(CMD_TRANSFER_TO_HOST_2D),
            r: Rect {
                x,
                y,
                width: w,
                height: h,
            },
            offset: 0,
            resource_id,
            padding: 0,
        };
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
        self.transfer_3d(
            false,
            ctx_id,
            &drm::Transfer3d::whole(resource_id, PROBE_TEX_W, PROBE_TEX_H),
        )?;
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
            PIPE_TEXTURE_2D,
            PROBE_TEX_W,
            PROBE_TEX_H,
            VIRGL_FORMAT_B8G8R8A8_UNORM,
            VIRGL_BIND_TEXTURE,
        )?;
        self.attach_backing(RES, pa, PROBE_TEX_BYTES)?;
        self.ctx_attach_resource(ctx_id, RES)?;
        self.transfer_3d(
            true,
            ctx_id,
            &drm::Transfer3d::whole(RES, PROBE_TEX_W, PROBE_TEX_H),
        )?;
        for b in buf.iter_mut() {
            *b = 0xA5;
        }
        self.transfer_3d(
            false,
            ctx_id,
            &drm::Transfer3d::whole(RES, PROBE_TEX_W, PROBE_TEX_H),
        )?;
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
/// Where a [`DrmNode`]'s device comes from.
///
/// On an x86 boot whose output is `bochs`, the node probes the virtio-gpu itself and owns it
/// (D3). When the virtio-gpu *is* the output (D11 — the client's render resource and the
/// scanout are one device), the fb server's output backend already holds the device's queues,
/// so the node borrows that device rather than probing again and reprogramming the rings out
/// from under the output backend.
// Thyere size difference is the point: one variant *is* the device and the other is a pointer to
// it. Boxing the large one is not available without `alloc`.
#[allow(clippy::large_enum_variant)]
enum Device {
    Owned(VirtioGpuArch),
    /// The fb server's output device, live and already owning the device's queues. A raw
    /// pointer, not a `&mut`: the output backend reaches the same device through its own
    /// static, so neither side holds a borrow the other could invalidate.
    Borrowed(*mut VirtioGpuArch),
}

impl Device {
    fn arch(&mut self) -> &mut VirtioGpuArch {
        match self {
            Device::Owned(a) => a,
            // SAFETY: the pointer is the fb server's output backend (a static), which outlives
            // every use of this node, and the server is single-threaded, so no other borrow is
            // live at the same moment.
            Device::Borrowed(a) => unsafe { &mut **a },
        }
    }
}

pub struct DrmNode {
    dev: Device,
    /// What `GET_CAPSET_INFO` said, index after index, until one answered with id 0.
    capsets: [CapsetInfo; MAX_CAPSETS],
    count: usize,
    /// The feature word, as `GETPARAM` reports it.
    virgl: bool,
    blob: bool,
    /// The device offered `VIRTIO_GPU_F_CONTEXT_INIT`. Named for the feature, not for the
    /// request, so that `context_init` above it can be the ABI's name.
    has_context_init: bool,
    /// The one context a client may have, once it has made one. The ABI ties this to the open
    /// file on Linux; this node is one allocator, so it is the node's (§6.10 D3).
    context: Option<u32>,
    /// Which objects are in that context's table, one bit per slot. A duplicate
    /// `CTX_ATTACH_RESOURCE` is not the same request as the first, and the table goes away with
    /// the context, so the bits are cleared with it.
    context_resources: u32,
    /// The objects this node has handed out.
    gems: drm::GemTable,
    /// The resource-id base the objects are numbered from (`GemTable::set_res_base`): 0 when the
    /// node's device is its own, the display's highest resource id when the node shares the
    /// display's device.
    res_base: u32,
    /// Image VA of slot 0 of the backing the objects come from. Slot `i` is at
    /// `gem_arena + i * slot_len`; the server that owns the memory names it here, so the
    /// node owns the layout and the server owns the bytes.
    gem_arena: u64,
}

impl DrmNode {
    /// The device this node drives, whichever way it came by it.
    fn dev(&mut self) -> &mut VirtioGpuArch {
        self.dev.arch()
    }

    /// A node over the fb server's *output* device, which already holds the virtio-gpu's
    /// queues (D11 — the client's render resource and the scanout are one device). No probe
    /// and no queue setup: the device is live, and negotiating it again would reprogram the
    /// rings out from under the output backend.
    ///
    /// The features are the ones that device negotiated (the output backend invites
    /// `VIRGL`/`BLOB`), so a GL session gets a GL node and a plain 2D output gets a 2D one; the
    /// capsets are walked here so a GL client can fetch them.
    /// # Safety
    ///
    /// `dev` must be the fb server's live output backend, a static that outlives the node.
    pub unsafe fn on_output_device(dev: *mut VirtioGpuArch) -> Self {
        // SAFETY: the pointer is the fb server's output backend, a static that outlives the
        // node, and the server is single-threaded.
        let arch = unsafe { &mut *dev };
        let (virgl, blob, context_init) = match &arch.vdev {
            Some(v) => (
                virtio::virtio_host_supports(v, VIRTIO_GPU_F_VIRGL),
                virtio::virtio_host_supports(v, VIRTIO_GPU_F_RESOURCE_BLOB),
                virtio::virtio_host_supports(v, VIRTIO_GPU_F_CONTEXT_INIT),
            ),
            None => (false, false, false),
        };
        let mut node = Self {
            dev: Device::Borrowed(dev),
            capsets: [CapsetInfo::default(); MAX_CAPSETS],
            count: 0,
            virgl,
            blob,
            has_context_init: context_init,
            context: None,
            context_resources: 0,
            gems: drm::GemTable::new(0),
            gem_arena: 0,
            // The output backend holds resources 1 and 2 on this device, so the node numbers
            // above them.
            res_base: EXT_SCANOUT_RESOURCE_ID,
        };
        if virgl {
            // The host lists its capsets by index and answers past the last one with a zero id
            // instead of an error, so this walks until then — bounded, as in `open_render_node`.
            while node.count < MAX_CAPSETS {
                let index = node.count as u32;
                match node.dev().capset_info(index) {
                    Ok(info) if info.id != 0 => {
                        node.capsets[node.count] = info;
                        node.count += 1;
                    }
                    _ => break,
                }
            }
        }
        node
    }

    /// Hand the node the backing its objects come from: the image VA of slot 0, and how
    /// many bytes one slot holds. The *server* owns the memory (it is the one that can name
    /// a guest-physical address and hand a client a mapping of it), so the node is told
    /// rather than given it. Until this is called the node has no backing and refuses every
    /// create.
    pub fn set_gem_arena(&mut self, va: u64, slot_len: u32) {
        self.gem_arena = va;
        self.gems = drm::GemTable::new(slot_len);
        self.gems.set_res_base(self.res_base);
    }

    /// The image VA of slot `slot` of the backing. Which memory a slot names is this node's
    /// layout, so the arithmetic lives here and the server owns only the arena's base and
    /// slot size.
    fn slot_va(&self, slot: usize) -> u64 {
        self.gem_arena + slot as u64 * self.gems.slot_len() as u64
    }

    /// The image VA and length of the object an `mmap` offset names, or `ENOENT` for one
    /// this node does not hold. Translating the VA to a guest-physical address is the
    /// server's, because the VA→PA delta is its process's.
    pub fn map_range(&self, offset: u64) -> Result<(u64, u64), i32> {
        let Some(slot) = drm::GemTable::slot_of_offset(offset) else {
            return Err(drm::ENOENT);
        };
        let Some(len) = self.gems.len_at_offset(offset) else {
            return Err(drm::ENOENT);
        };
        Ok((self.slot_va(slot), len as u64))
    }

    /// `CDEV_CLOSE`: give up the context the node holds.
    ///
    /// Linux ties a context to the open file and destroys it when that file closes, so this is
    /// where a client's context goes. This node has no per-open table (§6.10 D3's deviation),
    /// so a close releases the one context it has; the day the node tracks opens, each close
    /// releases its own. A host that refuses the destroy keeps the context, so closing again can
    /// retry rather than leaving the host holding something this side has forgotten.
    pub fn release_context(&mut self) {
        let Some(ctx_id) = self.context else {
            return;
        };
        if self.dev().ctx_destroy(ctx_id).is_ok() {
            self.context = None;
            // The table went with the context, so the objects have to be attached again if a
            // client asks for another one.
            self.context_resources = 0;
        }
    }

    /// The context a transfer or a submit goes through, made if the client has not made one.
    ///
    /// Linux's `virtio_gpu_create_context` does exactly this, with no capset: a context that
    /// renders nothing still moves bytes and runs commands that name no capset, and a client that
    /// transferred before initialising one is not making an error.
    fn ensure_context(&mut self) -> Result<u32, i32> {
        match self.context {
            Some(ctx) => Ok(ctx),
            None => {
                self.create_context(0)?;
                Ok(CONTEXT_ID)
            }
        }
    }

    /// Make the context, with the `context_init` flags a request carried.
    ///
    /// `minixrs` for the debug name: Linux names a context after the calling *task*, and a
    /// process on this port has no comm to be named by.
    fn create_context(&mut self, flags: u32) -> Result<(), i32> {
        self.dev()
            .ctx_create(CONTEXT_ID, flags, b"minixrs")
            .map_err(|_| drm::EINVAL)?;
        self.context = Some(CONTEXT_ID);
        Ok(())
    }

    /// Put `handle`'s resource in the context's table, and remember that it is there.
    ///
    /// Every transfer and every command names its objects *through* the context, and a resource
    /// the context does not hold is refused by the host with an error that never reaches this
    /// side (`ctx_attach_resource`'s note) — so a missed attach shows as bytes that did not move,
    /// not as a failed call. Attached once, because a duplicate attach is not the same request as
    /// the first one.
    fn attach_to_context(&mut self, ctx_id: u32, handle: u32) -> Result<(), i32> {
        let Some((slot, _)) = self.gems.lookup(handle) else {
            return Err(drm::ENOENT);
        };
        if self.context_resources & (1 << slot) == 0 {
            self.dev()
                .ctx_attach_resource(ctx_id, handle)
                .map_err(|_| drm::EINVAL)?;
            self.context_resources |= 1 << slot;
        }
        Ok(())
    }

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
    /// The identity a client reads with `DRM_IOCTL_VERSION`.
    ///
    /// `major` is 0 because Linux's virtio_gpu reports 0 (`virtgpu_drv.h`'s `DRIVER_MAJOR`), and
    /// Mesa's virgl winsys *refuses a device whose major is not*. `virgl_drm_get_version` returns
    /// `-EINVAL` for a non-zero one and `virgl_drm_winsys_create` then answers NULL, so GL would go
    /// missing entirely for a number this side picked freely.
    ///
    /// `minor` is a *feature* level rather than a revision: 1 is `VIRTGPU_EXECBUF_FENCE_FD_IN`/
    /// `_OUT`, because `supports_fences = drm_version >= VIRGL_DRM_VERSION(0, 1)`, and with fences
    /// Mesa passes an in/out fence fd on every submit — which `submit` below refuses. Claiming 1
    /// would make every submit fail rather than fall back; 0 is what makes Mesa take the path this
    /// node can drive, a buffer resource polled through `VIRTGPU_WAIT` with `NOWAIT`. It becomes 1
    /// the day D4's fences land.
    fn version(&self) -> drm::DrmVersion {
        drm::DrmVersion {
            major: 0,
            minor: 0,
            patchlevel: 0,
            name: "virtio_gpu",
            date: "20260927",
            desc: "minixrs virtio-gpu render node",
        }
    }

    /// `DRM_IOCTL_GET_PCIINFO`: the PCI identity libdrm on OpenBSD/DragonFly reads to name the
    /// DRI driver to load. The node has no PCI bus on the mmio arches, but the identity is the
    /// same virtio-gpu one everywhere, and that is the pair Mesa's loader maps to `virtio_gpu`.
    fn pci_info(&self) -> drm::PciInfo {
        drm::PciInfo {
            domain: 0,
            bus: 0,
            dev: 0,
            func: 0,
            vendor_id: VIRTIO_PCI_VENDOR_ID,
            device_id: VIRTIO_GPU_PCI_DEVICE_ID,
            subvendor_id: VIRTIO_PCI_VENDOR_ID,
            subdevice_id: VIRTIO_GPU_PCI_DEVICE_ID,
            revision_id: 0,
        }
    }

    fn get_cap(&mut self, capability: u64) -> Result<u64, i32> {
        // A render node has no dumb buffers — those belong to the mode-setting path — and it
        // does have PRIME: every object is guest memory the node can name, so export and
        // import are both answerable (§6.10, 3d-b-2).
        match capability {
            drm::DRM_CAP_DUMB_BUFFER => Ok(0),
            drm::DRM_CAP_PRIME => Ok(drm::DRM_PRIME_CAP_IMPORT | drm::DRM_PRIME_CAP_EXPORT),
            _ => Err(drm::EINVAL),
        }
    }

    fn getparam(&mut self, param: u64) -> Result<i32, i32> {
        match param {
            // Whether the *device* has 3D, which is what a client reads to decide the
            // node is usable. A node exists for a device without GL too.
            drm::VIRTGPU_PARAM_3D_FEATURES => Ok(self.virgl as i32),
            drm::VIRTGPU_PARAM_CAPSET_QUERY_FIX => Ok(1),
            drm::VIRTGPU_PARAM_RESOURCE_BLOB => Ok(self.blob as i32),
            // The node hands out guest blobs only, so a client is told there is no host-visible
            // kind to map — even when the device has the shared-memory window
            // (`VIRTIO_PCI_CAP_SHARED_MEMORY_CFG`, grown under the `hostmem=` a GL output needs).
            // That window is the *display's*: the scanout blob is mapped into it by the driver,
            // and the node's `create_blob` still refuses `HOST3D`. CROSS_DEVICE is 0 because there
            // is one virtio device, so there is nothing to share across.
            drm::VIRTGPU_PARAM_HOST_VISIBLE | drm::VIRTGPU_PARAM_CROSS_DEVICE => Ok(0),
            drm::VIRTGPU_PARAM_CONTEXT_INIT => Ok(self.has_context_init as i32),
            drm::VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS => Ok(self.capset_mask() as i32),
            _ => Err(drm::EINVAL),
        }
    }

    fn get_caps(&mut self, capset_id: u32, version: u32, out: &mut [u8]) -> Result<usize, i32> {
        // A node with no capsets is a device the host gave no GL (D7's degrade), and the
        // ABI's answer for "there is no capset here at all" is `ENOSYS`. `EINVAL` would
        // instead say the *request* was malformed, which is a client's bug and not this
        // machine's — a difference a client reads to decide whether to retry elsewhere.
        if self.count == 0 {
            return Err(drm::ENOSYS);
        }
        let Some(info) = self.capset(capset_id) else {
            return Err(drm::EINVAL);
        };
        // A version above the host's is refused here rather than passed on: the host
        // answers an over-high version with an error, and a caller asking with one has
        // plainly not read `GET_CAPSET_INFO` first.
        if version > info.max_version {
            return Err(drm::EINVAL);
        }
        self.dev()
            .fetch_capset(capset_id, version, out)
            .map(|n| n as usize)
            .map_err(|_| drm::EINVAL)
    }

    fn create_object(&mut self, request: drm::ResourceCreate) -> Result<drm::CreatedObject, i32> {
        // A node with no backing can name no object. The server that owns the memory says
        // so with a zero-length slot, and every create is then `EINVAL` rather than a
        // mapping onto nothing.
        if self.gems.slot_len() == 0 {
            return Err(drm::EINVAL);
        }
        // Linux's re-create: a new resource attached to an existing GEM object. Refused
        // rather than answered with a second object, which is what a client that set the
        // field would otherwise silently get.
        if request.bo_handle != 0 {
            return Err(drm::EINVAL);
        }
        // The shape a 2D command cannot carry. Linux refuses these the same way when the
        // device has no 3D, and the checks are on the request rather than on the device so
        // that a client hears about them before the geometry is used.
        if !self.virgl
            && (request.depth > 1
                || request.nr_samples > 1
                || request.last_level > 1
                || request.target != PIPE_TEXTURE_2D
                || request.array_size > 1)
        {
            return Err(drm::EINVAL);
        }
        // The caller's own byte count: this ABI does not derive the length from the geometry,
        // and a request that named none gets one page, which is the table's rule.
        let object = self.gems.create(request.size)?;
        let pa = self
            .slot_va((object.handle - 1) as usize)
            .wrapping_add(virtio::virtio_phys_delta() as u64);

        let host = if self.virgl {
            self.dev().resource_create_3d(
                object.res_handle,
                request.target,
                request.width,
                request.height,
                request.format,
                request.bind,
            )
        } else {
            self.dev().resource_create_2d(
                object.res_handle,
                request.format,
                request.width,
                request.height,
            )
        };
        if host.is_err() {
            // Nothing to unref: the host refused the resource itself, so the slot is all this
            // create took, and it goes back before the failure is reported. A handle the
            // client has but cannot use would surface at `mmap`, one call further from the
            // cause.
            self.gems.remove(object.handle);
            return Err(drm::EINVAL);
        }
        if self
            .dev()
            .attach_backing(object.res_handle, pa, object.size)
            .is_err()
        {
            // The resource exists now. The slot is released only once the host has released
            // the resource, so a refused unref keeps the two together: the create fails, and
            // the next one cannot reuse an id the device never gave back.
            if self.dev().resource_unref(object.res_handle).is_ok() {
                self.gems.remove(object.handle);
            }
            return Err(drm::EINVAL);
        }
        Ok(object)
    }

    fn create_blob(&mut self, request: drm::BlobCreate) -> Result<drm::CreatedObject, i32> {
        // The feature first: a device that never offered `VIRTIO_GPU_F_RESOURCE_BLOB` has no blob
        // kind at all, so *every* blob request is one this node cannot read. `GETPARAM` reports
        // the same bit, which is how a client knows not to ask.
        if !self.blob {
            return Err(drm::EINVAL);
        }
        if request.blob_flags & !drm::VIRTGPU_BLOB_FLAG_USE_MASK != 0 {
            return Err(drm::EINVAL);
        }
        // Sharing a blob with another device needs `RESOURCE_ASSIGN_UUID`, a command this port has
        // not written — which is the same reason `GETPARAM` answers `CROSS_DEVICE` with 0.
        if request.blob_flags & drm::VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE != 0 {
            return Err(drm::EINVAL);
        }
        match request.blob_mem {
            drm::VIRTGPU_BLOB_MEM_GUEST => {}
            // Host memory reaches the guest through a shared-memory window this device does not
            // have, which is what its 0 for `VIRTGPU_PARAM_HOST_VISIBLE` says. The request is
            // well formed and this device cannot answer it, so it is refused rather than answered
            // with an object no client could map or read.
            drm::VIRTGPU_BLOB_MEM_HOST3D | drm::VIRTGPU_BLOB_MEM_HOST3D_GUEST => {
                return Err(drm::ENOSYS);
            }
            _ => return Err(drm::EINVAL),
        }
        // A guest blob's memory is its own, so the two fields that name *another* object's memory
        // have to be absent. Linux refuses either being set, and doing the same is what keeps this
        // node from ever having to reach a user pointer to read `cmd`.
        if request.cmd_size != 0 || request.blob_id != 0 {
            return Err(drm::EINVAL);
        }
        // The ABI's length is 64-bit and this node's backing is counted in 32, so a request too
        // large for the type is refused rather than truncated into one that fits.
        let Ok(bytes) = u32::try_from(request.size) else {
            return Err(drm::EINVAL);
        };
        let object = self.gems.create_blob(bytes, request.blob_mem)?;
        let pa = self
            .slot_va((object.handle - 1) as usize)
            .wrapping_add(virtio::virtio_phys_delta() as u64);
        let created = self.dev().resource_create_blob_guest(
            object.res_handle,
            request.blob_mem,
            request.blob_flags,
            pa,
            object.size,
        );
        if created.is_err() {
            // The slot goes back before the failure is reported, as a failed `RESOURCE_CREATE`
            // releases its own: a handle a client has but cannot use would surface at `mmap`, one
            // call further from the cause.
            self.gems.remove(object.handle);
            return Err(drm::EINVAL);
        }
        Ok(object)
    }

    fn map_offset(&mut self, handle: u32) -> Result<u64, i32> {
        let Some((slot, _)) = self.gems.lookup(handle) else {
            return Err(drm::ENOENT);
        };
        Ok(drm::GemTable::offset_of(slot))
    }

    fn resource_info(&mut self, handle: u32) -> Result<drm::ResourceInfo, i32> {
        let Some((_, len)) = self.gems.lookup(handle) else {
            return Err(drm::ENOENT);
        };
        Ok(drm::ResourceInfo {
            res_handle: handle,
            size: len,
            // The kind the object was made with, which is the one field of a blob that no
            // arithmetic on the others can reconstruct: 0 for an object that is not a blob.
            blob_mem: self.gems.blob_mem(handle),
        })
    }

    fn close_object(&mut self, handle: u32) -> Result<(), i32> {
        if self.gems.lookup(handle).is_none() {
            return Err(drm::ENOENT);
        }
        // Unref first, and keep the slot on a refusal: the slot names memory the host still
        // holds a resource for, so releasing it early would let the next create reuse a
        // resource id the device never gave back.
        if self.dev().resource_unref(handle).is_err() {
            return Err(drm::EINVAL);
        }
        // The context's table must not be left believing it holds a slot that is about to be a
        // different object: slot `i` is reused by the next create, and a stale bit would skip the
        // attach that object needs.
        if let Some((slot, _)) = self.gems.remove(handle) {
            self.context_resources &= !(1 << slot);
        }
        Ok(())
    }

    fn context_init(&mut self, params: &[drm::ContextParam]) -> Result<(), i32> {
        // Both the feature and GL: a device without `VIRTIO_GPU_F_CONTEXT_INIT` has no context
        // command, and one without GL has no capset for a context to render with.
        if !self.has_context_init || !self.virgl {
            return Err(drm::EINVAL);
        }
        // One context per client, and a second is `EEXIST` rather than a new one — Linux ties it
        // to the open file and refuses the second attempt on the same file.
        if self.context.is_some() {
            return Err(drm::EEXIST);
        }
        let mut flags = 0u32;
        for p in params {
            match p.param {
                drm::VIRTGPU_CONTEXT_PARAM_CAPSET_ID => {
                    // The capset must be one the device has, and it may be named once.
                    if p.value > u32::MAX as u64
                        || self.capset(p.value as u32).is_none()
                        || flags != 0
                    {
                        return Err(drm::EINVAL);
                    }
                    flags = p.value as u32;
                }
                drm::VIRTGPU_CONTEXT_PARAM_NUM_RINGS => {
                    // This node serves one command ring — its `CTX_CREATE` carries no ring count
                    // — so one is the only number it can honour. Linux allows up to its own
                    // `MAX_RINGS`; a caller asking for more rings hears about it here rather
                    // than getting one and believing it has more.
                    if p.value > 1 {
                        return Err(drm::EINVAL);
                    }
                }
                // The ring-poll mask asks for completion events on rings, which this port does
                // not deliver, and `DEBUG_NAME` is a parameter this version of the host ABI
                // rejects itself (a context is named after the calling task there).
                _ => return Err(drm::EINVAL),
            }
        }
        self.create_context(flags)
    }

    fn transfer(&mut self, to_host: bool, request: &drm::Transfer3d) -> Result<(), i32> {
        // The ABI names the object by its GEM handle; the host knows it by the resource id the
        // same slot carries, which is why they are numbered alike.
        if self.gems.lookup(request.resource_id).is_none() {
            return Err(drm::ENOENT);
        }
        if !self.virgl {
            // A device with no GL has the 2D transfer and not the 3D one, and only in the
            // direction a display needs: the host's own driver refuses the other with `ENOSYS`
            // (`virtio_gpu_transfer_from_host_ioctl`).
            if !to_host {
                return Err(drm::ENOSYS);
            }
            return self
                .dev()
                .transfer_2d(
                    request.resource_id,
                    request.x,
                    request.y,
                    request.w,
                    request.h,
                )
                .map_err(|_| drm::EINVAL);
        }
        let ctx = self.ensure_context()?;
        // A 3D transfer is read *through* the context, so the object has to be in its table
        // before the transfer names it — the same ordering a submit needs, and for the same
        // silent reason.
        self.attach_to_context(ctx, request.resource_id)?;
        self.dev()
            .transfer_3d(to_host, ctx, request)
            .map_err(|_| drm::EINVAL)
    }

    fn submit(&mut self, command: &[u8], handles: &[u32]) -> Result<(), i32> {
        // A submit is a virgl command buffer, so without GL there is no renderer to hand it to.
        // `ENOSYS` for the same reason the 3D transfer is: the request is well formed and this
        // device cannot do it.
        if !self.virgl {
            return Err(drm::ENOSYS);
        }
        let ctx = self.ensure_context()?;
        for handle in handles {
            self.attach_to_context(ctx, *handle)?;
        }
        self.dev().submit_3d(ctx, command).map_err(|_| drm::EINVAL)
    }

    fn wait(&mut self, handle: u32, _nowait: bool) -> Result<(), i32> {
        if self.gems.lookup(handle).is_none() {
            return Err(drm::ENOENT);
        }
        // Reported as done, which is why `submit` can be synchronous at all: the device answers a
        // `SUBMIT_3D` after the renderer has run it — the boot probe reads an inline write's
        // effect back through the transfer that follows it — so there is no queued work to wait
        // for. `nowait` therefore changes nothing, and blocking would be a lie about work this
        // node does not have (`.notes/wayland.md` §6.10 D4: no fence objects yet).
        Ok(())
    }

    /// The object's export token is the page-granular name `map_offset` hands out, so the
    /// number a client gets from an export is the same identity the object answers `mmap`
    /// with — one name for one object, whether it is being mapped or shared.
    fn prime_export(&mut self, handle: u32) -> Result<u32, i32> {
        let Some((slot, _)) = self.gems.lookup(handle) else {
            return Err(drm::ENOENT);
        };
        Ok(drm::GemTable::offset_of(slot) as u32)
    }

    /// A token names a slot, and a slot is a live object only while it holds one — so a token
    /// the node never issued, or one whose object was closed, is `ENOENT`. `len_at_offset` is
    /// the same liveness test `map_offset` uses; the handle is the slot's own number, so an
    /// import yields the *existing* handle for the object, which is Linux's documented
    /// behavior for a second import of one object on one file description.
    fn prime_import(&mut self, token: u32) -> Result<u32, i32> {
        if self.gems.len_at_offset(token as u64).is_none() {
            return Err(drm::ENOENT);
        }
        let Some(slot) = drm::GemTable::slot_of_offset(token as u64) else {
            return Err(drm::ENOENT);
        };
        Ok(slot as u32 + 1)
    }
}

/// The matching `virtio-gpu` devices the role pick considers: one display and, at most, one render
/// node. Bounded so a host whose `skip` never runs out cannot make the probe spin.
pub const MAX_GPU_DEVICES: u16 = 2;

/// Split devices into the display and node roles by *capability*, not by the order QEMU enumerates
/// them (§6.10 3d-b). `virgl[i]` is device `i`'s VIRGL support.
///
/// The display takes the first device *without* VIRGL: classic scanout is the only scanout a host
/// with no GL (Windows' ANGLE) can do, and a GL device's only scanout is a host blob. When every
/// device has GL the first is the display and its scanout is a blob (RISC-V/AArch64's one device).
/// The node is the first device *with* VIRGL that is not the display; `None` means the display is
/// itself the node — because it is the only GL device (D11) or because nothing has GL (D7).
fn assign_roles(virgl: &[bool]) -> Option<(u16, Option<u16>)> {
    if virgl.is_empty() {
        return None;
    }
    let display = virgl.iter().position(|&v| !v).unwrap_or(0) as u16;
    let node = if virgl[display as usize] {
        None
    } else {
        virgl
            .iter()
            .enumerate()
            .find(|&(i, &v)| v && i != display as usize)
            .map(|(i, _)| i as u16)
    };
    Some((display, node))
}

/// The node's `skip` when nothing else holds a `virtio-gpu` (the output is `bochs`, or there is no
/// output device at all): the first device with VIRGL, or the first device at all when none has it,
/// whose node then reports no GL (D7).
fn pick_node_skip(virgl: &[bool]) -> Option<u16> {
    if virgl.is_empty() {
        return None;
    }
    Some(virgl.iter().position(|&v| v).unwrap_or(0) as u16)
}

/// Probe up to [`MAX_GPU_DEVICES`] matching `virtio-gpu` devices and report each one's VIRGL
/// support and how many there were. The devices are dropped: a caller that keeps one re-probes it,
/// which is harmless before its queues are set up.
fn probe_virgl_flags(name: &'static str) -> ([bool; MAX_GPU_DEVICES as usize], usize) {
    let mut virgl = [false; MAX_GPU_DEVICES as usize];
    let mut count = 0usize;
    while count < MAX_GPU_DEVICES as usize {
        let Ok(dev) = virtio::virtio_probe(VIRTIO_GPU_DEVICE_ID, name, GPU_FEATURES, count as u16)
        else {
            break;
        };
        virgl[count] = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_VIRGL);
        count += 1;
    }
    (virgl, count)
}

/// Probe the machine's `virtio-gpu` devices, pick the display by capability, and report the node's
/// `skip` (`None` when the display device is the node too). The display keeps its device, probed in
/// the transport's slot 0; the node re-probes its own in a later slot, so the two never share a slot
/// (the vring storage is one set per slot).
fn probe_roles() -> Result<(VirtioDevice, Option<u16>), DriverError> {
    let (virgl, count) = probe_virgl_flags("virtio-gpu");
    let (display, node) = assign_roles(&virgl[..count]).ok_or(DriverError::NotFound)?;
    let dev = virtio::virtio_probe(VIRTIO_GPU_DEVICE_ID, "virtio-gpu", GPU_FEATURES, display)
        .map_err(|_| DriverError::NotFound)?;
    Ok((dev, node))
}

/// Negotiate the render node and take it (§6.10, stage 3b): the features, the control
/// queues, and the capsets the host offers.
///
/// `out` receives what the negotiation learned whether or not a node comes back, because
/// "there is no 3D here" is itself an answer a caller has to be able to report (D7). `skip`
/// selects which matching device becomes the node — 1 when the display already holds the first
/// one — and `slot` names the vring storage its queues use, which must differ from a display
/// device's (the transport's storage is one set per slot per process).
fn open_render_node(out: &mut Gpu3d, skip: u16, slot: usize) -> Option<DrmNode> {
    let Ok(mut dev) = virtio::virtio_probe_slot(
        VIRTIO_GPU_DEVICE_ID,
        "virtio-gpu-3d",
        GPU_FEATURES,
        skip,
        slot,
    ) else {
        return None;
    };
    out.device = true;
    out.virgl = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_VIRGL);
    out.blob = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_RESOURCE_BLOB);
    out.context_init = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_CONTEXT_INIT);

    // The control queues exist for a device that can be *commanded*, which is what the 2D
    // commands a render node serves its clients with need — GL or no GL. The capset and
    // context commands are the ones a device without VIRGL answers with errors, so those
    // are what the feature gates below; the queues are not.
    let Ok(queues) = setup_queues(&mut dev) else {
        return None;
    };
    out.queues = queues;

    let mut node = DrmNode {
        dev: Device::Owned(VirtioGpuArch::new()),
        capsets: [CapsetInfo::default(); MAX_CAPSETS],
        count: 0,
        virgl: out.virgl,
        blob: out.blob,
        has_context_init: out.context_init,
        res_base: 0,
        context: None,
        context_resources: 0,
        gems: drm::GemTable::new(0),
        gem_arena: 0,
    };
    node.dev().vdev = Some(dev);
    // A device with no GL still has a *node*: a DRM render node whose driver has no 3D
    // answers `VERSION`, answers `GETPARAM` with `3D_FEATURES` 0, and refuses `GET_CAPS`
    // with `ENOSYS` — which is what a client is meant to find there (D7's degrade, seen
    // from the client's side). Only a machine with no device at all has no node.
    if !out.virgl {
        return Some(node);
    }

    // The host lists its capsets by index and answers past the last one with a zero id
    // instead of an error, so this walks until then — bounded, in case a host never does.
    while node.count < MAX_CAPSETS {
        let index = node.count as u32;
        match node.dev().capset_info(index) {
            Ok(info) if info.id != 0 => {
                node.capsets[node.count] = info;
                node.count += 1;
            }
            Ok(_) => break,
            Err(_) => {
                // Only worth reporting when nothing was learnt: one refused query after a
                // good first one is a host saying "that is all", badly.
                if node.count == 0 {
                    out.capset_rtype = node.dev().last_response;
                }
                break;
            }
        }
    }
    // A host that answered with no capset at all is still a node — `GET_CAPS` is what
    // then refuses, with `ENOSYS`, which is the ABI's answer for "no capsets" — so only
    // the report is left short.
    if node.count > 0 {
        out.capset = node.capsets[0];
        out.capset_count = node.count as u32;
    }
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
    // Nothing else holds a matching device here (the output is `bochs`, or there is none), so the
    // node may take the first device with GL, in slot 0 — but it must still find it when that is
    // not the first device, so the pick is by capability like the fb output's (§6.10 3d-b).
    let (virgl, count) = probe_virgl_flags("virtio-gpu-3d");
    match pick_node_skip(&virgl[..count]) {
        Some(skip) => probe_render_node_in(skip, 0),
        None => (Gpu3d::default(), None),
    }
}

/// [`probe_render_node`] for a process whose display holds a *different* `virtio-gpu` device:
/// `skip` picks which matching device becomes the node (1 when the display took the first), and
/// `slot` names the vring storage its queues use. Two devices in one process must occupy
/// different slots, because the transport's vring storage is one set per slot.
pub fn probe_render_node_in(skip: u16, slot: usize) -> (Gpu3d, Option<DrmNode>) {
    let mut out = Gpu3d::default();
    let Some(mut node) = open_render_node(&mut out, skip, slot) else {
        return (out, None);
    };
    // Without VIRGL there is nothing to fetch or create — the device answers those with
    // errors — so the report stops here with the features it saw and the node goes back
    // unasked.
    if !out.virgl {
        return (out, Some(node));
    }
    // The blob for the capset the report names: what the host renders with is only useful
    // with its contents.
    if let Ok(len) = node.dev().capset_blob(out.capset) {
        out.capset_len = len;
    }
    // Create is the evidence and destroy keeps the host from holding a context the port
    // will never use, so the flag means both halves worked.
    let created = node.dev().ctx_create(CONTEXT_ID, 0, b"minixrs").is_ok();
    if created {
        // The bytes, not the response types: see `Gpu3d::xfer_matched` and
        // `Gpu3d::submit_matched`.
        if let Ok((xfer, submit)) = node.dev().round_trip(CONTEXT_ID) {
            out.xfer_matched = xfer;
            out.submit_matched = submit;
        }
    }
    out.ctx = created && node.dev().ctx_destroy(CONTEXT_ID).is_ok();
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

        // The device is picked by capability, not by whichever QEMU enumerates first (§6.10
        // 3d-b): a device *without* GL is the display, because classic scanout is the only scanout
        // a host with no GL (Windows' ANGLE) can do, and a GL device's only scanout is a host blob.
        // A GL device that is not the display becomes the render node instead. The same features
        // the render node asks for are negotiated here, because when the virtio-gpu *is* the output
        // the node is served from this device (D11), so the display must negotiate VIRGL/blob for
        // the node to answer 3D at all. A plain 2D virtio-gpu simply does not negotiate them, and
        // the node reports no GL, as before.
        let (mut dev, node_skip) = probe_roles()?;
        let _ = setup_queues(&mut dev)?;
        let virgl = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_VIRGL);
        let blob = virtio::virtio_host_supports(&dev, VIRTIO_GPU_F_RESOURCE_BLOB);
        let window = virtio::virtio_hostmem_region(&dev);
        self.vdev = Some(dev);
        self.node_skip = node_skip;

        // A GL device never turns a 2D resource into an EGL image, so its classic `SET_SCANOUT`
        // is refused — its only scanout is a host blob, which the shared-memory window carries.
        // A device that carries GL without both halves of that has no scanout at all, and saying
        // so is better than a command the host is going to refuse.
        if virgl {
            return match (blob, window) {
                (true, Some((base, _len))) => self.init_blob_scanout(base),
                _ => Err(DriverError::Unsupported),
            };
        }

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
        if self.scanout_blob {
            // The scanout is a host blob: push the framebuffer into it and let the host re-read.
            self.flush_blob()
        } else if self.scanout_resource == EXT_SCANOUT_RESOURCE_ID {
            // The scanout is the client's own memory: only the host's re-read is owed, and a
            // TRANSFER would copy guest memory onto itself.
            self.flush_resource(EXT_SCANOUT_RESOURCE_ID)
        } else {
            self.flush_inner()
        }
    }

    /// Present guest memory this process does not own: create a 2D resource backed by `ranges` and
    /// point scanout 0 at it. The scanout then *is* the buffer, so presenting it is a flush — no
    /// transfer and no blit. The ranges are the caller's to translate (a device server has no page
    /// tables of its own to walk for another process's buffer).
    fn set_scanout_source(&mut self, ranges: &[ScanRange]) -> Result<(), DriverError> {
        if !self.initialized {
            return Err(DriverError::NotFound);
        }
        // A host-blob scanout is the *host's* memory, not the caller's pages, so there is nothing
        // to re-point at `ranges` and no command to back a blob with them. Presents on a GL output
        // go through `/dev/fb` and are blitted; only a classic scanout can be re-pointed.
        if self.scanout_blob {
            return Err(DriverError::Unsupported);
        }
        if self.ext_resource {
            self.resource_unref(EXT_SCANOUT_RESOURCE_ID)?;
            self.ext_resource = false;
        }
        self.resource_create_2d(
            EXT_SCANOUT_RESOURCE_ID,
            VIRTIO_GPU_FORMAT_B8G8R8X8,
            XRES,
            YRES,
        )?;
        self.attach_backing_ranges(EXT_SCANOUT_RESOURCE_ID, ranges)?;
        self.set_scanout_resource(EXT_SCANOUT_RESOURCE_ID)?;
        self.ext_resource = true;
        self.scanout_resource = EXT_SCANOUT_RESOURCE_ID;
        self.flush_resource(EXT_SCANOUT_RESOURCE_ID)
    }

    /// Point the output back at the driver's own buffer and drop the external resource.
    fn clear_scanout_source(&mut self) -> Result<(), DriverError> {
        if !self.initialized {
            return Err(DriverError::NotFound);
        }
        if !self.ext_resource {
            return Ok(());
        }
        self.set_scanout_resource(RESOURCE_ID)?;
        self.resource_unref(EXT_SCANOUT_RESOURCE_ID)?;
        self.ext_resource = false;
        self.scanout_resource = RESOURCE_ID;
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
        assert_eq!(core::mem::size_of::<SetScanoutBlob>(), 96);
        assert_eq!(core::mem::size_of::<ResourceMapBlob>(), 40);
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
        assert_eq!(CMD_RESOURCE_CREATE_BLOB, 0x010c);
        assert_eq!(CMD_SET_SCANOUT_BLOB, 0x010d);
        assert_eq!(CMD_RESOURCE_MAP_BLOB, 0x0208);
        assert_eq!(RESP_OK_MAP_INFO, 0x1106);
        assert_eq!(BLOB_MEM_HOST3D, 0x0002);
        assert_eq!(BLOB_FLAG_USE_MAPPABLE, 0x0001);
        assert_eq!(FB_SIZE, 1024 * 768 * 4);
    }

    #[test]
    fn assign_roles_picks_the_display_by_capability_not_order() {
        // A device without GL is the display whatever order it comes in, and the first GL device
        // that is not the display is the node — so a `[2D, GL]` line and a `[GL, 2D]` line pick
        // the same pair (§6.10 3d-b).
        assert_eq!(assign_roles(&[false, true]), Some((0, Some(1))));
        assert_eq!(assign_roles(&[true, false]), Some((1, Some(0))));
    }

    #[test]
    fn assign_roles_borrows_when_the_display_is_the_node() {
        // One device, GL or not, is both roles: `None` tells the fb server to borrow the output
        // device (D11 / D7). Two GL devices leave the second as the node too.
        assert_eq!(assign_roles(&[false]), Some((0, None)));
        assert_eq!(assign_roles(&[true]), Some((0, None)));
        assert_eq!(assign_roles(&[false, false]), Some((0, None)));
        assert_eq!(assign_roles(&[true, true]), Some((0, None)));
        assert_eq!(assign_roles(&[]), None);
    }

    #[test]
    fn pick_node_skip_finds_the_first_gl_device() {
        // With no virtio-gpu output the node takes the first GL device, wherever it is, and falls
        // back to the first device — whose node reports no GL (D7) — when none has it.
        assert_eq!(pick_node_skip(&[false, true]), Some(1));
        assert_eq!(pick_node_skip(&[true, false]), Some(0));
        assert_eq!(pick_node_skip(&[false]), Some(0));
        assert_eq!(pick_node_skip(&[]), None);
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
    fn attach_backing_ranges_lays_entries_after_the_body() {
        let ranges = [
            ScanRange {
                pa: 0x1000_0000,
                len: 4096,
            },
            ScanRange {
                pa: 0x2000_0000,
                len: 8192,
            },
            ScanRange {
                pa: 0x3000_0000,
                len: 4096,
            },
        ];
        let mut out = [0u8; 128];
        let n = attach_backing_bytes(7, &ranges, &mut out).unwrap();
        assert_eq!(n, 32 + 3 * 16);
        assert_eq!(
            u32::from_le_bytes(out[0..4].try_into().unwrap()),
            CMD_RESOURCE_ATTACH_BACKING
        );
        assert_eq!(u32::from_le_bytes(out[24..28].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(out[28..32].try_into().unwrap()), 3);
        // Entries begin at 32 and are 16 bytes each: addr u64, length u32, padding u32.
        assert_eq!(
            u64::from_le_bytes(out[32..40].try_into().unwrap()),
            0x1000_0000
        );
        assert_eq!(u32::from_le_bytes(out[40..44].try_into().unwrap()), 4096);
        assert_eq!(
            u64::from_le_bytes(out[48..56].try_into().unwrap()),
            0x2000_0000
        );
        assert_eq!(u32::from_le_bytes(out[56..60].try_into().unwrap()), 8192);
        assert_eq!(
            u64::from_le_bytes(out[64..72].try_into().unwrap()),
            0x3000_0000
        );
        // No ranges, or one that does not fit the buffer, is refused rather than truncated.
        assert!(attach_backing_bytes(1, &[], &mut out).is_none());
        assert!(attach_backing_bytes(1, &ranges, &mut out[..40]).is_none());
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

    /// The two blob-scanout commands against the spec: `SET_SCANOUT_BLOB` carries the resource's
    /// format and geometry and its four plane strides and offsets, and `RESOURCE_MAP_BLOB` names
    /// the window offset it wants the blob at. A field at the wrong offset is a command the host
    /// reads as a different request.
    #[test]
    fn blob_scanout_commands_layout() {
        let mut ss = SetScanoutBlob {
            hdr: CtrlHdr::new(CMD_SET_SCANOUT_BLOB),
            r: Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            },
            scanout_id: 0,
            resource_id: 1,
            width: 1024,
            height: 768,
            format: VIRTIO_GPU_FORMAT_B8G8R8X8,
            padding: 0,
            strides: [4096, 0, 0, 0],
            offsets: [0, 0, 0, 0],
        };
        ss.hdr.ctx_id = BLOB_CONTEXT_ID;
        let b = struct_bytes(&ss);
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_SET_SCANOUT_BLOB
        );
        // scanout_id @40, resource_id @44, width @48, height @52, format @56, padding @60.
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(b[48..52].try_into().unwrap()), 1024);
        assert_eq!(u32::from_le_bytes(b[52..56].try_into().unwrap()), 768);
        assert_eq!(
            u32::from_le_bytes(b[56..60].try_into().unwrap()),
            VIRTIO_GPU_FORMAT_B8G8R8X8
        );
        // strides @64..80, offsets @80..96 — one plane, the rest empty.
        assert_eq!(u32::from_le_bytes(b[64..68].try_into().unwrap()), 4096);
        assert_eq!(u32::from_le_bytes(b[68..72].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(b[80..84].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(b[92..96].try_into().unwrap()), 0);

        let map = ResourceMapBlob {
            hdr: CtrlHdr::new(CMD_RESOURCE_MAP_BLOB),
            resource_id: 1,
            padding: 0,
            offset: 0x40_0000,
        };
        let b = struct_bytes(&map);
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_RESOURCE_MAP_BLOB
        );
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 1); // resource_id
        assert_eq!(u32::from_le_bytes(b[28..32].try_into().unwrap()), 0); // padding
        assert_eq!(u64::from_le_bytes(b[32..40].try_into().unwrap()), 0x40_0000); // offset
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
    fn drm_node(capsets: &[CapsetInfo], virgl: bool, blob: bool, context_init: bool) -> DrmNode {
        let mut node = DrmNode {
            dev: Device::Owned(VirtioGpuArch::new()),
            capsets: [CapsetInfo::default(); MAX_CAPSETS],
            count: capsets.len(),
            virgl,
            blob,
            has_context_init: context_init,
            res_base: 0,
            context: None,
            context_resources: 0,
            gems: drm::GemTable::new(0),
            gem_arena: 0,
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
        let mut node = drm_node(&[virgl], true, false, true);
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
        let mut both = drm_node(&[virgl, virgl2], true, true, false);
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
        let mut three = drm_node(&[virgl, virgl2, third], true, false, false);
        assert_eq!(
            three.getparam(drm::VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS),
            Ok(0b111)
        );

        // A device without GL has a node all the same, and says so: this is the answer
        // a client has to be able to read rather than finding no device.
        let mut no_gl = drm_node(&[], false, false, false);
        assert_eq!(no_gl.getparam(drm::VIRTGPU_PARAM_3D_FEATURES), Ok(0));
        assert_eq!(
            no_gl.getparam(drm::VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS),
            Ok(0)
        );
        // A node with no capsets at all is "no GL on this host", which the ABI answers
        // `ENOSYS` — not `EINVAL`, which would say the request was malformed (`virtgpu`
        // returns on `num_capsets == 0` before it looks at the id).
        assert_eq!(
            no_gl.get_caps(drm::VIRTGPU_DRM_CAPSET_VIRGL, 1, &mut [0u8; 64]),
            Err(drm::ENOSYS)
        );
    }

    /// PRIME is the capability this node does have, as a bitfield; dumb buffers it does not,
    /// and anything else is refused rather than answered 0, so a caller can tell "not
    /// capable" from "not asked about".
    #[test]
    fn get_cap_reports_prime_and_refuses_the_rest() {
        let mut node = drm_node(&[], true, false, false);
        assert_eq!(node.get_cap(drm::DRM_CAP_DUMB_BUFFER), Ok(0));
        assert_eq!(
            node.get_cap(drm::DRM_CAP_PRIME),
            Ok(drm::DRM_PRIME_CAP_IMPORT | drm::DRM_PRIME_CAP_EXPORT)
        );
        assert_eq!(node.get_cap(0x99), Err(drm::EINVAL));
    }

    /// An object's export token is the name `map_offset` gives it, and importing that token
    /// yields the object's handle back — the round trip that makes the descriptor VFS builds
    /// over a token meaningful. A token the node never issued, or one whose object was
    /// closed, is `ENOENT`, which is what keeps a stale descriptor from naming a slot that
    /// has since been reused.
    #[test]
    fn prime_round_trips_an_objects_own_name_and_refuses_a_stale_token() {
        let mut node = drm_node(&[], true, false, false);
        node.gems = drm::GemTable::new(4096);
        let object = node.gems.create(4096).expect("one page fits the slot");
        let other = node.gems.create(4096).expect("the next slot is free");

        let token = node
            .prime_export(object.handle)
            .expect("a live handle exports");
        assert_eq!(token as u64, node.map_offset(object.handle).unwrap());
        assert_eq!(node.prime_import(token), Ok(object.handle));
        assert_ne!(token as u64, node.map_offset(other.handle).unwrap());

        // A handle the node does not hold, and tokens it never issued: zero, and a free
        // slot's own name.
        assert_eq!(node.prime_export(0x99), Err(drm::ENOENT));
        assert_eq!(node.prime_import(0), Err(drm::ENOENT));
        let free_token = drm::GemTable::offset_of(drm::MAX_OBJECTS - 1) as u32;
        assert_eq!(node.prime_import(free_token), Err(drm::ENOENT));

        // Closing the object takes its token away with it.
        node.gems.remove(object.handle);
        assert_eq!(node.prime_import(token), Err(drm::ENOENT));
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
            true,
            false,
            false,
        );
        let mut out = [0u8; 64];
        assert_eq!(node.get_caps(2, 1, &mut out), Err(drm::EINVAL));
        assert_eq!(node.get_caps(1, 2, &mut out), Err(drm::EINVAL));
    }

    /// The version is a *contract*, not just a name: a non-zero major makes Mesa's virgl winsys
    /// refuse the device outright, and a minor of 1 or more makes it pass fence fds this node
    /// refuses — either is a one-line change that breaks GL with nothing failing on this side to
    /// say so. The name is the kernel driver's, which is what a client logs and some match on, and
    /// the request's protocol is what decides how much of each string a caller gets.
    #[test]
    fn the_version_names_the_driver() {
        let node = drm_node(&[], true, false, false);
        let v = node.version();
        assert_eq!(v.name, "virtio_gpu");
        assert_eq!(v.major, 0, "Mesa refuses a device whose major is not 0");
        assert_eq!(v.minor, 0, "1 means fence fds; see the method's own note");
        assert!(!v.date.is_empty() && !v.desc.is_empty());
    }

    /// The context request: everything a node refuses, and every one of those refusals happens
    /// before any device command, so the answers below are the node's own. The *success* path
    /// needs a device, which is the GL gate's.
    #[test]
    fn context_init_refuses_what_the_node_cannot_serve() {
        let capsets = [CapsetInfo {
            id: 1,
            max_version: 1,
            max_size: 308,
        }];
        let capset = [drm::ContextParam {
            param: drm::VIRTGPU_CONTEXT_PARAM_CAPSET_ID,
            value: 1,
        }];

        // No `VIRTIO_GPU_F_CONTEXT_INIT`: the device has no context command at all.
        let mut node = drm_node(&capsets, true, false, false);
        assert_eq!(node.context_init(&capset), Err(drm::EINVAL));

        // The feature, but no GL: there is no capset for a context to render with.
        let mut node = drm_node(&capsets, false, false, true);
        assert_eq!(node.context_init(&capset), Err(drm::EINVAL));

        // Both features, so the knobs are what is left: a capset the device does not have, more
        // command rings than this node serves, the ring-poll mask (this port delivers no ring
        // events), the `DEBUG_NAME` this version of the host ABI rejects itself, and a knob the
        // ABI does not define.
        let mut node = drm_node(&capsets, true, false, true);
        for bad in [
            drm::ContextParam {
                param: drm::VIRTGPU_CONTEXT_PARAM_CAPSET_ID,
                value: 9,
            },
            drm::ContextParam {
                param: drm::VIRTGPU_CONTEXT_PARAM_NUM_RINGS,
                value: 2,
            },
            drm::ContextParam {
                param: drm::VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK,
                value: 1,
            },
            drm::ContextParam {
                param: drm::VIRTGPU_CONTEXT_PARAM_DEBUG_NAME,
                value: 0,
            },
            drm::ContextParam {
                param: 0x9999,
                value: 0,
            },
        ] {
            assert_eq!(node.context_init(&[bad]), Err(drm::EINVAL), "{bad:?}");
        }

        // The one context a client may have: a second request is `EEXIST`, and that is said
        // before the knobs are looked at — a client that already has one hears *that*.
        let mut taken = drm_node(&capsets, true, false, true);
        taken.context = Some(CONTEXT_ID);
        assert_eq!(taken.context_init(&capset), Err(drm::EEXIST));
        assert_eq!(taken.context_init(&[]), Err(drm::EEXIST));
    }

    /// `RESOURCE_CREATE_BLOB` is the one create that carries its own backing: a 40-byte body and
    /// then the memory entries, at `sizeof(struct virtio_gpu_resource_create_blob)` — which is
    /// where QEMU reads them from, so an entry placed anywhere else is a blob with no pages.
    #[test]
    fn resource_create_blob_layout_puts_the_entries_after_the_body() {
        let cmd = ResourceCreateBlob {
            hdr: CtrlHdr::new(CMD_RESOURCE_CREATE_BLOB),
            resource_id: 7,
            blob_mem: 1,
            blob_flags: 1,
            nr_entries: 1,
            blob_id: 3,
            size: 0x4000,
            entries: [MemEntry {
                addr: 0x1234_5000,
                length: 0x4000,
                padding: 0,
            }],
        };
        let b = struct_bytes(&cmd);
        assert_eq!(b.len(), 72);
        assert_eq!(
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            CMD_RESOURCE_CREATE_BLOB
        );
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 7);
        assert_eq!(
            u32::from_le_bytes(b[28..32].try_into().unwrap()),
            1,
            "blob_mem"
        );
        assert_eq!(
            u32::from_le_bytes(b[32..36].try_into().unwrap()),
            1,
            "blob_flags"
        );
        assert_eq!(
            u32::from_le_bytes(b[36..40].try_into().unwrap()),
            1,
            "nr_entries"
        );
        assert_eq!(
            u64::from_le_bytes(b[40..48].try_into().unwrap()),
            3,
            "blob_id"
        );
        assert_eq!(
            u64::from_le_bytes(b[48..56].try_into().unwrap()),
            0x4000,
            "size"
        );
        assert_eq!(
            u64::from_le_bytes(b[56..64].try_into().unwrap()),
            0x1234_5000
        );
        assert_eq!(u32::from_le_bytes(b[64..68].try_into().unwrap()), 0x4000);
        assert_eq!(
            u32::from_le_bytes(b[68..72].try_into().unwrap()),
            0,
            "padding"
        );
    }

    /// `create_blob`'s answers *before* any device command, so they are the node's own: the feature
    /// gate, then the flags, then the memory kind, then the two fields a guest blob must leave
    /// zero, and finally the table's own bound. The success path needs a device, and is the GL
    /// gate's.
    #[test]
    fn create_blob_refuses_what_the_node_cannot_serve() {
        let guest = |flags: u32| drm::BlobCreate {
            blob_mem: drm::VIRTGPU_BLOB_MEM_GUEST,
            blob_flags: flags,
            size: 4096,
            ..Default::default()
        };

        // No `VIRTIO_GPU_F_RESOURCE_BLOB`: this node has no blob kind at all, whatever the request
        // says — which is the same answer `GETPARAM` already gave for `RESOURCE_BLOB`.
        let mut node = drm_node(&[], true, false, false);
        assert_eq!(node.create_blob(guest(1)), Err(drm::EINVAL));

        let mut node = drm_node(&[], true, true, false);

        // A memory kind the ABI does not define, and the two that need a host window this device
        // does not have.
        for mem in [0, 4] {
            assert_eq!(
                node.create_blob(drm::BlobCreate {
                    blob_mem: mem,
                    ..guest(1)
                }),
                Err(drm::EINVAL),
                "blob_mem {mem}"
            );
        }
        for mem in [
            drm::VIRTGPU_BLOB_MEM_HOST3D,
            drm::VIRTGPU_BLOB_MEM_HOST3D_GUEST,
        ] {
            assert_eq!(
                node.create_blob(drm::BlobCreate {
                    blob_mem: mem,
                    ..guest(1)
                }),
                Err(drm::ENOSYS),
                "blob_mem {mem}"
            );
        }

        // A flag bit the ABI does not define, and the one that needs a UUID command.
        for flags in [0x8, drm::VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE] {
            assert_eq!(
                node.create_blob(guest(flags)),
                Err(drm::EINVAL),
                "{flags:#x}"
            );
        }

        // The two fields that name another object's memory, which a guest blob must leave zero.
        assert_eq!(
            node.create_blob(drm::BlobCreate {
                blob_id: 1,
                ..guest(1)
            }),
            Err(drm::EINVAL)
        );
        assert_eq!(
            node.create_blob(drm::BlobCreate {
                cmd_size: 4,
                ..guest(1)
            }),
            Err(drm::EINVAL)
        );

        // A length no slot could hold. Until a server names the node's backing there are no slots
        // at all, so even a page is more than this one has.
        assert_eq!(node.create_blob(guest(1)), Err(drm::EINVAL));
    }

    /// `RESOURCE_INFO` reports the kind the object was made with — the one field of a blob that no
    /// arithmetic on the others can reconstruct, so it is read off the table rather than assumed.
    #[test]
    fn resource_info_reports_the_kind_the_object_was_made_with() {
        let mut node = drm_node(&[], true, true, false);
        node.gems = drm::GemTable::new(4096);
        let blob = node
            .gems
            .create_blob(4096, drm::VIRTGPU_BLOB_MEM_GUEST)
            .expect("one page fits the slot");
        let plain = node.gems.create(4096).expect("the next slot is free");

        assert_eq!(
            node.resource_info(blob.handle)
                .expect("the blob is in the table")
                .blob_mem,
            drm::VIRTGPU_BLOB_MEM_GUEST
        );
        assert_eq!(
            node.resource_info(plain.handle)
                .expect("the plain object is too")
                .blob_mem,
            0,
            "not a blob"
        );
    }
}
