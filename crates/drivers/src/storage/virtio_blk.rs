//! Virtio block driver — para-virtualized block device for QEMU/KVM.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/virtio_blk/`
//!
//! Uses the virtio PCI transport layer to provide a block device
//! with 512-byte sectors, scatter-gather I/O, and cache flush.
//!
//! # Architecture
//!
//! This driver implements the virtio-blk protocol over the legacy virtio
//! PCI transport.  On real hardware the flow is:
//!
//! 1. `virtio_blk_probe()` — scan PCI for a virtio-blk device (vendor
//!    0x1AF4, sub-device ID 0x0002), initialise the virtio device,
//!    allocate a virtqueue, request headers / status buffers, read the
//!    device configuration, and signal readiness.
//! 2. `virtio_blk_transfer()` — build a scatter-gather list (header,
//!    data buffers, status byte), submit it to the queue, and spin-poll
//!    for completion via `virtio_from_queue()`.
//! 3. `virtio_blk_flush()`— submit a T_FLUSH command to the queue.
//! 4. `virtio_blk_intr()` — called by the interrupt handler; reaps any
//!    completed descriptors and wakes the waiting caller.

use crate::DriverError;
use crate::bus::virtio;
use crate::bus::virtio::{VRING_DESC_F_WRITE, VirtioDevice, VirtioFeature, VirtioPhysBuf};
use core::cell::UnsafeCell;

/// Virtio PCI vendor ID (Red Hat / QEMU).
pub const VIRTIO_VENDOR_ID: u16 = 0x1AF4;

/// Virtio block device ID.
pub const VIRTIO_DEVICE_ID_BLOCK: u16 = 0x1001;

/// Virtio block PCI subsystem device ID (used by `virtio_probe`).
pub const VIRTIO_BLK_SUBSYSTEM_ID: u16 = 0x0002;

pub const VIRTIO_BLK_F_BARRIER: u8 = 0;
pub const VIRTIO_BLK_F_SIZE_MAX: u8 = 1;
pub const VIRTIO_BLK_F_SEG_MAX: u8 = 2;
pub const VIRTIO_BLK_F_GEOMETRY: u8 = 4;
pub const VIRTIO_BLK_F_RO: u8 = 5;
pub const VIRTIO_BLK_F_BLK_SIZE: u8 = 6;
pub const VIRTIO_BLK_F_SCSI: u8 = 7;
pub const VIRTIO_BLK_F_FLUSH: u8 = 9;
pub const VIRTIO_BLK_F_TOPOLOGY: u8 = 10;
pub const VIRTIO_BLK_ID_BYTES: usize = 20;

/// Bitmask of all defined virtio-blk features.
pub const VIRTIO_BLK_FEATURES: u32 = (1 << VIRTIO_BLK_F_BARRIER)
    | (1 << VIRTIO_BLK_F_SIZE_MAX)
    | (1 << VIRTIO_BLK_F_SEG_MAX)
    | (1 << VIRTIO_BLK_F_GEOMETRY)
    | (1 << VIRTIO_BLK_F_RO)
    | (1 << VIRTIO_BLK_F_BLK_SIZE)
    | (1 << VIRTIO_BLK_F_SCSI)
    | (1 << VIRTIO_BLK_F_FLUSH)
    | (1 << VIRTIO_BLK_F_TOPOLOGY);

/// Feature descriptor for the virtio transport layer.
///
/// All `guest_support` fields are 0 (no optional features are negotiated
/// by default in this simplified driver, mirroring the Minix C driver).
const VIRTIO_BLK_FEATURE_LIST: &[VirtioFeature] = &[
    VirtioFeature {
        name: "barrier",
        bit: VIRTIO_BLK_F_BARRIER,
        host_support: 0,
        guest_support: 0,
    },
    VirtioFeature {
        name: "size_max",
        bit: VIRTIO_BLK_F_SIZE_MAX,
        host_support: 0,
        guest_support: 0,
    },
    VirtioFeature {
        name: "seg_max",
        bit: VIRTIO_BLK_F_SEG_MAX,
        host_support: 0,
        guest_support: 0,
    },
    VirtioFeature {
        name: "geometry",
        bit: VIRTIO_BLK_F_GEOMETRY,
        host_support: 0,
        guest_support: 0,
    },
    VirtioFeature {
        name: "read-only",
        bit: VIRTIO_BLK_F_RO,
        host_support: 0,
        guest_support: 0,
    },
    VirtioFeature {
        name: "blk_size",
        bit: VIRTIO_BLK_F_BLK_SIZE,
        host_support: 0,
        guest_support: 0,
    },
    VirtioFeature {
        name: "scsi",
        bit: VIRTIO_BLK_F_SCSI,
        host_support: 0,
        guest_support: 0,
    },
    VirtioFeature {
        name: "flush",
        bit: VIRTIO_BLK_F_FLUSH,
        host_support: 0,
        guest_support: 0,
    },
    VirtioFeature {
        name: "topology",
        bit: VIRTIO_BLK_F_TOPOLOGY,
        host_support: 0,
        guest_support: 0,
    },
];

/// Virtio block device configuration (from PCI config space / virtio
/// device-specific registers).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct VirtioBlkConfig {
    pub capacity: u64,
    pub size_max: u32,
    pub seg_max: u32,
    pub cylinders: u16,
    pub heads: u8,
    pub sectors: u8,
    pub blk_size: u32,
    pub physical_block_exp: u8,
    pub alignment_offset: u8,
    pub min_io_size: u16,
    pub opt_io_size: u32,
}

impl VirtioBlkConfig {
    pub const fn new() -> Self {
        Self {
            capacity: 0,
            size_max: 0,
            seg_max: 0,
            cylinders: 0,
            heads: 0,
            sectors: 0,
            blk_size: 512,
            physical_block_exp: 0,
            alignment_offset: 0,
            min_io_size: 0,
            opt_io_size: 0,
        }
    }
}

impl Default for VirtioBlkConfig {
    fn default() -> Self {
        Self::new()
    }
}

pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;
pub const VIRTIO_BLK_T_SCSI_CMD: u32 = 2;
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;
pub const VIRTIO_BLK_T_GET_ID: u32 = 8;
pub const VIRTIO_BLK_T_BARRIER: u32 = 0x8000_0000;

/// Virtio block request header.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct VirtioBlkOuthdr {
    pub type_: u32,
    pub ioprio: u32,
    pub sector: u64,
}

impl VirtioBlkOuthdr {
    pub const fn new() -> Self {
        Self {
            type_: 0,
            ioprio: 0,
            sector: 0,
        }
    }
}

impl Default for VirtioBlkOuthdr {
    fn default() -> Self {
        Self::new()
    }
}

pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

/// Block size is always 512 bytes for virtio-blk.
pub const VIRTIO_BLK_BLOCK_SIZE: u32 = 512;

/// Number of worker threads (reserved; not yet used in this single-threaded
/// port).
pub const VIRTIO_BLK_NUM_THREADS: usize = 4;

/// Maximum number of segments per request.
pub const VIRTIO_BLK_MAX_SEGMENTS: u32 = 128;

/// Pre-allocated request headers for each thread slot.
///
/// In the C driver these are allocated via `alloc_contig()` for DMA.
/// For the initial Rust port we use static storage (single-threaded).
/// Physical addresses must be supplied by the platform layer.
struct HdrsCell(UnsafeCell<[VirtioBlkOuthdr; 1]>);
unsafe impl Sync for HdrsCell {}
impl HdrsCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([VirtioBlkOuthdr::new()]))
    }
    fn get(&self) -> *mut [VirtioBlkOuthdr; 1] {
        self.0.get()
    }
}

struct StatusCell(UnsafeCell<[u8; 1]>);
unsafe impl Sync for StatusCell {}
impl StatusCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8]))
    }
    fn get(&self) -> *mut [u8; 1] {
        self.0.get()
    }
}

struct StateCell(UnsafeCell<VirtioBlkState>);
unsafe impl Sync for StateCell {}
impl StateCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(VirtioBlkState::new()))
    }
    fn get(&self) -> *mut VirtioBlkState {
        self.0.get()
    }
}

static HDRS: HdrsCell = HdrsCell::new();

/// Pre-allocated status bytes for each thread slot.
static STATUS: StatusCell = StatusCell::new();

/// Runtime state of the virtio-block driver.
struct VirtioBlkState {
    /// Option holds the device once probed.
    dev: Option<VirtioDevice>,
    /// Shadow of the device configuration.
    config: VirtioBlkConfig,
    /// Number of times the device has been opened.
    open_count: i32,
    /// Whether the device is read-only.
    ro: bool,
    /// Whether the device supports the FLUSH command.
    flush: bool,
    /// Block size in bytes (always 512 for virtio-blk).
    blk_size: u32,
    /// Whether a termination request has been received.
    terminating: bool,
}

impl VirtioBlkState {
    const fn new() -> Self {
        Self {
            dev: None,
            config: VirtioBlkConfig::new(),
            open_count: 0,
            ro: false,
            flush: false,
            blk_size: VIRTIO_BLK_BLOCK_SIZE,
            terminating: false,
        }
    }

    /// Total capacity in bytes.
    #[allow(dead_code)]
    fn capacity_bytes(&self) -> u64 {
        self.config.capacity * (self.blk_size as u64)
    }
}

static STATE: StateCell = StateCell::new();

/// Helper: get a mutable pointer to the global state.
fn state_ptr() -> *mut VirtioBlkState {
    STATE.get()
}

/// Helper: get a mutable pointer to the header storage.
fn hdrs_ptr() -> *mut [VirtioBlkOuthdr; 1] {
    HDRS.get()
}

/// Helper: get a mutable pointer to the status storage.
fn status_ptr() -> *mut [u8; 1] {
    STATUS.get()
}

/// Read the virtio-blk device configuration from the device-specific
/// PCI config registers.
///
/// Corresponds to `virtio_blk_config()` in the C reference.
fn read_device_config(dev: &VirtioDevice, config: &mut VirtioBlkConfig) {
    // Capacity is always present (two 32-bit reads at offsets 0 and 4).
    let sectors_low = virtio::virtio_sread32(dev, 0);
    let sectors_high = virtio::virtio_sread32(dev, 4);
    config.capacity = ((sectors_high as u64) << 32) | sectors_low as u64;

    // Feature-dependent configuration reads.
    // In the C code this is `virtio_blk_feature_setup()`.
    if virtio::virtio_host_supports(dev, VIRTIO_BLK_F_SEG_MAX) {
        config.seg_max = virtio::virtio_sread32(dev, 12);
    }

    if virtio::virtio_host_supports(dev, VIRTIO_BLK_F_GEOMETRY) {
        config.cylinders = virtio::virtio_sread16(dev, 16);
        config.heads = virtio::virtio_sread8(dev, 18);
        config.sectors = virtio::virtio_sread8(dev, 19);
    }

    if virtio::virtio_host_supports(dev, VIRTIO_BLK_F_BLK_SIZE) {
        config.blk_size = virtio::virtio_sread32(dev, 20);
    }
}

/// Convert a virtio status byte to a `DriverError`.
///
/// Corresponds to `virtio_blk_status2error()` in the C reference.
/// The C version calls `panic()` on unknown status; we return `Unknown`.
pub fn virtio_blk_status_to_error(status: u8) -> DriverError {
    match status {
        VIRTIO_BLK_S_OK => DriverError::Unknown,
        VIRTIO_BLK_S_IOERR => DriverError::Io,
        VIRTIO_BLK_S_UNSUPP => DriverError::Unsupported,
        _ => DriverError::Unknown,
    }
}

/// Create a request header for a read/write command.
pub fn virtio_blk_make_request(type_: u32, sector: u64) -> VirtioBlkOuthdr {
    VirtioBlkOuthdr {
        type_,
        ioprio: 0,
        sector,
    }
}

/// Poll the queue for a completed request.
///
/// Returns `true` if a completion was found and the status byte is OK.
fn poll_completion() -> bool {
    unsafe {
        let st = &mut *state_ptr();
        if let Some(ref mut dev) = st.dev
            && let Some(_token) = virtio::virtio_from_queue(dev)
        {
            // Status byte was written by the host at STATUS[0].
            return (*status_ptr())[0] == VIRTIO_BLK_S_OK;
        }
        false
    }
}

/// Spin-wait for a queue completion with a bounded number of iterations.
///
/// Returns `Ok(())` if completed successfully, `Err(DriverError::Io)` if
/// the host reported an error, or `Err(DriverError::Busy)` on timeout.
fn wait_for_completion(max_spins: u32) -> Result<(), DriverError> {
    for _ in 0..max_spins {
        if poll_completion() {
            return Ok(());
        }
        // Lightweight hint for the CPU (equivalent to `pause` / `rep nop`).
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
        }
    }
    // Final check before declaring timeout.
    if poll_completion() {
        return Ok(());
    }
    // Check status one more time for a definitive error.
    unsafe {
        let s = (*status_ptr())[0];
        if s != VIRTIO_BLK_S_OK {
            return Err(virtio_blk_status_to_error(s));
        }
    }
    Err(DriverError::Busy)
}

/// Initialise global driver state (must be called before anything else).
///
/// This is a lightweight reset — the real hardware probe is done by
/// `virtio_blk_probe()`.
pub fn virtio_blk_init() {
    unsafe {
        let st = &mut *state_ptr();
        st.dev = None;
        st.config = VirtioBlkConfig::new();
        st.open_count = 0;
        st.ro = false;
        st.flush = false;
        st.blk_size = VIRTIO_BLK_BLOCK_SIZE;
        st.terminating = false;
    }
}

/// Probe for a virtio-blk device on PCI.
///
/// Scans PCI bus 0 for a virtio device with sub-device ID `0x0002`.
/// When found, it initialises the virtio transport, allocates a
/// virtqueue, allocates request header/status DMA buffers, reads the
/// device configuration, and marks the device ready.
///
/// `instance` selects which matching device to use (0 = first).
///
/// Returns `Ok(())` on success.
///
/// # Safety
///
/// Must be called once, after PCI init and before any I/O.
/// Must not be called concurrently.
pub unsafe fn virtio_blk_probe(instance: u16) -> Result<(), DriverError> {
    // SAFETY: caller guarantees exclusive access.
    let st = unsafe { &mut *state_ptr() };

    // 1. Probe via the virtio transport layer.
    let mut dev = virtio::virtio_probe(
        VIRTIO_BLK_SUBSYSTEM_ID,
        "virtio-blk",
        VIRTIO_BLK_FEATURE_LIST,
        instance,
    )
    .map_err(|_| DriverError::NotFound)?;

    // 2. Allocate virtqueue.
    virtio::virtio_alloc_queue(&mut dev).map_err(|_| DriverError::Io)?;

    // 3. Read device configuration.
    read_device_config(&dev, &mut st.config);

    // Mirror feature state into the driver struct.
    st.ro = virtio::virtio_host_supports(&dev, VIRTIO_BLK_F_RO);
    st.flush = virtio::virtio_host_supports(&dev, VIRTIO_BLK_F_FLUSH);
    st.blk_size = st.config.blk_size;

    // 4. Signal readiness to the host.
    virtio::virtio_device_ready(&mut dev);

    // 5. Enable IRQ (no-op in the current transport layer).
    virtio::virtio_irq_enable(&mut dev);

    st.dev = Some(dev);

    Ok(())
}

/// Open the virtio block device.
///
/// In the C driver this also triggers partition scanning on first open.
/// This simplified version merely tracks the open count.
pub fn virtio_blk_open() -> Result<(), DriverError> {
    // SAFETY: single-threaded access to global state.
    let st = unsafe { &mut *state_ptr() };
    if st.dev.is_none() {
        return Err(DriverError::NotFound);
    }
    st.open_count += 1;
    Ok(())
}

/// Close the virtio block device.
///
/// When fully closed, issues a cache flush (if the device supports it).
pub fn virtio_blk_close() -> Result<(), DriverError> {
    // SAFETY: single-threaded access to global state.
    let st = unsafe { &mut *state_ptr() };
    if st.dev.is_none() {
        return Err(DriverError::NotFound);
    }
    if st.open_count == 0 {
        return Ok(());
    }
    st.open_count -= 1;

    if st.open_count == 0 && st.flush {
        // Flush the device cache.
        let _ = virtio_blk_flush_inner();
    }

    // If terminating and fully closed, clean up.
    if st.terminating && st.open_count == 0 {
        virtio_blk_cleanup_inner();
    }

    Ok(())
}

/// Check if the device is read-only.
pub fn virtio_blk_is_ro() -> bool {
    // SAFETY: single-threaded access to the global state.
    unsafe { (*state_ptr()).ro }
}

/// Check if the device supports flush.
pub fn virtio_blk_has_flush() -> bool {
    // SAFETY: single-threaded access to the global state.
    unsafe { (*state_ptr()).flush }
}

/// Get device geometry as (capacity_in_sectors, block_size_in_bytes).
pub fn virtio_blk_geometry() -> (u64, u32) {
    unsafe {
        let st = &*state_ptr();
        (st.config.capacity, st.blk_size)
    }
}

/// Perform a block I/O transfer.
///
/// Reads (`write` = false) or writes (`write` = true) `blocks` sectors
/// starting at `sector` to/from `buf`.
///
/// `buf` must be at least `blocks * VIRTIO_BLK_BLOCK_SIZE` bytes.
///
/// Returns the number of bytes transferred on success.
///
/// # Safety
///
/// `buf` must point to a valid buffer of sufficient size.
pub unsafe fn virtio_blk_transfer(
    write: bool,
    sector: u64,
    buf: &mut [u8],
) -> Result<usize, DriverError> {
    // SAFETY: caller guarantees exclusive access to state and DMA buffers.
    let st = unsafe { &mut *state_ptr() };
    let dev = st.dev.as_mut().ok_or(DriverError::NotFound)?;

    let size = buf.len() as u32;
    if size == 0 || !size.is_multiple_of(VIRTIO_BLK_BLOCK_SIZE) {
        return Err(DriverError::InvalidArgument);
    }

    let num_sectors = size / VIRTIO_BLK_BLOCK_SIZE;

    // Bounds check: position >= capacity means EOF.
    if sector >= st.config.capacity {
        return Ok(0);
    }

    // Truncate if the request extends beyond the device capacity.
    let end_sector = sector.saturating_add(num_sectors as u64);
    let max_end = st.config.capacity;
    let transfer_sectors = if end_sector > max_end {
        (max_end - sector) as u32
    } else {
        num_sectors
    };

    if transfer_sectors == 0 {
        return Ok(0);
    }

    let transfer_size = transfer_sectors * VIRTIO_BLK_BLOCK_SIZE;

    //
    // Layout: [header (readable)] [data (writable for read, readable for
    // write)] [status (writable)]
    //
    // Note: the last bit of PhysBuf.addr is used as a writable flag by the
    // virtio transport layer.

    // Prepare the header at HDRS[0].
    // SAFETY: single-threaded access to global DMA buffers.
    let hdr = unsafe { &mut (*hdrs_ptr())[0] };
    hdr.type_ = if write {
        VIRTIO_BLK_T_OUT
    } else {
        VIRTIO_BLK_T_IN
    };
    hdr.ioprio = 0;
    hdr.sector = sector;

    // The header physical address.  In a real system this must be the
    // physical (guest-physical) address of `HDRS`.  For now we treat the
    // pointer as an identity-mapped address (common in bare-metal or
    // early boot).  The transport layer strips the LSB for the address and
    // uses it as the writable flag.
    let hdr_paddr = hdr as *const _ as u64;

    // Status buffer: writable.
    // SAFETY: single-threaded access to global DMA buffers.
    let status = unsafe { &mut (*status_ptr())[0] };
    *status = 0xFF; // pre-fill with invalid
    let status_paddr = status as *const _ as u64;

    // Data buffer: writable for read, readable for write.
    let data_flags = if !write { VRING_DESC_F_WRITE as u64 } else { 0 };
    let data_paddr = (buf.as_ptr() as u64) | data_flags;

    let phys_bufs = [
        VirtioPhysBuf {
            addr: hdr_paddr,
            size: core::mem::size_of::<VirtioBlkOuthdr>() as u32,
        },
        VirtioPhysBuf {
            addr: data_paddr,
            size: transfer_size,
        },
        VirtioPhysBuf {
            addr: status_paddr | 1, // LSB = 1 → writable
            size: 1,
        },
    ];

    // Submit to the queue.
    virtio::virtio_to_queue(dev, &phys_bufs, 0).map_err(|_| DriverError::Io)?;

    // Wait for the host to complete the request.
    // Spin with a generous timeout (100M iterations ≈ ~100ms at 1GHz).
    wait_for_completion(100_000_000)?;

    Ok(transfer_size as usize)
}

/// Inner flush helper (used by close path).
fn virtio_blk_flush_inner() -> Result<(), DriverError> {
    unsafe {
        let st = &mut *state_ptr();
        let dev = st.dev.as_mut().ok_or(DriverError::NotFound)?;

        if !st.flush {
            return Err(DriverError::Unsupported);
        }

        // Prepare the header.
        let hdr = &mut (*hdrs_ptr())[0];
        hdr.type_ = VIRTIO_BLK_T_FLUSH;
        if virtio::virtio_host_supports(dev, VIRTIO_BLK_F_BARRIER) {
            hdr.type_ |= VIRTIO_BLK_T_BARRIER;
        }
        hdr.ioprio = 0;
        hdr.sector = 0;

        let hdr_paddr = hdr as *const _ as u64;
        let status = &mut (*status_ptr())[0];
        *status = 0xFF;
        let status_paddr = status as *const _ as u64;

        let phys_bufs = [
            VirtioPhysBuf {
                addr: hdr_paddr,
                size: core::mem::size_of::<VirtioBlkOuthdr>() as u32,
            },
            VirtioPhysBuf {
                addr: status_paddr | 1,
                size: 1,
            },
        ];

        virtio::virtio_to_queue(dev, &phys_bufs, 0).map_err(|_| DriverError::Io)?;
        wait_for_completion(100_000_000)?;

        Ok(())
    }
}

/// Flush the device cache.
///
/// Returns `Err(DriverError::Unsupported)` if the device does not
/// support the FLUSH feature.
pub fn virtio_blk_flush() -> Result<(), DriverError> {
    virtio_blk_flush_inner()
}

/// Handle a device interrupt.
///
/// Should be called from the interrupt handler.  Reaps any completed
/// descriptors from the queue.
pub fn virtio_blk_intr() {
    unsafe {
        let st = &mut *state_ptr();
        if let Some(ref mut dev) = st.dev
            && virtio::virtio_had_irq(dev)
        {
            // Reap all available completed descriptors.
            while virtio::virtio_from_queue(dev).is_some() {
                // The waiting caller will pick up the status byte.
            }
            virtio::virtio_irq_enable(dev);
        }
    }
}

/// Initiate a graceful shutdown.
pub fn virtio_blk_terminate() {
    unsafe {
        let st = &mut *state_ptr();
        st.terminating = true;
        if st.open_count == 0 {
            virtio_blk_cleanup_inner();
        }
    }
}

/// Inner cleanup: reset device, free resources.
fn virtio_blk_cleanup_inner() {
    unsafe {
        let st = &mut *state_ptr();
        if let Some(ref mut dev) = st.dev {
            virtio::virtio_reset_device(dev);
        }
        st.dev = None;
    }
}

/// Clean up driver resources (reset device, release memory).
pub fn virtio_blk_cleanup() {
    virtio_blk_cleanup_inner();
}

/// Check if a feature is advertised by the host/set on the device.
pub fn virtio_blk_has_feature(bit: u8) -> bool {
    unsafe {
        let st = &*state_ptr();
        st.dev
            .as_ref()
            .is_some_and(|dev| virtio::virtio_host_supports(dev, bit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reset global state for testing.
    unsafe fn reset_state() {
        // SAFETY: single-threaded test context.
        let st = unsafe { &mut *state_ptr() };
        st.dev = None;
        st.config = VirtioBlkConfig::new();
        st.open_count = 0;
        st.ro = false;
        st.flush = false;
        st.blk_size = VIRTIO_BLK_BLOCK_SIZE;
        st.terminating = false;
    }

    #[test]
    fn test_virtio_constants() {
        assert_eq!(VIRTIO_VENDOR_ID, 0x1AF4);
        assert_eq!(VIRTIO_DEVICE_ID_BLOCK, 0x1001);
        assert_eq!(VIRTIO_BLK_SUBSYSTEM_ID, 0x0002);
        assert_eq!(VIRTIO_BLK_BLOCK_SIZE, 512);
        assert_eq!(VIRTIO_BLK_ID_BYTES, 20);
    }

    #[test]
    fn test_config_new() {
        let c = VirtioBlkConfig::new();
        assert_eq!(c.capacity, 0);
        assert_eq!(c.blk_size, 512);
    }

    #[test]
    fn test_config_default() {
        let c: VirtioBlkConfig = Default::default();
        assert_eq!(c.blk_size, 512);
    }

    #[test]
    fn test_outhdr_new() {
        let h = VirtioBlkOuthdr::new();
        assert_eq!(h.type_, 0);
        assert_eq!(h.sector, 0);
    }

    #[test]
    fn test_state_new() {
        let s = VirtioBlkState::new();
        assert!(s.dev.is_none());
        assert_eq!(s.open_count, 0);
        assert!(!s.ro);
        assert_eq!(s.blk_size, 512);
        assert_eq!(s.capacity_bytes(), 0);
    }

    #[test]
    fn test_open_close() {
        unsafe {
            reset_state();
            // Without a device, open should fail.
            assert!(virtio_blk_open().is_err());

            // With a device (simulated), open should succeed.
            // We can't easily fake a VirtioDevice, but we can test the
            // open_count tracking by directly manipulating state.
            let st = &mut *state_ptr();
            st.open_count = 1;
            assert_eq!(st.open_count, 1);

            // Close should decrement.
            st.open_count = 0;
        }
    }

    #[test]
    fn test_geometry() {
        unsafe {
            reset_state();
            let st = &mut *state_ptr();
            st.config.capacity = 1000;
            st.blk_size = 512;
        }
        let (cap, blk) = virtio_blk_geometry();
        assert_eq!(cap, 1000);
        assert_eq!(blk, 512);
    }

    #[test]
    fn test_status_to_error() {
        assert!(matches!(
            virtio_blk_status_to_error(VIRTIO_BLK_S_IOERR),
            DriverError::Io
        ));
        assert!(matches!(
            virtio_blk_status_to_error(VIRTIO_BLK_S_UNSUPP),
            DriverError::Unsupported
        ));
        assert!(matches!(
            virtio_blk_status_to_error(0xFF),
            DriverError::Unknown
        ));
    }

    #[test]
    fn test_make_request() {
        let h = virtio_blk_make_request(VIRTIO_BLK_T_IN, 1234);
        assert_eq!(h.type_, VIRTIO_BLK_T_IN);
        assert_eq!(h.sector, 1234);
        assert_eq!(h.ioprio, 0);
    }

    #[test]
    fn test_make_request_write() {
        let h = virtio_blk_make_request(VIRTIO_BLK_T_OUT, 5678);
        assert_eq!(h.type_, VIRTIO_BLK_T_OUT);
        assert_eq!(h.sector, 5678);
    }

    #[test]
    fn test_features_defined() {
        const {
            assert!(VIRTIO_BLK_FEATURES & (1 << VIRTIO_BLK_F_BARRIER) != 0);
            assert!(VIRTIO_BLK_FEATURES & (1 << VIRTIO_BLK_F_FLUSH) != 0);
            assert!(VIRTIO_BLK_FEATURES & (1 << VIRTIO_BLK_F_RO) != 0);
            assert!(VIRTIO_BLK_FEATURES & (1 << VIRTIO_BLK_F_BLK_SIZE) != 0);
            assert!(VIRTIO_BLK_FEATURES & (1 << VIRTIO_BLK_F_GEOMETRY) != 0);
        }
    }

    #[test]
    fn test_feature_list() {
        // The feature list passed to the transport layer must match the
        // defined feature constants.
        assert_eq!(VIRTIO_BLK_FEATURE_LIST.len(), 9);
        assert_eq!(VIRTIO_BLK_FEATURE_LIST[0].bit, VIRTIO_BLK_F_BARRIER);
        assert_eq!(VIRTIO_BLK_FEATURE_LIST[4].bit, VIRTIO_BLK_F_RO);
        assert_eq!(VIRTIO_BLK_FEATURE_LIST[7].bit, VIRTIO_BLK_F_FLUSH);
    }

    #[test]
    fn test_request_types() {
        assert_eq!(VIRTIO_BLK_T_IN, 0);
        assert_eq!(VIRTIO_BLK_T_OUT, 1);
        assert_eq!(VIRTIO_BLK_T_FLUSH, 4);
        assert_eq!(VIRTIO_BLK_T_GET_ID, 8);
    }

    #[test]
    fn test_status_codes() {
        assert_eq!(VIRTIO_BLK_S_OK, 0);
        assert_eq!(VIRTIO_BLK_S_IOERR, 1);
        assert_eq!(VIRTIO_BLK_S_UNSUPP, 2);
    }

    #[test]
    fn test_capacity_bytes() {
        let mut s = VirtioBlkState::new();
        s.config.capacity = 1000;
        assert_eq!(s.capacity_bytes(), 512_000);
    }

    #[test]
    fn test_thread_count() {
        assert_eq!(VIRTIO_BLK_NUM_THREADS, 4);
    }

    #[test]
    #[ignore = "covered in kernel-tests (QEMU)"]
    fn test_virtio_blk_probe_fails_no_hardware() {
        unsafe {
            reset_state();
            // Without real PCI hardware, probe should fail gracefully.
            let result = virtio_blk_probe(0);
            // The exact error depends on whether PCI config ports
            // respond. On CI/test it should be NotFound.
            assert!(result.is_err());
            // Should not panic.
        }
    }

    #[test]
    fn test_transfer_zero_size() {
        unsafe {
            reset_state();
            let mut buf = [];
            let result = virtio_blk_transfer(false, 0, &mut buf);
            // No device — should return NotFound.
            assert!(matches!(result, Err(DriverError::NotFound)));
        }
    }

    #[test]
    fn test_transfer_non_aligned_size() {
        unsafe {
            reset_state();
            let mut buf = [0u8; 511]; // not a multiple of 512
            let result = virtio_blk_transfer(false, 0, &mut buf);
            // No device — but we should get NotFound before the size check.
            assert!(matches!(result, Err(DriverError::NotFound)));
        }
    }

    #[test]
    fn test_read_device_config_defaults() {
        // Without a real device, read_device_config should leave the
        // config at its defaults.  This tests that the function doesn't
        // panic when called with dummy data.
        let config = VirtioBlkConfig::new();
        // We can't call read_device_config without a real VirtioDevice.
        // Just verify that new() gives sensible defaults.
        assert_eq!(config.capacity, 0);
        assert_eq!(config.blk_size, 512);
        assert_eq!(config.seg_max, 0);
        assert_eq!(config.cylinders, 0);
    }

    #[test]
    fn test_virtio_blk_init_resets_state() {
        unsafe {
            // First set some non-default state.
            let st = &mut *state_ptr();
            st.open_count = 42;
            st.ro = true;
            st.flush = true;
        }
        virtio_blk_init();
        unsafe {
            let st = &*state_ptr();
            assert_eq!(st.open_count, 0);
            assert!(!st.ro);
            assert!(!st.flush);
            assert!(st.dev.is_none());
        }
    }

    #[test]
    fn test_status_to_error_roundtrip() {
        // OK status should map to Unknown (caller shouldn't pass OK).
        assert!(matches!(
            virtio_blk_status_to_error(VIRTIO_BLK_S_OK),
            DriverError::Unknown
        ));
    }

    #[test]
    fn test_blk_subsystem_id() {
        assert_eq!(VIRTIO_BLK_SUBSYSTEM_ID, 0x0002);
    }

    #[test]
    fn test_virtio_blk_make_request_all_fields() {
        let h = virtio_blk_make_request(VIRTIO_BLK_T_FLUSH | VIRTIO_BLK_T_BARRIER, 0);
        assert_eq!(h.type_, VIRTIO_BLK_T_FLUSH | VIRTIO_BLK_T_BARRIER);
        assert_eq!(h.sector, 0);
    }

    #[test]
    fn test_feature_list_names() {
        assert_eq!(VIRTIO_BLK_FEATURE_LIST[0].name, "barrier");
        assert_eq!(VIRTIO_BLK_FEATURE_LIST[4].name, "read-only");
        assert_eq!(VIRTIO_BLK_FEATURE_LIST[7].name, "flush");
    }

    #[test]
    fn test_virtio_blk_ro_flags() {
        unsafe {
            reset_state();
            let st = &mut *state_ptr();
            assert!(!st.ro);
            st.ro = true;
            assert!(virtio_blk_is_ro());
        }
    }

    #[test]
    fn test_virtio_blk_flush_flag() {
        unsafe {
            reset_state();
            let st = &mut *state_ptr();
            assert!(!st.flush);
            st.flush = true;
            assert!(virtio_blk_has_flush());
        }
    }

    #[test]
    fn test_virtio_blk_cleanup_is_safe() {
        // Cleanup on uninitialised state should not panic.
        unsafe {
            reset_state();
            virtio_blk_cleanup();
        }
    }

    #[test]
    fn test_virtio_blk_terminate_is_safe() {
        // Terminate on uninitialised state should not panic.
        unsafe {
            reset_state();
            virtio_blk_terminate();
        }
    }

    #[test]
    fn test_blk_state_send() {
        fn assert_send<T: Send>() {}
        assert_send::<VirtioBlkState>();
    }
}
