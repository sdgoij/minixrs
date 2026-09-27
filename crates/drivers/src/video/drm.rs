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
/// `ENOENT` — a handle, capset or resource this node does not hold.
pub const ENOENT: i32 = -2;
/// `EEXIST` — a thing this node has already made and will not make twice, such as the one
/// context a client may have.
pub const EEXIST: i32 = -17;
/// `EFAULT` — a user buffer could not be reached.
pub const EFAULT: i32 = -14;
/// `ENOSPC` — a request that would exceed a fixed capacity. Its own name here because it is
/// not a malformed request: it is a caller closing an object and asking again.
pub const ENOSPC: i32 = -28;

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

/// `VIRTGPU_CONTEXT_PARAM_CAPSET_ID` — which capset the context renders with. The one knob a
/// context cannot be made without: a context is a rendering context *for* a capset.
pub const VIRTGPU_CONTEXT_PARAM_CAPSET_ID: u64 = 0x0001;
/// `VIRTGPU_CONTEXT_PARAM_NUM_RINGS` — how many command rings to make.
pub const VIRTGPU_CONTEXT_PARAM_NUM_RINGS: u64 = 0x0002;
/// `VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK` — which rings deliver completion events.
pub const VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK: u64 = 0x0003;
/// `VIRTGPU_CONTEXT_PARAM_DEBUG_NAME` — Linux names a context after the calling *task*, and
/// this version of the host ABI rejects this parameter rather than reading a name from it
/// (`virtio_gpu_context_init_ioctl` has no case for it).
pub const VIRTGPU_CONTEXT_PARAM_DEBUG_NAME: u64 = 0x0004;

/// How many `drm_virtgpu_context_set_param` entries a context request may carry. Linux caps it
/// at three — the number of knobs it has — before it reads any of them.
pub const MAX_CONTEXT_PARAMS: usize = 3;

/// The most bytes of virgl command buffer one submit may carry.
///
/// Not a policy choice: the buffer travels *after* the submit command in the same virtqueue
/// descriptor (`virtio_gpu.rs`'s `submit_3d`), so the device's command slot is the limit, and a
/// longer buffer has nowhere to go. This is what `dispatch` reads the caller's buffer into, so
/// it is also the most it will copy from one.
pub const MAX_COMMAND_BYTES: usize = 1024;

/// The most handles one submit may name. The node's object table is smaller than this, so a
/// longer list cannot be fully resolved anyway; the bound exists so that `dispatch` can read the
/// array into a fixed buffer rather than trusting the caller's count.
pub const MAX_BO_HANDLES: usize = 32;

/// `VIRTGPU_WAIT_NOWAIT` — report the object's state rather than waiting for it.
pub const VIRTGPU_WAIT_NOWAIT: u32 = 1;

/// `VIRTGPU_EXECBUF_FENCE_FD_IN` / `_OUT` — the submit carries an input fence to wait on, or asks
/// for an output fence fd. This node has no fence objects (`WAYLAND.md` §6.10 D4), so a request
/// that sets either is refused rather than run without the synchronisation it asked for.
pub const VIRTGPU_EXECBUF_FENCE_FD_IN: u32 = 0x01;
/// See [`VIRTGPU_EXECBUF_FENCE_FD_IN`].
pub const VIRTGPU_EXECBUF_FENCE_FD_OUT: u32 = 0x02;
/// `VIRTGPU_EXECBUF_RING_IDX` — the submit names which command ring it goes on. This node serves
/// one ring, so it is refused with the fences rather than ignored.
pub const VIRTGPU_EXECBUF_RING_IDX: u32 = 0x04;
/// The flags this ABI defines, and so the mask a request's flags are checked against.
pub const VIRTGPU_EXECBUF_FLAGS: u32 =
    VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_FENCE_FD_OUT | VIRTGPU_EXECBUF_RING_IDX;

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

    /// `DRM_IOCTL_VIRTGPU_RESOURCE_CREATE`: make a memory object. A request this node
    /// cannot honour — a re-create, a 3D shape on a device without GL, one longer than its
    /// backing — is `EINVAL`, and a full table is `ENOSPC`.
    fn create_object(&mut self, request: ResourceCreate) -> Result<CreatedObject, i32>;

    /// `DRM_IOCTL_VIRTGPU_MAP`: the object's `mmap` offset, or `ENOENT` for a handle this
    /// node does not hold. The offset is a page-granular token a client hands back to
    /// `mmap`; which token names which object is the node's business, as it is the host's.
    fn map_offset(&mut self, handle: u32) -> Result<u64, i32>;

    /// `DRM_IOCTL_VIRTGPU_RESOURCE_INFO`: the object's host resource, its length and its
    /// blob kind (0 for a plain object).
    fn resource_info(&mut self, handle: u32) -> Result<ResourceInfo, i32>;

    /// `DRM_IOCTL_GEM_CLOSE`: drop the object and release the host resource behind it.
    /// Dropping a handle twice, or one this node never issued, is `ENOENT` — the ABI has no
    /// "already closed".
    fn close_object(&mut self, handle: u32) -> Result<(), i32>;

    /// `DRM_IOCTL_VIRTGPU_CONTEXT_INIT`: make the one rendering context a client may have, from
    /// the `drm_virtgpu_context_set_param` array [`dispatch`] read on its behalf. A second call
    /// is `EEXIST`, a device with no GL or no context support is `EINVAL`, and so is a parameter
    /// this node cannot honour.
    fn context_init(&mut self, params: &[ContextParam]) -> Result<(), i32>;

    /// `DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST` / `TRANSFER_FROM_HOST`: move a box of an object's
    /// contents between the guest's pages and the host's copy of the resource.
    fn transfer(&mut self, to_host: bool, request: &Transfer3d) -> Result<(), i32>;

    /// `DRM_IOCTL_VIRTGPU_EXECBUFFER`: hand a context a virgl command buffer, with the objects it
    /// names made visible to it. `handles` are GEM handles; a node that does not hold one is
    /// `ENOENT`, and one whose device has no 3D is `ENOSYS`.
    fn submit(&mut self, command: &[u8], handles: &[u32]) -> Result<(), i32>;

    /// `DRM_IOCTL_VIRTGPU_WAIT`: wait for an object's work to finish, or with `nowait` report
    /// whether it has. An unknown handle is `ENOENT`.
    fn wait(&mut self, handle: u32, nowait: bool) -> Result<(), i32>;
}

/// `struct drm_virtgpu_3d_transfer_to_host` (and `_from_host`, which is the same shape): one box
/// of one object, with the strides the host reads it with. Every field but the resource and the
/// box is zero for a whole-resource transfer, which is what the boot probe does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Transfer3d {
    pub resource_id: u32,
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
    pub level: u32,
    pub offset: u32,
    pub stride: u32,
    pub layer_stride: u32,
}

impl Transfer3d {
    /// The whole of one object: the box the boot probe transfers.
    pub const fn whole(resource_id: u32, w: u32, h: u32) -> Self {
        Self {
            resource_id,
            x: 0,
            y: 0,
            z: 0,
            w,
            h,
            d: 1,
            level: 0,
            offset: 0,
            stride: 0,
            layer_stride: 0,
        }
    }
}

/// The most objects one node may hold at once. A node's backing is a fixed arena, so this
/// count is fixed with it.
pub const MAX_OBJECTS: usize = 8;

/// `struct drm_virtgpu_resource_create`, as [`dispatch`] decoded it.
///
/// `bo_handle` is carried because a client may set it (Linux: "recreate a new resource
/// attached to this bo") and a node must be able to refuse that rather than silently make
/// a second object. `size` is the caller's own byte count, defaulting to one page — this
/// ABI does not derive it from the geometry, which is why a client that knows its stride
/// passes both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceCreate {
    pub target: u32,
    pub format: u32,
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub nr_samples: u32,
    pub flags: u32,
    pub bo_handle: u32,
    pub size: u32,
    pub stride: u32,
}

/// What a `RESOURCE_CREATE` produced: the GEM handle a client names the object by, the host
/// resource it is attached to, and how many bytes it holds (the request's size rounded up
/// to a whole page, which is what a GEM object's length always is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreatedObject {
    pub handle: u32,
    pub res_handle: u32,
    pub size: u32,
}

/// The answer to a `RESOURCE_INFO`: the object's host resource, its length, and its blob
/// kind (0 for a plain object).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceInfo {
    pub res_handle: u32,
    pub size: u32,
    pub blob_mem: u32,
}

/// The objects one node hands out.
///
/// Slot `i` owns `[i * max_len, (i + 1) * max_len)` of whatever backing the node put behind
/// it, and its `mmap` offset is `(i + 1) << 12` — one page number per slot, so two objects
/// never share a token and offset 0 never names one. The table holds no *addresses*: which
/// memory a slot names is the node's, which is what keeps this testable without a device.
///
/// Handles and resource ids are both `slot + 1`. The ABI has two ids because Linux's GEM
/// handles are per open file and its resource ids per device; this node is one allocator, so
/// numbering them alike is the honest simplification, not an accident. A client treats both
/// as opaque, and only the resource id ever reaches the host.
#[derive(Debug, Clone, Copy)]
pub struct GemTable {
    max_len: u32,
    /// The length of the object in each slot, or `None` for a free one.
    objects: [Option<u32>; MAX_OBJECTS],
}

impl GemTable {
    /// A table of [`MAX_OBJECTS`] objects, none longer than `max_len` bytes.
    pub const fn new(max_len: u32) -> Self {
        Self {
            max_len,
            objects: [None; MAX_OBJECTS],
        }
    }

    /// The object in `slot`, if it is in use.
    fn len_of(&self, slot: usize) -> Option<u32> {
        self.objects.get(slot).copied().flatten()
    }

    /// How much backing one slot has: the longest object this table can hold.
    pub const fn slot_len(&self) -> u32 {
        self.max_len
    }

    /// The `mmap` offset of the object in `slot`.
    pub const fn offset_of(slot: usize) -> u64 {
        ((slot as u64) + 1) << 12
    }

    /// The slot an `mmap` offset names, or `None` for one this table never issued.
    pub fn slot_of_offset(offset: u64) -> Option<usize> {
        let slot = (offset >> 12) as usize;
        (1..=MAX_OBJECTS).contains(&slot).then(|| slot - 1)
    }

    /// The length of the object an `mmap` offset names, or `None` for a token no live object
    /// holds — an object that was closed stops naming its old offset, which is what makes a
    /// mapping of a closed object impossible rather than merely unwise.
    pub fn len_at_offset(&self, offset: u64) -> Option<u32> {
        self.len_of(Self::slot_of_offset(offset)?)
    }

    /// Make an object of `bytes` (one page when the caller asked for none), or `EINVAL` when
    /// it is longer than the backing, or `ENOSPC` when every slot is taken.
    pub fn create(&mut self, bytes: u32) -> Result<CreatedObject, i32> {
        const PAGE: u32 = 4096;
        // The *rounded* length is what a slot has to have room for, and what the object's
        // `size` becomes: a GEM object's length is always a whole number of pages, and a
        // request one byte over a page boundary is a page longer than it looks.
        let Some(len) = bytes.max(1).div_ceil(PAGE).checked_mul(PAGE) else {
            // `u32::MAX` rounds up out of the type. A request that large is not a request
            // this node can answer at all, so it is refused rather than wrapped to a length
            // that would fit.
            return Err(EINVAL);
        };
        if len > self.max_len {
            return Err(EINVAL);
        }
        let Some(slot) = self.objects.iter().position(|o| o.is_none()) else {
            return Err(ENOSPC);
        };
        self.objects[slot] = Some(len);
        let id = slot as u32 + 1;
        Ok(CreatedObject {
            handle: id,
            res_handle: id,
            size: len,
        })
    }

    /// The slot and length behind `handle`, or `None`.
    pub fn lookup(&self, handle: u32) -> Option<(usize, u32)> {
        let slot = (handle as usize).checked_sub(1)?;
        self.len_of(slot).map(|len| (slot, len))
    }

    /// Release `handle`, returning its slot and id, or `None` for a handle not held.
    pub fn remove(&mut self, handle: u32) -> Option<(usize, u32)> {
        let (slot, _) = self.lookup(handle)?;
        self.objects[slot] = None;
        Some((slot, handle))
    }
}

/// The pointed-to buffers a DRM argument struct carries.
///
/// The structs hold *user* addresses (`drm_version.name`, `drm_virtgpu_getparam.value`,
/// `drm_virtgpu_get_caps.addr`, `drm_virtgpu_context_init.ctx_set_params`), which a real node
/// reaches with `safecopy_from`/`safecopy_to`. Keeping that behind a trait is what lets the
/// decoders be tested without a user process, and it is also the only place a request can fail
/// with `EFAULT`.
pub trait UserBuffers {
    /// Copy `src` to the user address `addr`.
    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), i32>;

    /// Copy from the user address `addr` into `dst`.
    fn read(&mut self, addr: u64, dst: &mut [u8]) -> Result<(), i32>;
}

/// One `struct drm_virtgpu_context_set_param`: which knob, and what to set it to. Both are
/// `u64` in the struct, not the pair of `u32`s the shape suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextParam {
    pub param: u64,
    pub value: u64,
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
        DRM_IOCTL_VIRTGPU_RESOURCE_CREATE => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let create = ResourceCreate {
                target: rd_u32(arg, 0),
                format: rd_u32(arg, 4),
                bind: rd_u32(arg, 8),
                width: rd_u32(arg, 12),
                height: rd_u32(arg, 16),
                depth: rd_u32(arg, 20),
                array_size: rd_u32(arg, 24),
                last_level: rd_u32(arg, 28),
                nr_samples: rd_u32(arg, 32),
                flags: rd_u32(arg, 36),
                bo_handle: rd_u32(arg, 40),
                size: rd_u32(arg, 48),
                stride: rd_u32(arg, 52),
            };
            match node.create_object(create) {
                Ok(o) => {
                    wr_u32(arg, 40, o.handle);
                    wr_u32(arg, 44, o.res_handle);
                    // The object's length, which is the request's rounded up to a page. The
                    // host leaves this field as the caller set it; reporting what was
                    // actually allocated is the one number a client cannot work out itself,
                    // and it is what `RESOURCE_INFO` reports for the same object.
                    wr_u32(arg, 48, o.size);
                    0
                }
                Err(e) => e,
            }
        }
        DRM_IOCTL_VIRTGPU_MAP => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let handle = rd_u32(arg, 8);
            match node.map_offset(handle) {
                Ok(offset) => {
                    wr_u64(arg, 0, offset);
                    0
                }
                Err(e) => e,
            }
        }
        DRM_IOCTL_VIRTGPU_RESOURCE_INFO => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let handle = rd_u32(arg, 0);
            match node.resource_info(handle) {
                Ok(i) => {
                    wr_u32(arg, 4, i.res_handle);
                    wr_u32(arg, 8, i.size);
                    wr_u32(arg, 12, i.blob_mem);
                    0
                }
                Err(e) => e,
            }
        }
        DRM_IOCTL_GEM_CLOSE => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let handle = rd_u32(arg, 0);
            match node.close_object(handle) {
                Ok(()) => 0,
                Err(e) => e,
            }
        }
        DRM_IOCTL_VIRTGPU_CONTEXT_INIT => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let count = rd_u32(arg, 0) as usize;
            let addr = rd_u64(arg, 8);
            // Linux refuses an over-long list before it reads any of it, so a caller that sent
            // more knobs than the ABI has hears about its request and not about its pointer.
            if count > MAX_CONTEXT_PARAMS {
                return EINVAL;
            }
            let mut raw = [0u8; MAX_CONTEXT_PARAMS * 16];
            let bytes = &mut raw[..count * 16];
            if !bytes.is_empty() {
                // The array travels by pointer (`memdup_user` on the host), so a null one is a
                // request that named nothing rather than an empty context.
                if addr == 0 {
                    return EFAULT;
                }
                if let Err(e) = users.read(addr, bytes) {
                    return e;
                }
            }
            let mut params = [ContextParam::default(); MAX_CONTEXT_PARAMS];
            for (i, p) in params.iter_mut().take(count).enumerate() {
                p.param = rd_u64(&raw, i * 16);
                p.value = rd_u64(&raw, i * 16 + 8);
            }
            match node.context_init(&params[..count]) {
                Ok(()) => 0,
                Err(e) => e,
            }
        }
        DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST | DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let transfer = Transfer3d {
                resource_id: rd_u32(arg, 0),
                x: rd_u32(arg, 4),
                y: rd_u32(arg, 8),
                z: rd_u32(arg, 12),
                w: rd_u32(arg, 16),
                h: rd_u32(arg, 20),
                d: rd_u32(arg, 24),
                level: rd_u32(arg, 28),
                offset: rd_u32(arg, 32),
                stride: rd_u32(arg, 36),
                layer_stride: rd_u32(arg, 40),
            };
            match node.transfer(request == DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST, &transfer) {
                Ok(()) => 0,
                Err(e) => e,
            }
        }
        DRM_IOCTL_VIRTGPU_EXECBUFFER => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let flags = rd_u32(arg, 0);
            let size = rd_u32(arg, 4) as usize;
            let command = rd_u64(arg, 8);
            let handle_addr = rd_u64(arg, 16);
            let count = rd_u32(arg, 24) as usize;
            let fence_fd = rd_u32(arg, 28) as i32;
            // A flag this ABI does not define is a request this node cannot read, and one of the
            // three it does define is synchronisation this node does not have: a submit that
            // asked for either is refused rather than run without it.
            if flags & !VIRTGPU_EXECBUF_FLAGS != 0 {
                return EINVAL;
            }
            if flags & VIRTGPU_EXECBUF_FLAGS != 0 || fence_fd != -1 {
                return ENOSYS;
            }
            if rd_u32(arg, 36) != 0
                || rd_u32(arg, 40) != 0
                || rd_u32(arg, 44) != 0
                || rd_u64(arg, 48) != 0
                || rd_u64(arg, 56) != 0
            {
                return EINVAL;
            }
            // A command buffer is whole dwords, and it has to fit the device's command slot: the
            // host reads it out of the same descriptor the submit command travels in.
            if size == 0 || !size.is_multiple_of(4) || size > MAX_COMMAND_BYTES {
                return EINVAL;
            }
            if count > MAX_BO_HANDLES {
                return EINVAL;
            }
            if command == 0 {
                return EFAULT;
            }
            let mut words = [0u8; MAX_COMMAND_BYTES];
            if let Err(e) = users.read(command, &mut words[..size]) {
                return e;
            }
            let mut handles = [0u32; MAX_BO_HANDLES];
            if count > 0 {
                let mut raw = [0u8; MAX_BO_HANDLES * 4];
                if handle_addr == 0 {
                    return EFAULT;
                }
                if let Err(e) = users.read(handle_addr, &mut raw[..count * 4]) {
                    return e;
                }
                for (i, h) in handles.iter_mut().take(count).enumerate() {
                    *h = rd_u32(&raw, i * 4);
                }
            }
            match node.submit(&words[..size], &handles[..count]) {
                Ok(()) => 0,
                Err(e) => e,
            }
        }
        DRM_IOCTL_VIRTGPU_WAIT => {
            if !fits(arg, request) {
                return EINVAL;
            }
            let handle = rd_u32(arg, 0);
            let flags = rd_u32(arg, 4);
            if flags & !VIRTGPU_WAIT_NOWAIT != 0 {
                return EINVAL;
            }
            match node.wait(handle, flags & VIRTGPU_WAIT_NOWAIT != 0) {
                Ok(()) => 0,
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

    // `FakeNode`'s one object: handle 7 / resource 9 / offset 0x7000, so what each request
    // writes back is pinned without a device. A re-create is refused, as the real node does.
    const OBJECT: u32 = 7;
    const RESOURCE: u32 = 9;
    const LEN: u32 = 4096;

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

        fn create_object(&mut self, request: ResourceCreate) -> Result<CreatedObject, i32> {
            if request.bo_handle != 0 {
                return Err(EINVAL);
            }
            Ok(CreatedObject {
                handle: OBJECT,
                res_handle: RESOURCE,
                size: LEN,
            })
        }
        fn map_offset(&mut self, handle: u32) -> Result<u64, i32> {
            if handle != OBJECT {
                return Err(ENOENT);
            }
            Ok(0x7000)
        }
        fn resource_info(&mut self, handle: u32) -> Result<ResourceInfo, i32> {
            if handle != OBJECT {
                return Err(ENOENT);
            }
            Ok(ResourceInfo {
                res_handle: RESOURCE,
                size: LEN,
                blob_mem: 0,
            })
        }
        fn close_object(&mut self, handle: u32) -> Result<(), i32> {
            if handle != OBJECT {
                return Err(ENOENT);
            }
            Ok(())
        }

        /// A capset knob naming capset 2, optionally preceded by a `NUM_RINGS` of one — the only
        /// shapes the context tests pass. A mis-decoded array is then an *error here* rather than
        /// a context accepted with the wrong capset, which is what makes the offsets in the array
        /// this method's business and the test's subject. Capset 2 rather than 1 because the
        /// knob's own id and capset 1's are the same number: a swapped pair would then be
        /// indistinguishable from a correct one.
        fn context_init(&mut self, params: &[ContextParam]) -> Result<(), i32> {
            let capset = |p: &ContextParam| {
                p.param == VIRTGPU_CONTEXT_PARAM_CAPSET_ID
                    && p.value == VIRTGPU_DRM_CAPSET_VIRGL2 as u64
            };
            let rings = |p: &ContextParam| {
                p.param == VIRTGPU_CONTEXT_PARAM_NUM_RINGS && p.value == 1
            };
            match params {
                [only] if capset(only) => Ok(()),
                [first, second] if rings(first) && capset(second) => Ok(()),
                _ => Err(EINVAL),
            }
        }

        /// The whole of object 7 as a 64×64×1 box — the shape the transfer tests build, so a
        /// field read from the wrong offset is an error here rather than a transfer of the wrong
        /// region.
        fn transfer(&mut self, _to_host: bool, request: &Transfer3d) -> Result<(), i32> {
            if request.resource_id != OBJECT
                || request.x != 0
                || request.w != 64
                || request.h != 64
                || request.d != 1
            {
                return Err(EINVAL);
            }
            Ok(())
        }

        /// Eight bytes of command naming object 7 — the shape the submit tests build.
        fn submit(&mut self, command: &[u8], handles: &[u32]) -> Result<(), i32> {
            if command.len() != 8 || handles != [OBJECT] {
                return Err(EINVAL);
            }
            Ok(())
        }

        fn wait(&mut self, handle: u32, _nowait: bool) -> Result<(), i32> {
            if handle != OBJECT {
                return Err(ENOENT);
            }
            Ok(())
        }
    }

    /// A `struct drm_virtgpu_resource_create`: geometry, the caller's own byte count, and
    /// zeroes where the request will write the handles back.
    fn resource_create_arg(size: u32, stride: u32) -> [u8; 56] {
        let mut arg = [0u8; 56];
        wr_u32(&mut arg, 0, 2); // target: PIPE_TEXTURE_2D
        wr_u32(&mut arg, 4, 2); // format: B8G8R8X8
        wr_u32(&mut arg, 8, 1 << 3); // bind: RENDER_TARGET
        wr_u32(&mut arg, 12, 64); // width
        wr_u32(&mut arg, 16, 64); // height
        wr_u32(&mut arg, 20, 1); // depth
        wr_u32(&mut arg, 24, 1); // array_size
        wr_u32(&mut arg, 28, 0); // last_level
        wr_u32(&mut arg, 32, 0); // nr_samples
        wr_u32(&mut arg, 48, size);
        wr_u32(&mut arg, 52, stride);
        arg
    }

    /// A `struct drm_virtgpu_resource_info`: the handle in, the object's numbers out.
    fn resource_info_arg(handle: u32) -> [u8; 16] {
        let mut arg = [0u8; 16];
        wr_u32(&mut arg, 0, handle);
        arg
    }

    /// A `struct drm_virtgpu_context_init`: how many knob entries, and where the array is.
    fn context_init_arg(count: u32, addr: u64) -> [u8; 16] {
        let mut arg = [0u8; 16];
        wr_u32(&mut arg, 0, count);
        wr_u64(&mut arg, 8, addr);
        arg
    }

    /// `drm_virtgpu_context_set_param`: the knob, then its value — two `u64`s, not a `u32` pair.
    fn ctx_param(param: u64, value: u64) -> [u8; 16] {
        let mut p = [0u8; 16];
        wr_u64(&mut p, 0, param);
        wr_u64(&mut p, 8, value);
        p
    }

    /// A `drm_virtgpu_3d_transfer_to_host`: the resource, a box, and the strides to read it with.
    fn transfer_arg(resource_id: u32, w: u32, h: u32) -> [u8; 44] {
        let mut arg = [0u8; 44];
        wr_u32(&mut arg, 0, resource_id);
        wr_u32(&mut arg, 16, w);
        wr_u32(&mut arg, 20, h);
        wr_u32(&mut arg, 24, 1);
        arg
    }

    /// A `drm_virtgpu_execbuffer`: the flags, the command buffer and its length, and the handles.
    fn execbuffer_arg(flags: u32, size: u32, command: u64, handles: u64, count: u32) -> [u8; 64] {
        let mut arg = [0u8; 64];
        wr_u32(&mut arg, 0, flags);
        wr_u32(&mut arg, 4, size);
        wr_u64(&mut arg, 8, command);
        wr_u64(&mut arg, 16, handles);
        wr_u32(&mut arg, 24, count);
        // `fence_fd` is `-1` for "none", which is what every other field of the request is 0 for.
        wr_u32(&mut arg, 28, u32::MAX);
        arg
    }

    /// A `drm_virtgpu_3d_wait`.
    fn wait_arg(handle: u32, flags: u32) -> [u8; 8] {
        let mut arg = [0u8; 8];
        wr_u32(&mut arg, 0, handle);
        wr_u32(&mut arg, 4, flags);
        arg
    }

    /// A `struct drm_virtgpu_map`: the handle in, the offset out.
    fn map_arg(handle: u32) -> [u8; 16] {
        let mut arg = [0u8; 16];
        wr_u32(&mut arg, 8, handle);
        arg
    }

    /// A `struct drm_gem_close`: the handle in and nothing else.
    fn gem_close_arg(handle: u32) -> [u8; 8] {
        let mut arg = [0u8; 8];
        wr_u32(&mut arg, 0, handle);
        arg
    }

    /// `RESOURCE_CREATE` writes the handle, the resource id and the object's length into the
    /// fields the kernel writes them into — and leaves `stride`, which is the caller's.
    #[test]
    fn resource_create_writes_the_handles_and_the_object_length() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = resource_create_arg(16384, 256);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_RESOURCE_CREATE,
                &mut arg
            ),
            0
        );
        assert_eq!(rd_u32(&arg, 40), 7, "bo_handle at 40");
        assert_eq!(rd_u32(&arg, 44), 9, "res_handle at 44");
        assert_eq!(rd_u32(&arg, 48), 4096, "size at 48");
        assert_eq!(rd_u32(&arg, 52), 256, "stride is the caller's");
        assert_eq!(users.writes(), 0, "nothing is copied through a pointer");

        // A re-create is refused, and the refusal is the node's to make.
        let mut arg = resource_create_arg(4096, 0);
        wr_u32(&mut arg, 40, 3);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_RESOURCE_CREATE,
                &mut arg
            ),
            EINVAL
        );
    }

    /// `MAP` writes the offset and nothing else; an unknown handle is `ENOENT`.
    #[test]
    fn map_writes_the_offset_or_refuses_an_unknown_handle() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = map_arg(7);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_MAP, &mut arg),
            0
        );
        assert_eq!(rd_u64(&arg, 0), 0x7000);
        assert_eq!(rd_u32(&arg, 8), 7, "the handle is an in field");

        let mut arg = map_arg(8);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_MAP, &mut arg),
            ENOENT
        );
    }

    /// `RESOURCE_INFO` writes the resource id, the length and the blob kind.
    #[test]
    fn resource_info_writes_the_object_numbers() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = resource_info_arg(7);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_RESOURCE_INFO,
                &mut arg
            ),
            0
        );
        assert_eq!(rd_u32(&arg, 4), 9);
        assert_eq!(rd_u32(&arg, 8), 4096);
        assert_eq!(rd_u32(&arg, 12), 0, "not a blob");

        let mut arg = resource_info_arg(1);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_RESOURCE_INFO,
                &mut arg
            ),
            ENOENT
        );
    }

    /// `GEM_CLOSE` is `_IOW`: it writes nothing back, and closing a handle twice is `ENOENT`
    /// rather than a no-op.
    #[test]
    fn gem_close_writes_nothing_and_refuses_an_unknown_handle() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let mut arg = gem_close_arg(7);
        let before = arg;
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_GEM_CLOSE, &mut arg),
            0
        );
        assert_eq!(arg, before, "_IOW carries nothing out");

        let mut arg = gem_close_arg(1);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_GEM_CLOSE, &mut arg),
            ENOENT
        );
    }

    /// The context request reads its parameter array through a user pointer, and the node is
    /// handed what that read produced — so the array's layout is the subject, and the fake node
    /// accepts only the shapes these tests build.
    #[test]
    fn context_init_reads_the_parameter_array_through_the_pointer() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();

        users.seed(0x6000, &ctx_param(VIRTGPU_CONTEXT_PARAM_CAPSET_ID, 2));
        let mut arg = context_init_arg(1, 0x6000);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &mut arg
            ),
            0
        );

        // Entries are 16 bytes apart, so the second is read past the first: two entries are
        // read where one would have taken the first sixteen bytes twice.
        let mut two = [0u8; 32];
        two[..16].copy_from_slice(&ctx_param(VIRTGPU_CONTEXT_PARAM_NUM_RINGS, 1));
        two[16..].copy_from_slice(&ctx_param(VIRTGPU_CONTEXT_PARAM_CAPSET_ID, 2));
        users.seed(0x6100, &two);
        let mut arg = context_init_arg(2, 0x6100);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &mut arg
            ),
            0
        );

        // `param` and `value` are not interchangeable: the entry above with its two `u64`s
        // swapped names the `NUM_RINGS` knob — which the ABI reads as *rings* 2, not as a
        // capset — so an array that is one swapped entry is refused. Were the decoder swapping
        // the pair itself, this would be the valid array above and answer 0.
        users.seed(0x6200, &ctx_param(2, VIRTGPU_CONTEXT_PARAM_CAPSET_ID));
        let mut arg = context_init_arg(1, 0x6200);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &mut arg
            ),
            EINVAL
        );
    }

    /// What a context request that cannot be read is: more knobs than the ABI has, a pointer
    /// that names nothing, an array this side cannot reach, and no knobs at all — which the read
    /// skips, so the answer is the node's rather than the copy's.
    #[test]
    fn context_init_refuses_what_it_cannot_read() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();

        let mut arg = context_init_arg(MAX_CONTEXT_PARAMS as u32 + 1, 0x6000);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &mut arg
            ),
            EINVAL
        );

        let mut arg = context_init_arg(1, 0);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &mut arg
            ),
            EFAULT
        );

        // Nothing seeded at this address, which is what a pointer into a client's unmapped
        // memory looks like from here.
        let mut arg = context_init_arg(1, 0x7700);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &mut arg
            ),
            EFAULT
        );

        let mut arg = context_init_arg(0, 0);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &mut arg
            ),
            EINVAL
        );

        // A short argument is refused before any field is read, as every arm does.
        let mut arg = [0u8; 8];
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
                &mut arg
            ),
            EINVAL
        );
    }

    /// The transfer's box: the resource at 0, and `x y z w h d` at 4, 8, 12, 16, 20 and 24, with
    /// the level and strides after them. The fake node only accepts one box, so a field read from
    /// the wrong offset is a refusal rather than a transfer of the wrong region.
    #[test]
    fn transfer_decodes_the_resource_and_the_box() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();

        for request in [
            DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST,
            DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST,
        ] {
            let mut arg = transfer_arg(7, 64, 64);
            assert_eq!(dispatch(&mut node, &mut users, request, &mut arg), 0);
        }

        // A `w` where `h` belongs is a different box, and the node refuses it.
        let mut arg = transfer_arg(7, 64, 32);
        assert_eq!(
            dispatch(
                &mut node,
                &mut users,
                DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST,
                &mut arg
            ),
            EINVAL
        );
    }

    /// A submit reads its command buffer and its handle array through pointers, both
    /// caller-sized, and refuses the requests it cannot run: one that names synchronisation this
    /// node does not have, one whose buffer is not whole dwords or is longer than the device's
    /// command slot, one that names nothing, and one naming more handles than it can hold.
    #[test]
    fn execbuffer_reads_its_command_and_handles_and_refuses_the_rest() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();
        let command = [0xAAu8; 8];
        users.seed(0x4000, &command);
        let handle = 7u32.to_le_bytes();
        users.seed(0x5000, &handle);

        let mut arg = execbuffer_arg(0, 8, 0x4000, 0x5000, 1);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
            0
        );

        // A buffer that is not whole dwords, an empty one, and one longer than the slot the
        // command travels in.
        for size in [7u32, 0, MAX_COMMAND_BYTES as u32 + 4] {
            let mut arg = execbuffer_arg(0, size, 0x4000, 0x5000, 1);
            assert_eq!(
                dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
                EINVAL,
                "size {size}"
            );
        }

        // A flag the ABI does not define, and one it does that asks for a fence or a ring this
        // node does not have. The second is `ENOSYS` and not `EINVAL`: the request is well formed
        // and the node simply cannot do it.
        let mut arg = execbuffer_arg(0x80, 8, 0x4000, 0x5000, 1);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
            EINVAL
        );
        let mut arg = execbuffer_arg(VIRTGPU_EXECBUF_FENCE_FD_OUT, 8, 0x4000, 0x5000, 1);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
            ENOSYS
        );

        // Sync objects, which this node implements none of.
        let mut arg = execbuffer_arg(0, 8, 0x4000, 0x5000, 1);
        wr_u32(&mut arg, 40, 1);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
            EINVAL
        );

        // Pointers that name nothing, and more handles than the node will read.
        let mut arg = execbuffer_arg(0, 8, 0, 0x5000, 1);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
            EFAULT
        );
        let mut arg = execbuffer_arg(0, 8, 0x4000, 0, 1);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
            EFAULT
        );
        let mut arg = execbuffer_arg(0, 8, 0x4000, 0x5000, MAX_BO_HANDLES as u32 + 1);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
            EINVAL
        );

        // No handles at all is a submit with nothing attached, which the node decides on.
        let mut arg = execbuffer_arg(0, 8, 0x4000, 0, 0);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_EXECBUFFER, &mut arg),
            EINVAL,
            "the fake wants the one handle"
        );
    }

    /// `WAIT` takes a handle and the `NOWAIT` flag, and nothing else; an unknown handle is
    /// `ENOENT` and an undefined flag is `EINVAL`.
    #[test]
    fn wait_takes_a_handle_and_the_nowait_flag() {
        let mut node = FakeNode;
        let mut users = FakeUser::new();

        for flags in [0, VIRTGPU_WAIT_NOWAIT] {
            let mut arg = wait_arg(7, flags);
            assert_eq!(
                dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_WAIT, &mut arg),
                0
            );
        }

        let mut arg = wait_arg(7, 0x80);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_WAIT, &mut arg),
            EINVAL
        );
        let mut arg = wait_arg(9, 0);
        assert_eq!(
            dispatch(&mut node, &mut users, DRM_IOCTL_VIRTGPU_WAIT, &mut arg),
            ENOENT
        );
    }

    /// The object table: lengths round up to a page, a closed handle's slot is reused, and an
    /// offset stops naming its object the moment that object is closed.
    #[test]
    fn the_object_table_rounds_lengths_and_frees_slots() {
        let mut objects = GemTable::new(64 * 1024);

        let first = objects.create(1).expect("one byte is one page");
        assert_eq!((first.handle, first.res_handle), (1, 1));
        assert_eq!(first.size, 4096);
        let second = objects.create(4097).expect("just over a page is two");
        assert_eq!(second.size, 8192);
        assert_eq!(second.handle, 2);

        // Offsets are one page number per slot, and a slot's own offset names its object.
        assert_eq!(GemTable::offset_of(0), 0x1000);
        assert_eq!(GemTable::offset_of(1), 0x2000);
        assert_eq!(GemTable::slot_of_offset(0x1000), Some(0));
        assert_eq!(objects.len_at_offset(0x2000), Some(8192));
        // Offset 0, a page past the last slot, and a non-page token name nothing.
        assert_eq!(GemTable::slot_of_offset(0), None);
        assert_eq!(objects.len_at_offset(0x9000), None);
        // A token that is not page-aligned still names its slot's page.
        assert_eq!(GemTable::slot_of_offset(0x2800), Some(1));

        assert_eq!(objects.lookup(2), Some((1, 8192)));
        assert_eq!(objects.remove(2), Some((1, 2)));
        assert_eq!(objects.lookup(2), None, "a closed handle is gone");
        assert_eq!(objects.remove(2), None, "and closing it twice is not an answer");
        // The freed slot is the one reused, and its offset names the new object.
        let third = objects.create(4096).expect("the slot is free again");
        assert_eq!(third.handle, 2);
        assert_eq!(objects.len_at_offset(0x2000), Some(4096));
    }

    /// The table refuses what it cannot hold: too long for a slot, no slot left, and a length
    /// that would round up out of the type.
    #[test]
    fn the_object_table_refuses_more_than_its_backing() {
        let mut objects = GemTable::new(8192);
        assert_eq!(
            objects.create(8193),
            Err(EINVAL),
            "longer than the slot's backing"
        );
        // The bound is on the *rounded* length: a backing of 5000 has room for one page and
        // not for two, whatever the request says.
        let mut odd = GemTable::new(5000);
        assert_eq!(odd.create(4096).map(|o| o.size), Ok(4096));
        let mut odd = GemTable::new(5000);
        assert_eq!(odd.create(4097), Err(EINVAL));

        assert_eq!(objects.create(8192).map(|o| o.size), Ok(8192));
        // A length that rounds up past `u32::MAX` is refused rather than wrapped to one that
        // would fit — a wrapped length is an object a client asked for and did not get.
        let mut huge = GemTable::new(8192);
        assert_eq!(huge.create(u32::MAX), Err(EINVAL));

        let mut full = GemTable::new(4096);
        for _ in 0..MAX_OBJECTS {
            assert!(full.create(4096).is_ok());
        }
        assert_eq!(full.create(4096), Err(ENOSPC));
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

        /// Put `bytes` at `addr` as if the client's own array were there, so a request that
        /// *reads* through a pointer field has something to find.
        fn seed(&mut self, addr: u64, bytes: &[u8]) {
            if self.write(addr, bytes).is_err() {
                unreachable!("the fake only fails when a test asked it to");
            }
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

        fn read(&mut self, addr: u64, dst: &mut [u8]) -> Result<(), i32> {
            if self.fail {
                return Err(EFAULT);
            }
            let Some((_, buf, n)) = self.slots[..self.count]
                .iter()
                .find(|(a, _, _)| *a == addr)
            else {
                return Err(EFAULT);
            };
            let n = (*n).min(dst.len());
            dst[..n].copy_from_slice(&buf[..n]);
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
            DRM_IOCTL_VIRTGPU_CONTEXT_INIT,
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
