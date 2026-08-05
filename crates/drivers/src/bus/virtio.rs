//! Virtio transport layer.
//!
//! The legacy virtio-over-PCI path (I/O port BARs, x86_64) is ported from
//! `.refs/minix-3.3.0/minix/lib/libvirtio/virtio.c`. The modern
//! virtio-mmio path (virtio 1.x, fixed MMIO base on QEMU `virt` machines)
//! serves RISC-V and AArch64, which have no I/O ports.
//!
//! Uses fixed-size static arrays for vring storage (no heap allocation
//! required). The vring/queue machinery is shared by both transports.
//!
//! # Safety
//!
//! This module performs raw I/O port access and manipulates mutable
//! statics via `core::ptr::addr_of_mut!()` per Rust 2024
//! `deny(static_mut_refs)`. All `unsafe` blocks are documented with
//! their invariants.

#![allow(dead_code)]

use core::cell::UnsafeCell;
#[cfg(test)]
use core::mem::size_of;
use core::sync::atomic::{AtomicI64, AtomicUsize};

/// Error type for virtio operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioError;

/// Device access transport for a virtio device.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VirtioTransport {
    /// Legacy virtio-pci: config space via 0xCF8/0xCFC, device registers
    /// via an I/O port BAR (x86_64).
    Pci,
    /// Modern virtio-mmio (virtio 1.x): device registers at a fixed MMIO
    /// base address (RISC-V and AArch64 QEMU `virt` machines).
    Mmio,
}

/// Register offsets for legacy virtio PCI (I/O port BAR).
pub const VIRTIO_HOST_F_OFF: u16 = 0x0000;
pub const VIRTIO_GUEST_F_OFF: u16 = 0x0004;
pub const VIRTIO_QADDR_OFF: u16 = 0x0008;
pub const VIRTIO_QSIZE_OFF: u16 = 0x000C;
pub const VIRTIO_QSEL_OFF: u16 = 0x000E;
pub const VIRTIO_QNOTIFY_OFF: u16 = 0x0010;
pub const VIRTIO_DEV_STATUS_OFF: u16 = 0x0012;
pub const VIRTIO_ISR_STATUS_OFF: u16 = 0x0013;
pub const VIRTIO_DEV_SPECIFIC_OFF: u16 = 0x0014;

/// MSI offset compensation when MSI is enabled.
pub const VIRTIO_MSI_ADD_OFF: u16 = 0x0004;

/// Device status flags.
pub const VIRTIO_STATUS_ACK: u8 = 0x01;
pub const VIRTIO_STATUS_DRV: u8 = 0x02;
pub const VIRTIO_STATUS_DRV_OK: u8 = 0x04;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 0x08;
pub const VIRTIO_STATUS_FAIL: u8 = 0x80;

/// VRing descriptor flags.
pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;
pub const VRING_DESC_F_INDIRECT: u16 = 4;

/// VRing avail/used flags.
pub const VRING_USED_F_NO_NOTIFY: u16 = 1;
pub const VRING_AVAIL_F_NO_INTERRUPT: u16 = 1;

/// Virtio PCI vendor ID.
pub const VIRTIO_PCI_VENDOR: u16 = 0x1AF4;

/// Modern virtio-mmio (virtio 1.x) register offsets.
/// Layout from `linux/virtio_mmio.h`, which QEMU vendors verbatim.
pub const VIRTIO_MMIO_MAGIC_VALUE: u16 = 0x000;
pub const VIRTIO_MMIO_VERSION: u16 = 0x004;
pub const VIRTIO_MMIO_DEVICE_ID: u16 = 0x008;
pub const VIRTIO_MMIO_VENDOR_ID: u16 = 0x00c;
pub const VIRTIO_MMIO_DEVICE_FEATURES: u16 = 0x010;
pub const VIRTIO_MMIO_DEVICE_FEATURES_SEL: u16 = 0x014;
pub const VIRTIO_MMIO_DRIVER_FEATURES: u16 = 0x020;
pub const VIRTIO_MMIO_DRIVER_FEATURES_SEL: u16 = 0x024;
pub const VIRTIO_MMIO_QUEUE_SEL: u16 = 0x030;
pub const VIRTIO_MMIO_QUEUE_NUM_MAX: u16 = 0x034;
pub const VIRTIO_MMIO_QUEUE_NUM: u16 = 0x038;
pub const VIRTIO_MMIO_QUEUE_READY: u16 = 0x044;
pub const VIRTIO_MMIO_QUEUE_NOTIFY: u16 = 0x050;
pub const VIRTIO_MMIO_INTERRUPT_STATUS: u16 = 0x060;
pub const VIRTIO_MMIO_INTERRUPT_ACK: u16 = 0x064;
pub const VIRTIO_MMIO_STATUS: u16 = 0x070;
pub const VIRTIO_MMIO_QUEUE_DESC_LOW: u16 = 0x080;
pub const VIRTIO_MMIO_QUEUE_DESC_HIGH: u16 = 0x084;
pub const VIRTIO_MMIO_QUEUE_AVAIL_LOW: u16 = 0x090;
pub const VIRTIO_MMIO_QUEUE_AVAIL_HIGH: u16 = 0x094;
pub const VIRTIO_MMIO_QUEUE_USED_LOW: u16 = 0x0a0;
pub const VIRTIO_MMIO_QUEUE_USED_HIGH: u16 = 0x0a4;
pub const VIRTIO_MMIO_CONFIG: u16 = 0x100;

/// Magic value identifying a live virtio-mmio transport ("virt").
pub const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
/// Modern (virtio 1.x) MMIO transport version reported at offset 0x004.
pub const VIRTIO_MMIO_VERSION_MODERN: u32 = 2;
/// First virtio-mmio transport base on QEMU `virt` machines. The two
/// machines place the transports at different addresses with different
/// strides: RISC-V at 0x10001000 (0x1000 apart), AArch64 at 0x0a000000
/// (0x200 apart). The x86_64 value is a placeholder (never probed).
#[cfg(target_arch = "riscv64")]
pub const VIRTIO_MMIO_BASE: u64 = 0x1000_1000;
#[cfg(target_arch = "riscv64")]
pub const VIRTIO_MMIO_STRIDE: u64 = 0x1000;
#[cfg(target_arch = "aarch64")]
pub const VIRTIO_MMIO_BASE: u64 = 0x0a00_0000;
#[cfg(target_arch = "aarch64")]
pub const VIRTIO_MMIO_STRIDE: u64 = 0x200;
#[cfg(target_arch = "x86_64")]
pub const VIRTIO_MMIO_BASE: u64 = 0;
#[cfg(target_arch = "x86_64")]
pub const VIRTIO_MMIO_STRIDE: u64 = 0x200;

/// Transport count differs by machine: RISC-V virt has 8 transports
/// (0x10001000 + n*0x1000); AArch64 virt has 32 (0x0a000000 + n*0x200).
/// QEMU assigns the first `-device` to the HIGHEST-numbered transport, so
/// the driver must scan them all.
#[cfg(target_arch = "riscv64")]
pub const VIRTIO_MMIO_NUM_TRANSPORTS: u64 = 8;
#[cfg(target_arch = "aarch64")]
pub const VIRTIO_MMIO_NUM_TRANSPORTS: u64 = 32;
#[cfg(target_arch = "x86_64")]
pub const VIRTIO_MMIO_NUM_TRANSPORTS: u64 = 8;
/// VIRTIO_F_VERSION_1 (bit 32), mandatory for modern devices. It lives in
/// the high feature word, so it is bit 0 of the word selected by sel=1.
const VIRTIO_F_VERSION_1: u32 = 1;

/// Maximum number of descriptors per queue.
const QUEUE_NUM: u16 = 256;

/// Virtio feature descriptor.
///
/// Each feature is identified by a bit position. `host_support` is set
/// during feature exchange; `guest_support` indicates which features
/// the driver wants to negotiate (0 = not negotiated).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VirtioFeature {
    pub name: &'static str,
    pub bit: u8,
    pub host_support: u8,
    pub guest_support: u8,
}

/// Opaque virtio device handle.
///
/// Contains the transport and its base address (I/O port for PCI, MMIO
/// base for virtio-mmio), feature list, a single virtqueue backed by
/// static vring storage, IRQ line, and initialization state.
pub struct VirtioDevice {
    pub transport: VirtioTransport,
    /// I/O port base (Pci) or MMIO base address (Mmio).
    pub base: u64,
    pub name: &'static str,
    pub features: &'static [VirtioFeature],
    /// Bitmap of host-supported features (set during `exchange_features`).
    pub host_features: u64,
    pub queues: [VirtioQueue; 1],
    pub num_queues: usize,
    pub irq: u8,
    pub msi: bool,
    pub initialized: bool,
}

/// A virtqueue with vring management.
///
/// Manages a free-list of descriptors and tracks used-ring consumption.
pub struct VirtioQueue {
    pub vring: Vring,
    pub paddr: u64,
    pub free_num: u16,
    pub free_head: u16,
    pub free_tail: u16,
    pub last_used: u16,
}

/// VRing descriptor (16 bytes).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VringDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

/// VRing avail ring header.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VringAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; 256],
}

/// Used ring element.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VringUsedElem {
    pub id: u32,
    pub len: u32,
}

/// VRing used ring.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VringUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VringUsedElem; 256],
}

/// Full vring structure.
///
/// Holds references to the descriptor table, avail ring, and used ring.
/// These are typically backed by static storage.
pub struct Vring {
    pub num: u16,
    pub desc: &'static mut [VringDesc],
    pub avail: &'static mut VringAvail,
    pub used: &'static mut VringUsed,
}

/// Physical buffer descriptor for scatter-gather I/O.
///
/// The LSB of `addr` is used as a writable flag (`1` = writable).
/// Only word-aligned buffers should be used.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VirtioPhysBuf {
    pub addr: u64,
    pub size: u32,
}

//
// Pre-allocated storage for one virtqueue. Access is always through
// `core::ptr::addr_of_mut!()` — never create direct references to
// mutable statics per Rust 2024 rules.

/// Guest-physical translation offset (PA - VA) for this process's loaded
/// image. DMA drivers set it once at init via [`virtio_set_phys_delta`];
/// every address written to the device (queue base, descriptor addresses)
/// is translated with it.
static PHYS_DELTA: AtomicI64 = AtomicI64::new(0);

/// Set the VA→PA translation offset used when programming queue and
/// descriptor addresses into the device.
pub fn virtio_set_phys_delta(delta: i64) {
    PHYS_DELTA.store(delta, core::sync::atomic::Ordering::Relaxed);
}

/// Current VA→PA translation offset.
fn phys_delta() -> i64 {
    PHYS_DELTA.load(core::sync::atomic::Ordering::Relaxed)
}

/// Read 32 bits from an MMIO register (device memory).
///
/// # Safety
///
/// `addr` must be a mapped device-MMIO address valid for reads.
#[inline]
unsafe fn mmio_read32(addr: u64) -> u32 {
    // SAFETY: caller guarantees `addr` is a mapped device register.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// Write 32 bits to an MMIO register (device memory).
///
/// # Safety
///
/// `addr` must be a mapped device-MMIO address valid for writes.
#[inline]
unsafe fn mmio_write32(addr: u64, val: u32) {
    // SAFETY: caller guarantees `addr` is a mapped device register.
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

//
// One contiguous, page-aligned vring for the single queue. Legacy virtio
// hands the host a single queue base address and lets it derive the
// avail/used ring addresses from the spec layout, so the three rings
// must live at the spec offsets inside one buffer.
//

/// Descriptor table size: 256 × 16 bytes.
const RING_DESC_SIZE: usize = QUEUE_NUM as usize * core::mem::size_of::<VringDesc>();
/// Avail ring starts right after the descriptors.
const RING_AVAIL_OFF: usize = RING_DESC_SIZE;
/// Avail ring size: flags(2) + idx(2) + ring(2·n) + reserved used_event(2).
const RING_AVAIL_SIZE: usize = 6 + QUEUE_NUM as usize * 2;
/// Ring section alignment (page size).
///
/// Legacy virtio PCI places the descriptor table, available ring, and
/// used ring on page boundaries — the C reference passes `PAGE_SIZE` to
/// `vring_init` and the host derives the used-ring address the same way.
/// A smaller alignment puts the used ring where the host never writes it,
/// so completions (the used.idx advance) are invisible and requests hang.
const RING_ALIGN: usize = 4096;
/// Used ring starts page-aligned after the avail ring.
const RING_USED_OFF: usize =
    (RING_AVAIL_OFF + RING_AVAIL_SIZE + RING_ALIGN - 1) & !(RING_ALIGN - 1);
/// Used ring size: flags(2) + idx(2) + elems(8·n) + reserved event(2).
const RING_USED_SIZE: usize = 4 + QUEUE_NUM as usize * 8 + 2;
/// Total vring size.
const RING_SIZE: usize = RING_USED_OFF + RING_USED_SIZE;

#[repr(align(4096))]
struct Q0RingCell(UnsafeCell<[u8; RING_SIZE]>);
unsafe impl Sync for Q0RingCell {}
impl Q0RingCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; RING_SIZE]))
    }
    fn get(&self) -> *mut [u8; RING_SIZE] {
        self.0.get()
    }
}

static Q0_RING: Q0RingCell = Q0RingCell::new();
struct Q0DataCell(UnsafeCell<[usize; QUEUE_NUM as usize]>);
unsafe impl Sync for Q0DataCell {}
impl Q0DataCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0; QUEUE_NUM as usize]))
    }
    fn get(&self) -> *mut [usize; QUEUE_NUM as usize] {
        self.0.get()
    }
}

/// Token storage for the queue — maps descriptor-chain head to the
/// opaque data token given via `virtio_to_queue`.
static Q0_DATA: Q0DataCell = Q0DataCell::new();

/// Port I/O callback: `fn(request, port, value) -> reply_value`.
///
/// The driver process sets this before probing so the transport can
/// perform I/O without depending on a userland runtime itself (drivers
/// is also linked into the kernel, which has no runtime). The hook is a
/// plain `fn` so it stays stateless and serializable.
type DevioFn = fn(u32, u16, u32) -> u32;

static DEVIO_FN: AtomicUsize = AtomicUsize::new(0);

/// Register the port I/O hook used by the transport on the MINIX target.
/// Must be called before `virtio_probe`.
pub fn virtio_set_devio(hook: DevioFn) {
    DEVIO_FN.store(hook as usize, core::sync::atomic::Ordering::Relaxed);
}

/// Execute one port I/O operation through the registered hook.
#[cfg(target_os = "none")]
unsafe fn devio(request: u32, port: u16, value: u32) -> u32 {
    let raw = DEVIO_FN.load(core::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        return 0;
    }
    // SAFETY: only `virtio_set_devio` stores into DEVIO_FN, and it stores
    // the address of a real DevioFn.
    let hook: DevioFn = unsafe { core::mem::transmute(raw) };
    hook(request, port, value)
}

/// Write 8 bits to an I/O port.
#[inline]
unsafe fn out8(port: u16, val: u8) {
    #[cfg(target_os = "none")]
    unsafe {
        devio(arch_common::com::DIO_OUTPUT_BYTE, port, val as u32);
    }
    #[cfg(not(target_os = "none"))]
    unsafe {
        crate::hal::outb(port, val);
    }
}

/// Write 16 bits to an I/O port.
#[inline]
unsafe fn out16(port: u16, val: u16) {
    #[cfg(target_os = "none")]
    unsafe {
        devio(arch_common::com::DIO_OUTPUT_WORD, port, val as u32);
    }
    #[cfg(not(target_os = "none"))]
    unsafe {
        crate::hal::outw(port, val);
    }
}

/// Write 32 bits to an I/O port.
#[inline]
unsafe fn out32(port: u16, val: u32) {
    #[cfg(target_os = "none")]
    unsafe {
        devio(arch_common::com::DIO_OUTPUT_LONG, port, val);
    }
    #[cfg(not(target_os = "none"))]
    unsafe {
        crate::hal::outl(port, val);
    }
}

/// Read 8 bits from an I/O port.
#[inline]
unsafe fn in8(port: u16) -> u8 {
    #[cfg(target_os = "none")]
    {
        unsafe { devio(arch_common::com::DIO_INPUT_BYTE, port, 0) as u8 }
    }
    #[cfg(not(target_os = "none"))]
    {
        unsafe { crate::hal::inb(port) }
    }
}

/// Read 16 bits from an I/O port.
#[inline]
unsafe fn in16(port: u16) -> u16 {
    #[cfg(target_os = "none")]
    {
        unsafe { devio(arch_common::com::DIO_INPUT_WORD, port, 0) as u16 }
    }
    #[cfg(not(target_os = "none"))]
    {
        unsafe { crate::hal::inw(port) }
    }
}

/// Read 32 bits from an I/O port.
#[inline]
unsafe fn in32(port: u16) -> u32 {
    #[cfg(target_os = "none")]
    {
        unsafe { devio(arch_common::com::DIO_INPUT_LONG, port, 0) }
    }
    #[cfg(not(target_os = "none"))]
    {
        unsafe { crate::hal::inl(port) }
    }
}

/// Build a PCI configuration address.
fn pci_config_addr(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | (reg as u32 & 0xFC)
}

/// Read 8 bits from PCI config space.
///
/// # Safety
///
/// May conflict with other PCI config accesses. Must be serialized.
unsafe fn pci_cfg_read8(bus: u8, dev: u8, func: u8, reg: u8) -> u8 {
    let raw = unsafe { pci_cfg_read32(bus, dev, func, reg) };
    ((raw >> ((reg as u32 & 0x03) * 8)) & 0xFF) as u8
}

/// Read 16 bits from PCI config space.
///
/// # Safety
///
/// May conflict with other PCI config accesses. Must be serialized.
unsafe fn pci_cfg_read16(bus: u8, dev: u8, func: u8, reg: u8) -> u16 {
    let raw = unsafe { pci_cfg_read32(bus, dev, func, reg) };
    ((raw >> ((reg as u32 & 0x02) * 8)) & 0xFFFF) as u16
}

/// Read 32 bits from PCI config space.
///
/// # Safety
///
/// May conflict with other PCI config accesses. Must be serialized.
unsafe fn pci_cfg_read32(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        out32(crate::hal::PCI_ADDR_PORT, addr);
        in32(crate::hal::PCI_DATA_PORT)
    }
}

/// Read 32-bit from device register at `offset`.
pub fn virtio_read32(dev: &VirtioDevice, offset: u16) -> u32 {
    match dev.transport {
        VirtioTransport::Pci => unsafe { in32(dev.base as u16 + offset) },
        VirtioTransport::Mmio => unsafe { mmio_read32(dev.base + offset as u64) },
    }
}

/// Read 16-bit from device register at `offset`.
pub fn virtio_read16(dev: &VirtioDevice, offset: u16) -> u16 {
    match dev.transport {
        VirtioTransport::Pci => unsafe { in16(dev.base as u16 + offset) },
        VirtioTransport::Mmio => unsafe { mmio_read32(dev.base + offset as u64) as u16 },
    }
}

/// Read 8-bit from device register at `offset`.
pub fn virtio_read8(dev: &VirtioDevice, offset: u16) -> u8 {
    match dev.transport {
        VirtioTransport::Pci => unsafe { in8(dev.base as u16 + offset) },
        VirtioTransport::Mmio => unsafe { mmio_read32(dev.base + offset as u64) as u8 },
    }
}

/// Write 32-bit to device register at `offset`.
pub fn virtio_write32(dev: &VirtioDevice, offset: u16, val: u32) {
    match dev.transport {
        VirtioTransport::Pci => unsafe { out32(dev.base as u16 + offset, val) },
        VirtioTransport::Mmio => unsafe { mmio_write32(dev.base + offset as u64, val) },
    }
}

/// Write 16-bit to device register at `offset`.
pub fn virtio_write16(dev: &VirtioDevice, offset: u16, val: u16) {
    match dev.transport {
        VirtioTransport::Pci => unsafe { out16(dev.base as u16 + offset, val) },
        VirtioTransport::Mmio => unsafe { mmio_write32(dev.base + offset as u64, val as u32) },
    }
}

/// Write 8-bit to device register at `offset`.
pub fn virtio_write8(dev: &VirtioDevice, offset: u16, val: u8) {
    match dev.transport {
        VirtioTransport::Pci => unsafe { out8(dev.base as u16 + offset, val) },
        VirtioTransport::Mmio => unsafe { mmio_write32(dev.base + offset as u64, val as u32) },
    }
}

/// Offset of the device-specific config area for a transport.
fn dev_specific_off(transport: VirtioTransport) -> u16 {
    match transport {
        VirtioTransport::Pci => VIRTIO_DEV_SPECIFIC_OFF,
        VirtioTransport::Mmio => VIRTIO_MMIO_CONFIG,
    }
}

/// Device-specific read 32-bit: adds the config base and MSI offset.
pub fn virtio_sread32(dev: &VirtioDevice, offset: u16) -> u32 {
    let off =
        dev_specific_off(dev.transport) + if dev.msi { VIRTIO_MSI_ADD_OFF } else { 0 } + offset;
    virtio_read32(dev, off)
}

/// Device-specific read 16-bit: adds the config base and MSI offset.
pub fn virtio_sread16(dev: &VirtioDevice, offset: u16) -> u16 {
    let off =
        dev_specific_off(dev.transport) + if dev.msi { VIRTIO_MSI_ADD_OFF } else { 0 } + offset;
    virtio_read16(dev, off)
}

/// Device-specific read 8-bit: adds the config base and MSI offset.
pub fn virtio_sread8(dev: &VirtioDevice, offset: u16) -> u8 {
    let off =
        dev_specific_off(dev.transport) + if dev.msi { VIRTIO_MSI_ADD_OFF } else { 0 } + offset;
    virtio_read8(dev, off)
}

/// Initialize a vring with the given descriptor table, avail, and used rings.
///
/// Chains all descriptors into the free list as a circular singly-linked
/// list using the `next` field.
fn vring_init(
    vr: &mut Vring,
    num: u16,
    desc: &'static mut [VringDesc],
    avail: &'static mut VringAvail,
    used: &'static mut VringUsed,
) {
    vr.num = num;
    vr.desc = desc;
    vr.avail = avail;
    vr.used = used;

    // Initialize free list: chain all descriptors with VRING_DESC_F_NEXT.
    for i in 0..num {
        let i = i as usize;
        vr.desc[i].flags = VRING_DESC_F_NEXT;
        vr.desc[i].next = ((i as u16) + 1) & (num - 1);
    }
}

/// Check if the host supports a specific feature bit.
pub fn virtio_host_supports(dev: &VirtioDevice, bit: u8) -> bool {
    (dev.host_features >> bit) & 1 != 0
}

/// Exchange features between host and device.
fn exchange_features(dev: &mut VirtioDevice) {
    match dev.transport {
        VirtioTransport::Pci => exchange_features_pci(dev),
        VirtioTransport::Mmio => exchange_features_mmio(dev),
    }
}

/// Legacy PCI feature exchange: single 32-bit feature word.
fn exchange_features_pci(dev: &mut VirtioDevice) {
    let host_val = virtio_read32(dev, VIRTIO_HOST_F_OFF);
    let mut guest_features: u32 = 0;

    for f in dev.features.iter() {
        let bit = f.bit;
        guest_features |= (f.guest_support as u32) << bit;
    }

    // Store host features bitmap for later queries.
    dev.host_features = host_val as u64;

    // Write negotiated guest features to the device.
    virtio_write32(dev, VIRTIO_GUEST_F_OFF, guest_features);
}

/// Modern MMIO feature exchange: 64-bit feature space via the selector
/// registers. The high word carries `VIRTIO_F_VERSION_1`, which is
/// mandatory for modern (virtio 1.x) devices.
fn exchange_features_mmio(dev: &mut VirtioDevice) {
    let base = dev.base;
    unsafe {
        // Read device features (two 32-bit words).
        mmio_write32(base + VIRTIO_MMIO_DEVICE_FEATURES_SEL as u64, 0);
        let low = mmio_read32(base + VIRTIO_MMIO_DEVICE_FEATURES as u64);
        mmio_write32(base + VIRTIO_MMIO_DEVICE_FEATURES_SEL as u64, 1);
        let high = mmio_read32(base + VIRTIO_MMIO_DEVICE_FEATURES as u64);
        dev.host_features = ((high as u64) << 32) | low as u64;

        // Write the driver-selected low-word features.
        let mut guest_low: u32 = 0;
        for f in dev.features.iter() {
            guest_low |= (f.guest_support as u32) << f.bit;
        }
        mmio_write32(base + VIRTIO_MMIO_DRIVER_FEATURES_SEL as u64, 0);
        mmio_write32(base + VIRTIO_MMIO_DRIVER_FEATURES as u64, guest_low);
        // High word: accept VIRTIO_F_VERSION_1.
        mmio_write32(base + VIRTIO_MMIO_DRIVER_FEATURES_SEL as u64, 1);
        mmio_write32(
            base + VIRTIO_MMIO_DRIVER_FEATURES as u64,
            VIRTIO_F_VERSION_1,
        );
    }
}

/// Initialize the shared vring storage for queue 0 and return the
/// guest-physical address of the descriptor table.
fn init_vring(dev: &mut VirtioDevice, num: u16) -> u64 {
    // SAFETY: this is the only place we initialise the static vring
    // storage. We hold `&mut VirtioDevice` guaranteeing exclusive
    // access. Static storage is accessed via raw pointers.
    unsafe {
        let ring = Q0_RING.get() as usize;
        let descs: &'static mut [VringDesc] =
            core::slice::from_raw_parts_mut(ring as *mut VringDesc, num as usize);
        let avail: &'static mut VringAvail = &mut *((ring + RING_AVAIL_OFF) as *mut VringAvail);
        let used: &'static mut VringUsed = &mut *((ring + RING_USED_OFF) as *mut VringUsed);

        let q = &mut dev.queues[0];
        vring_init(&mut q.vring, num, descs, avail, used);

        q.free_num = num;
        q.free_head = 0;
        q.free_tail = num - 1;
        q.last_used = 0;

        // The ring buffer's VA is page-aligned and lives in this process's
        // loaded image; the device needs its guest-physical address.
        q.paddr = (ring as u64).wrapping_add(phys_delta() as u64);

        // Clear token store.
        let data: &mut [usize; QUEUE_NUM as usize] = &mut *Q0_DATA.get();
        for slot in data.iter_mut() {
            *slot = 0;
        }
        q.paddr
    }
}

/// Allocate and initialize device queues.
///
/// For the single-queue device, this reads the queue size from the
/// device, validates it is a power of two, initialises the vring from
/// static storage, tells the host about the queue, and resets the queue
/// data token store.
pub fn virtio_alloc_queue(dev: &mut VirtioDevice) -> Result<(), VirtioError> {
    match dev.transport {
        VirtioTransport::Pci => alloc_queue_pci(dev),
        VirtioTransport::Mmio => alloc_queue_mmio(dev),
    }
}

/// Legacy PCI queue setup: select queue 0 and write its page frame number.
fn alloc_queue_pci(dev: &mut VirtioDevice) -> Result<(), VirtioError> {
    // Select queue 0.
    virtio_write16(dev, VIRTIO_QSEL_OFF, 0);

    let qsize = virtio_read16(dev, VIRTIO_QSIZE_OFF);
    if qsize == 0 || qsize & (qsize - 1) != 0 {
        return Err(VirtioError);
    }

    let num = qsize.min(QUEUE_NUM);
    let paddr = init_vring(dev, num);

    // Write guest-physical page number (paddr >> 12) to host.
    unsafe { out32(dev.base as u16 + VIRTIO_QADDR_OFF, (paddr >> 12) as u32) };

    dev.num_queues = 1;
    Ok(())
}

/// Modern MMIO queue setup: select queue 0, program the descriptor/
/// avail/used ring addresses, then hand the queue to the host with
/// `QueueReady`.
fn alloc_queue_mmio(dev: &mut VirtioDevice) -> Result<(), VirtioError> {
    let base = dev.base;
    unsafe {
        mmio_write32(base + VIRTIO_MMIO_QUEUE_SEL as u64, 0);
        let num_max = mmio_read32(base + VIRTIO_MMIO_QUEUE_NUM_MAX as u64);
        if num_max == 0 {
            return Err(VirtioError);
        }
        let num = num_max.min(QUEUE_NUM as u32) as u16;
        if num == 0 || num & (num - 1) != 0 {
            return Err(VirtioError);
        }
        mmio_write32(base + VIRTIO_MMIO_QUEUE_NUM as u64, num as u32);

        let paddr = init_vring(dev, num);
        let avail_pa = paddr + RING_AVAIL_OFF as u64;
        let used_pa = paddr + RING_USED_OFF as u64;

        mmio_write32(base + VIRTIO_MMIO_QUEUE_DESC_LOW as u64, paddr as u32);
        mmio_write32(
            base + VIRTIO_MMIO_QUEUE_DESC_HIGH as u64,
            (paddr >> 32) as u32,
        );
        mmio_write32(base + VIRTIO_MMIO_QUEUE_AVAIL_LOW as u64, avail_pa as u32);
        mmio_write32(
            base + VIRTIO_MMIO_QUEUE_AVAIL_HIGH as u64,
            (avail_pa >> 32) as u32,
        );
        mmio_write32(base + VIRTIO_MMIO_QUEUE_USED_LOW as u64, used_pa as u32);
        mmio_write32(
            base + VIRTIO_MMIO_QUEUE_USED_HIGH as u64,
            (used_pa >> 32) as u32,
        );

        // The host must observe the ring addresses before the queue is
        // marked ready.
        crate::hal::mfence();
        mmio_write32(base + VIRTIO_MMIO_QUEUE_READY as u64, 1);
    }
    dev.num_queues = 1;
    Ok(())
}

/// Kick the device: notify it that the selected queue has new descriptors.
fn queue_notify(dev: &VirtioDevice) {
    match dev.transport {
        VirtioTransport::Pci => unsafe { out16(dev.base as u16 + VIRTIO_QNOTIFY_OFF, 0) },
        VirtioTransport::Mmio => unsafe {
            mmio_write32(dev.base + VIRTIO_MMIO_QUEUE_NOTIFY as u64, 0)
        },
    }
}

/// Fill a single vring descriptor from a `VirtioPhysBuf`.
///
/// The LSB of `vp.addr` is used as the writable flag; the actual
/// address is `vp.addr & !1`.
fn use_vring_desc(vd: &mut VringDesc, vp: &VirtioPhysBuf) {
    // Translate the guest VA to the guest-physical address the device
    // DMAs to, then keep the writable flag bit from `vp.addr`.
    vd.addr = (vp.addr & !1u64).wrapping_add(phys_delta() as u64);
    vd.len = vp.size;
    vd.flags = VRING_DESC_F_NEXT;
    if vp.addr & 1 != 0 {
        vd.flags |= VRING_DESC_F_WRITE;
    }
}

/// Chain `num_bufs` descriptors starting at `free_head`.
/// Descriptors must already be filled by `fill_descriptors`.
///
/// Follows the actual `next` pointers (the free list may wrap around the
/// ring, e.g. head 255 → 0 → 1), and clears the NEXT flag on the true
/// last descriptor of the chain.
///
/// Returns the new free_head after consuming descriptors.
fn chain_descriptors(vring: &mut Vring, free_head: u16, num_bufs: usize) -> u16 {
    let mut i = free_head;
    let mut last = i;

    for _ in 0..num_bufs {
        let vd = &vring.desc[i as usize];
        last = i;
        i = vd.next;
    }

    // Unset NEXT flag on the last descriptor in the chain.
    vring.desc[last as usize].flags &= !VRING_DESC_F_NEXT;

    i
}

/// Apply `use_vring_desc` to a range of descriptors starting at `start`.
fn fill_descriptors(vring: &mut Vring, start: u16, bufs: &[VirtioPhysBuf]) {
    let mut i = start;
    for buf in bufs {
        let vd = &mut vring.desc[i as usize];
        use_vring_desc(vd, buf);
        i = vd.next;
    }
}

/// Submit buffers to queue 0.
///
/// Chains the provided physical buffers as descriptors in the vring,
/// places the head descriptor index into the avail ring, and kicks the
/// host.
///
/// `data` is an opaque token returned by `virtio_from_queue` when the
/// host completes the descriptor chain.
pub fn virtio_to_queue(
    dev: &mut VirtioDevice,
    bufs: &[VirtioPhysBuf],
    data: usize,
) -> Result<(), VirtioError> {
    if dev.num_queues == 0 {
        return Err(VirtioError);
    }

    let num_bufs = bufs.len();
    if num_bufs == 0 {
        return Err(VirtioError);
    }

    // All queue operations in a single borrow scope to avoid aliasing.
    let need_kick = {
        let q = &mut dev.queues[0];
        if q.free_num < num_bufs as u16 {
            return Err(VirtioError);
        }

        let vring = &mut q.vring;
        let head = q.free_head;

        // Fill descriptors with buffer data.
        fill_descriptors(vring, head, bufs);

        // Chain them and get the new free head.
        let new_head = chain_descriptors(vring, head, num_bufs);
        q.free_head = new_head;
        q.free_num -= num_bufs as u16;

        // Place the head descriptor into the avail ring.
        let avail_idx = vring.avail.idx % vring.num;
        vring.avail.ring[avail_idx as usize] = head;

        // Store the data token.
        unsafe {
            (*Q0_DATA.get())[head as usize] = data;
        }

        // Memory barrier: host must see descriptor writes before
        // the avail index update.
        unsafe {
            crate::hal::mfence();
        }

        // Advance the avail index.
        vring.avail.idx = vring.avail.idx.wrapping_add(1);

        // Memory barrier: host must see updated avail index before kick.
        unsafe {
            crate::hal::mfence();
        }

        // Check if the host wants notification.
        vring.used.flags & VRING_USED_F_NO_NOTIFY == 0
    };

    // Kick outside the queue borrow to avoid aliasing with `dev`.
    if need_kick {
        queue_notify(dev);
    }

    Ok(())
}

/// Reap a completed descriptor from queue 0.
///
/// Returns the data token that was provided to `virtio_to_queue` if the
/// host has processed a descriptor chain, or `None` if nothing is done.
pub fn virtio_from_queue(dev: &mut VirtioDevice) -> Option<usize> {
    if dev.num_queues == 0 {
        return None;
    }

    // Ensure we see the host's writes.
    unsafe {
        crate::hal::mfence();
    }

    // All queue operations in one borrow scope.
    {
        let q = &mut dev.queues[0];
        let vring = &mut q.vring;
        let num = vring.num;

        let used_idx = vring.used.idx % num;

        // Nothing new from the host.
        if q.last_used == used_idx {
            return None;
        }

        // Get the used element at the current `last_used` position.
        let uel = &vring.used.ring[q.last_used as usize];
        q.last_used = (q.last_used + 1) % num;

        let idx = (uel.id as u16) % num;
        let mut count: u16 = 0;

        // Reclaim descriptors: link the chain back into the free list.
        // Walk from `idx` following `next` until we find one without
        // VRING_DESC_F_NEXT.
        let mut cur = idx;

        // Attach the reclaimed chain to the tail of the free list.
        vring.desc[q.free_tail as usize].next = idx;

        loop {
            count += 1;
            let vd = &vring.desc[cur as usize];
            if vd.flags & VRING_DESC_F_NEXT == 0 {
                break;
            }
            cur = vd.next;
        }

        // `cur` now points to the last descriptor in the chain.
        q.free_tail = cur;

        // Link the reclaimed chain back to the old free head, making it
        // circular again.
        vring.desc[q.free_tail as usize].next = q.free_head;
        vring.desc[q.free_tail as usize].flags = VRING_DESC_F_NEXT;

        q.free_num = q.free_num.wrapping_add(count);

        // Retrieve the data token.
        let tok = unsafe { (*Q0_DATA.get())[idx as usize] };
        unsafe {
            (*Q0_DATA.get())[idx as usize] = 0;
        }

        Some(tok)
    }
}

/// Check if the device has asserted an interrupt.
///
/// Reads the ISR status register. Returns `true` if the interrupt was
/// for this device.
pub fn virtio_had_irq(dev: &VirtioDevice) -> bool {
    match dev.transport {
        VirtioTransport::Pci => virtio_read8(dev, VIRTIO_ISR_STATUS_OFF) & 1 != 0,
        VirtioTransport::Mmio => virtio_read32(dev, VIRTIO_MMIO_INTERRUPT_STATUS) & 1 != 0,
    }
}

/// Re-enable interrupts for this device.
///
/// In the legacy virtio model, reading the ISR status re-enables
/// interrupts. This is a no-op in the current implementation;
/// platform-specific IRQ re-enable logic should be added here.
pub fn virtio_irq_enable(_dev: &mut VirtioDevice) {
    // On real hardware, re-enable the IRQ line at the PIC/IOAPIC.
    // Legacy virtio re-enables interrupts by reading ISR status,
    // which `virtio_had_irq` already does.
}

/// Disable interrupts for this device.
///
/// Platform-specific IRQ masking should be added here.
pub fn virtio_irq_disable(_dev: &mut VirtioDevice) {
    // On real hardware, disable the IRQ line at the PIC/IOAPIC.
}

/// Probe for a virtio device with the given device ID.
///
/// Dispatches to the platform transport: legacy PCI (x86_64) or modern
/// virtio-mmio (RISC-V/AArch64). `subdevid` is the PCI subsystem device
/// ID on the PCI path and the virtio device ID on the MMIO path (both
/// are 2 for virtio-blk).
///
/// The `skip` parameter allows selecting the Nth matching device.
pub fn virtio_probe(
    subdevid: u16,
    name: &'static str,
    features: &'static [VirtioFeature],
    skip: u16,
) -> Result<VirtioDevice, VirtioError> {
    match current_transport() {
        VirtioTransport::Pci => pci_probe(subdevid, name, features, skip),
        VirtioTransport::Mmio => mmio_probe(subdevid, name, features, skip),
    }
}

/// The virtio transport for the current architecture: legacy PCI where
/// I/O ports exist, modern MMIO elsewhere.
fn current_transport() -> VirtioTransport {
    #[cfg(target_arch = "x86_64")]
    {
        VirtioTransport::Pci
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        VirtioTransport::Mmio
    }
}

/// Probe PCI bus 0 for a legacy virtio device with the given subsystem
/// device ID. Returns an initialized `VirtioDevice` on success.
fn pci_probe(
    subdevid: u16,
    name: &'static str,
    features: &'static [VirtioFeature],
    skip: u16,
) -> Result<VirtioDevice, VirtioError> {
    let mut found_skip = skip;

    for dev in 0..32u8 {
        for func in 0..8u8 {
            // SAFETY: PCI config access is inherently unsafe but we
            // serialise within this loop.
            let vendor = unsafe { pci_cfg_read16(0, dev, func, 0x00) };

            // Skip non-existent devices.
            if vendor == 0xFFFF || vendor == 0 {
                if func == 0 {
                    let header = unsafe { pci_cfg_read8(0, dev, 0, 0x0E) };
                    if header & 0x80 == 0 {
                        break;
                    }
                }
                continue;
            }

            if vendor != VIRTIO_PCI_VENDOR {
                if func == 0 {
                    let header = unsafe { pci_cfg_read8(0, dev, 0, 0x0E) };
                    if header & 0x80 == 0 {
                        break;
                    }
                }
                continue;
            }

            // Read subsystem device ID (PCI offset 0x2E).
            let sdid = unsafe { pci_cfg_read16(0, dev, func, 0x2E) };

            if sdid != subdevid {
                if func == 0 {
                    let header = unsafe { pci_cfg_read8(0, dev, 0, 0x0E) };
                    if header & 0x80 == 0 {
                        break;
                    }
                }
                continue;
            }

            // Found a matching device.
            if found_skip > 0 {
                found_skip -= 1;
                continue;
            }

            // Read BAR0: must be I/O space.
            let bar0 = unsafe { pci_cfg_read32(0, dev, func, 0x10) };
            if bar0 & 1 == 0 {
                return Err(VirtioError);
            }

            let base = (bar0 & !0x3) as u64;

            // Read IRQ line (PCI offset 0x3F).
            let irq = unsafe { pci_cfg_read8(0, dev, func, 0x3F) };

            // Build a temporary device for register access.
            let mut device = VirtioDevice {
                transport: VirtioTransport::Pci,
                base,
                name,
                features,
                host_features: 0,
                queues: [VirtioQueue {
                    vring: Vring {
                        num: 0,
                        desc: &mut [],
                        avail: unsafe {
                            &mut *((Q0_RING.get() as usize + RING_AVAIL_OFF) as *mut VringAvail)
                        },
                        used: unsafe {
                            &mut *((Q0_RING.get() as usize + RING_USED_OFF) as *mut VringUsed)
                        },
                    },
                    paddr: 0,
                    free_num: 0,
                    free_head: 0,
                    free_tail: 0,
                    last_used: 0,
                }],
                num_queues: 0,
                irq,
                msi: false,
                initialized: false,
            };

            // Reset the device.
            virtio_write8(&device, VIRTIO_DEV_STATUS_OFF, 0);

            // Set ACK status.
            virtio_write8(&device, VIRTIO_DEV_STATUS_OFF, VIRTIO_STATUS_ACK);

            // Exchange features (needs mutable access).
            exchange_features(&mut device);

            // Set DRV status.
            virtio_write8(&device, VIRTIO_DEV_STATUS_OFF, VIRTIO_STATUS_DRV);

            device.initialized = true;
            return Ok(device);
        }
    }

    Err(VirtioError)
}

/// Probe the QEMU virtio-mmio transports at their fixed machine addresses
/// for a modern (virtio 1.x) device with the given device ID.
fn mmio_probe(
    dev_id: u16,
    name: &'static str,
    features: &'static [VirtioFeature],
    skip: u16,
) -> Result<VirtioDevice, VirtioError> {
    let mut found_skip = skip;
    for n in 0..VIRTIO_MMIO_NUM_TRANSPORTS {
        let base = VIRTIO_MMIO_BASE + n * VIRTIO_MMIO_STRIDE;
        unsafe {
            // The magic register identifies a live virtio-mmio transport.
            if mmio_read32(base + VIRTIO_MMIO_MAGIC_VALUE as u64) != VIRTIO_MMIO_MAGIC {
                continue;
            }
            // Only the modern (virtio 1.x) interface is supported here.
            if mmio_read32(base + VIRTIO_MMIO_VERSION as u64) != VIRTIO_MMIO_VERSION_MODERN {
                continue;
            }
            if mmio_read32(base + VIRTIO_MMIO_DEVICE_ID as u64) as u16 != dev_id {
                continue;
            }
            if found_skip > 0 {
                found_skip -= 1;
                continue;
            }

            let mut device = VirtioDevice {
                transport: VirtioTransport::Mmio,
                base,
                name,
                features,
                host_features: 0,
                queues: [VirtioQueue {
                    vring: Vring {
                        num: 0,
                        desc: &mut [],
                        avail: &mut *((Q0_RING.get() as usize + RING_AVAIL_OFF) as *mut VringAvail),
                        used: &mut *((Q0_RING.get() as usize + RING_USED_OFF) as *mut VringUsed),
                    },
                    paddr: 0,
                    free_num: 0,
                    free_head: 0,
                    free_tail: 0,
                    last_used: 0,
                }],
                num_queues: 0,
                irq: 0,
                msi: false,
                initialized: false,
            };

            // Reset, then ACK → DRIVER → negotiate → FEATURES_OK.
            mmio_write32(base + VIRTIO_MMIO_STATUS as u64, 0);
            mmio_write32(base + VIRTIO_MMIO_STATUS as u64, VIRTIO_STATUS_ACK as u32);
            mmio_write32(
                base + VIRTIO_MMIO_STATUS as u64,
                (VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRV) as u32,
            );

            exchange_features(&mut device);

            // The host validates the negotiated feature set when
            // FEATURES_OK is set and clears the bit on failure.
            crate::hal::mfence();
            mmio_write32(
                base + VIRTIO_MMIO_STATUS as u64,
                (VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRV | VIRTIO_STATUS_FEATURES_OK) as u32,
            );
            if mmio_read32(base + VIRTIO_MMIO_STATUS as u64) & VIRTIO_STATUS_FEATURES_OK as u32 == 0
            {
                return Err(VirtioError);
            }

            device.initialized = true;
            return Ok(device);
        }
    }

    Err(VirtioError)
}

/// Set the device ready.
///
/// Sets the `DRV_OK` status bit, signalling to the host that the driver
/// is fully operational.
pub fn virtio_device_ready(dev: &mut VirtioDevice) {
    match dev.transport {
        VirtioTransport::Pci => virtio_write8(dev, VIRTIO_DEV_STATUS_OFF, VIRTIO_STATUS_DRV_OK),
        VirtioTransport::Mmio => unsafe {
            let base = dev.base;
            let status = mmio_read32(base + VIRTIO_MMIO_STATUS as u64);
            // OR in DRV_OK, preserving the ACK|DRV|FEATURES_OK bits.
            mmio_write32(
                base + VIRTIO_MMIO_STATUS as u64,
                status | VIRTIO_STATUS_DRV_OK as u32,
            );
        },
    }
}

/// Reset the device.
///
/// Clears the device status (writing 0), which triggers a device reset.
pub fn virtio_reset_device(dev: &mut VirtioDevice) {
    match dev.transport {
        VirtioTransport::Pci => virtio_write8(dev, VIRTIO_DEV_STATUS_OFF, 0),
        VirtioTransport::Mmio => unsafe { mmio_write32(dev.base + VIRTIO_MMIO_STATUS as u64, 0) },
    }
    dev.initialized = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build a test-only Vring from local buffers by
    /// extending their lifetime to `'static`.
    /// SAFETY: Only valid in single-threaded test code.
    fn make_test_vring(
        num: u16,
        descs: &mut [VringDesc],
        avail: &mut VringAvail,
        used: &mut VringUsed,
    ) -> Vring {
        let descs: &'static mut [VringDesc] = unsafe { &mut *(descs as *mut [VringDesc]) };
        let avail: &'static mut VringAvail = unsafe { &mut *(avail as *mut VringAvail) };
        let used: &'static mut VringUsed = unsafe { &mut *(used as *mut VringUsed) };

        // Use dummy init values; vring_init will overwrite them.
        let dummy = unsafe { &mut *((Q0_RING.get() as usize + RING_AVAIL_OFF) as *mut VringAvail) };
        let mut vr = Vring {
            num: 0,
            desc: &mut [], // vring_init overwrites this
            avail: dummy,  // vring_init overwrites this
            used: unsafe { &mut *((Q0_RING.get() as usize + RING_USED_OFF) as *mut VringUsed) },
        };
        vring_init(&mut vr, num, descs, avail, used);
        vr
    }

    #[test]
    fn test_virtio_constants() {
        assert_eq!(VIRTIO_HOST_F_OFF, 0x0000);
        assert_eq!(VIRTIO_GUEST_F_OFF, 0x0004);
        assert_eq!(VIRTIO_QADDR_OFF, 0x0008);
        assert_eq!(VIRTIO_QSIZE_OFF, 0x000C);
        assert_eq!(VIRTIO_QSEL_OFF, 0x000E);
        assert_eq!(VIRTIO_QNOTIFY_OFF, 0x0010);
        assert_eq!(VIRTIO_DEV_STATUS_OFF, 0x0012);
        assert_eq!(VIRTIO_ISR_STATUS_OFF, 0x0013);
        assert_eq!(VIRTIO_DEV_SPECIFIC_OFF, 0x0014);

        assert_eq!(VIRTIO_STATUS_ACK, 0x01);
        assert_eq!(VIRTIO_STATUS_DRV, 0x02);
        assert_eq!(VIRTIO_STATUS_DRV_OK, 0x04);
        assert_eq!(VIRTIO_STATUS_FAIL, 0x80);

        assert_eq!(VRING_DESC_F_NEXT, 1);
        assert_eq!(VRING_DESC_F_WRITE, 2);
        assert_eq!(VRING_DESC_F_INDIRECT, 4);

        assert_eq!(VIRTIO_PCI_VENDOR, 0x1AF4);
    }

    #[test]
    fn test_type_sizes() {
        assert_eq!(size_of::<VringDesc>(), 16);
        assert_eq!(size_of::<VringAvail>(), 516);
        assert_eq!(size_of::<VringUsedElem>(), 8);
        assert_eq!(size_of::<VringUsed>(), 4 + 256 * 8);
        assert_eq!(size_of::<VirtioPhysBuf>(), 16);
    }

    #[test]
    fn test_vring_init() {
        let mut raw_descs = [VringDesc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }; 16];
        let mut raw_avail = VringAvail {
            flags: 0,
            idx: 0,
            ring: [0; 256],
        };
        let mut raw_used = VringUsed {
            flags: 0,
            idx: 0,
            ring: [VringUsedElem { id: 0, len: 0 }; 256],
        };

        let vr = make_test_vring(16, &mut raw_descs, &mut raw_avail, &mut raw_used);

        assert_eq!(vr.num, 16);
        assert_eq!(vr.desc.len(), 16);

        // Each descriptor should be in the free list with NEXT flag.
        for i in 0..16 {
            assert_eq!(
                vr.desc[i].flags, VRING_DESC_F_NEXT,
                "desc[{i}] should have NEXT flag"
            );
            assert_eq!(
                vr.desc[i].next,
                ((i as u16) + 1) & 15,
                "desc[{i}].next should point to next free descriptor"
            );
        }

        // Avail and used should be zeroed.
        assert_eq!(vr.avail.flags, 0);
        assert_eq!(vr.avail.idx, 0);
        assert_eq!(vr.used.flags, 0);
        assert_eq!(vr.used.idx, 0);
    }

    /// Test that `use_vring_desc` correctly strips the LSB and sets
    /// the WRITE flag.
    #[test]
    fn test_use_vring_desc_readable() {
        let mut vd = VringDesc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        };

        let buf = VirtioPhysBuf {
            addr: 0x1000,
            size: 512,
        };

        use_vring_desc(&mut vd, &buf);
        assert_eq!(vd.addr, 0x1000);
        assert_eq!(vd.len, 512);
        assert_eq!(vd.flags, VRING_DESC_F_NEXT);
    }

    /// Test the writable flag via LSB.
    #[test]
    fn test_use_vring_desc_writable() {
        let mut vd = VringDesc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        };

        let buf = VirtioPhysBuf {
            addr: 0x2001,
            size: 256,
        };

        use_vring_desc(&mut vd, &buf);
        assert_eq!(vd.addr, 0x2000);
        assert_eq!(vd.len, 256);
        assert_eq!(vd.flags, VRING_DESC_F_NEXT | VRING_DESC_F_WRITE);
    }

    /// Simulate setting up direct descriptors from physical buffers.
    #[test]
    fn test_chain_descriptors() {
        let mut raw_descs = [VringDesc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }; 16];
        let mut raw_avail = VringAvail {
            flags: 0,
            idx: 0,
            ring: [0; 256],
        };
        let mut raw_used = VringUsed {
            flags: 0,
            idx: 0,
            ring: [VringUsedElem { id: 0, len: 0 }; 256],
        };

        let mut vr = make_test_vring(16, &mut raw_descs, &mut raw_avail, &mut raw_used);

        // Fill descriptors first with buffer data.
        let bufs = [
            VirtioPhysBuf {
                addr: 0x3000,
                size: 64,
            },
            VirtioPhysBuf {
                addr: 0x4001,
                size: 128,
            },
            VirtioPhysBuf {
                addr: 0x5000,
                size: 32,
            },
        ];

        fill_descriptors(&mut vr, 0, &bufs);

        // Now chain them.
        let new_head = chain_descriptors(&mut vr, 0, 3);
        assert_eq!(new_head, 3);

        // Check descriptor 0
        assert_eq!(vr.desc[0].addr, 0x3000);
        assert_eq!(vr.desc[0].len, 64);
        assert_eq!(vr.desc[0].flags, VRING_DESC_F_NEXT);

        // Check descriptor 1 (writable)
        assert_eq!(vr.desc[1].addr, 0x4000);
        assert_eq!(vr.desc[1].len, 128);
        assert_eq!(vr.desc[1].flags, VRING_DESC_F_NEXT | VRING_DESC_F_WRITE);

        // Descriptor 2 is the last -> no NEXT
        assert_eq!(vr.desc[2].addr, 0x5000);
        assert_eq!(vr.desc[2].len, 32);
        assert_eq!(vr.desc[2].flags, 0);
    }

    /// Regression test: the free list is not contiguous when it wraps
    /// (head 255 → 0 → 1). The old code computed the last descriptor as
    /// `free_head + num_bufs - 1` (index 257) and panicked; the chain
    /// must be found by walking the actual `next` pointers.
    #[test]
    fn test_chain_descriptors_wraps_ring() {
        let mut raw_descs = [VringDesc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }; 256];
        let mut raw_avail = VringAvail {
            flags: 0,
            idx: 0,
            ring: [0; 256],
        };
        let mut raw_used = VringUsed {
            flags: 0,
            idx: 0,
            ring: [VringUsedElem { id: 0, len: 0 }; 256],
        };

        let mut vr = make_test_vring(256, &mut raw_descs, &mut raw_avail, &mut raw_used);

        // vring_init links 255 → 0 → 1 → 2 → ... → 255, so a chain
        // starting at head 255 wraps around the ring.
        let bufs = [
            VirtioPhysBuf {
                addr: 0x3000,
                size: 64,
            },
            VirtioPhysBuf {
                addr: 0x4001,
                size: 128,
            },
            VirtioPhysBuf {
                addr: 0x5000,
                size: 32,
            },
        ];

        fill_descriptors(&mut vr, 255, &bufs);
        let new_head = chain_descriptors(&mut vr, 255, 3);
        assert_eq!(new_head, 2);

        // desc[255] is the chain head (middle -> keeps NEXT).
        assert_eq!(vr.desc[255].addr, 0x3000);
        assert_eq!(vr.desc[255].next, 0);
        assert_eq!(vr.desc[255].flags, VRING_DESC_F_NEXT);
        // desc[0] is the second link (writable).
        assert_eq!(vr.desc[0].addr, 0x4000);
        assert_eq!(vr.desc[0].next, 1);
        assert_eq!(vr.desc[0].flags, VRING_DESC_F_NEXT | VRING_DESC_F_WRITE);
        // desc[1] is the last link -> NEXT cleared.
        assert_eq!(vr.desc[1].addr, 0x5000);
        assert_eq!(vr.desc[1].flags, 0);
        // The next free descriptor is untouched.
        assert_eq!(vr.desc[2].flags, VRING_DESC_F_NEXT);
        assert_eq!(vr.desc[2].next, 3);
    }

    /// Simulate a full to-queue / from-queue cycle.
    #[test]
    fn test_descriptor_cycle() {
        let mut raw_descs = [VringDesc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }; 16];
        let mut raw_avail = VringAvail {
            flags: 0,
            idx: 0,
            ring: [0; 256],
        };
        let mut raw_used = VringUsed {
            flags: 0,
            idx: 0,
            ring: [VringUsedElem { id: 0, len: 0 }; 256],
        };

        // Cast the test buffers to 'static lifetime for vring init.
        // SAFETY: The test owns these buffers for its duration.
        let descs: &'static mut [VringDesc] =
            unsafe { &mut *(&mut raw_descs[..] as *mut [VringDesc]) };
        let avail: &'static mut VringAvail = unsafe { &mut *(&mut raw_avail as *mut VringAvail) };
        let used: &'static mut VringUsed = unsafe { &mut *(&mut raw_used as *mut VringUsed) };

        // Use dummy init values; vring_init will overwrite them.
        let mut v = Vring {
            num: 0,
            desc: &mut [],
            avail: unsafe { &mut *((Q0_RING.get() as usize + RING_AVAIL_OFF) as *mut VringAvail) },
            used: unsafe { &mut *((Q0_RING.get() as usize + RING_USED_OFF) as *mut VringUsed) },
        };
        vring_init(&mut v, 16, descs, avail, used);

        let mut q = VirtioQueue {
            vring: v,
            paddr: 0,
            free_num: 16,
            free_head: 0,
            free_tail: 15,
            last_used: 0,
        };

        // Submit a single-buffer descriptor chain.
        let bufs = [VirtioPhysBuf {
            addr: 0x6000,
            size: 256,
        }];

        // Manually simulate to_queue logic.
        let vring = &mut q.vring;
        let free_first = q.free_head;

        fill_descriptors(vring, free_first, &bufs);
        let new_head = chain_descriptors(vring, free_first, 1);
        q.free_head = new_head;
        q.free_num -= 1;

        // Place into avail ring.
        let avail_idx = vring.avail.idx % vring.num;
        vring.avail.ring[avail_idx as usize] = free_first;
        vring.avail.idx = vring.avail.idx.wrapping_add(1);

        assert_eq!(q.free_num, 15);
        assert_eq!(q.free_head, 1);

        // Simulate the host processing and placing into used ring.
        let used_idx = vring.used.idx as usize;
        vring.used.ring[used_idx] = VringUsedElem {
            id: free_first as u32,
            len: 256,
        };
        vring.used.idx = vring.used.idx.wrapping_add(1);

        // Now simulate from_queue logic.
        let num = vring.num;
        let new_used_idx = vring.used.idx % num;
        assert_ne!(q.last_used, new_used_idx);

        let uel = &vring.used.ring[q.last_used as usize];
        q.last_used = (q.last_used + 1) % num;

        let idx = (uel.id as u16) % num;
        let mut count: u16 = 0;

        // Reclaim descriptors.
        vring.desc[q.free_tail as usize].next = idx;
        let mut cur = idx;
        loop {
            count += 1;
            let vd = &vring.desc[cur as usize];
            if vd.flags & VRING_DESC_F_NEXT == 0 {
                break;
            }
            cur = vd.next;
        }

        q.free_tail = cur;
        vring.desc[q.free_tail as usize].next = q.free_head;
        vring.desc[q.free_tail as usize].flags = VRING_DESC_F_NEXT;
        q.free_num = q.free_num.wrapping_add(count);

        // All descriptors should be back in the free list.
        assert_eq!(q.free_num, 16);
        assert_eq!(q.free_tail, 0);
    }

    /// Regression test: a full to-queue / from-queue cycle with a free
    /// list that wraps (255 → 0 → 1 → 2). Exercises the same wrap in the
    /// chain walk and in the reclaim path, and checks the free list is
    /// restored as a circular chain afterwards.
    #[test]
    fn test_descriptor_cycle_wraps_ring() {
        let mut raw_descs = [VringDesc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }; 256];
        let mut raw_avail = VringAvail {
            flags: 0,
            idx: 0,
            ring: [0; 256],
        };
        let mut raw_used = VringUsed {
            flags: 0,
            idx: 0,
            ring: [VringUsedElem { id: 0, len: 0 }; 256],
        };

        let vr = make_test_vring(256, &mut raw_descs, &mut raw_avail, &mut raw_used);

        // Free list of four slots wrapping around the ring:
        // 255 → 0 → 1 → 2 → 255. vring_init already links 255 → 0 → 1 → 2;
        // close the circle at the tail (this is what the reclaim path in
        // virtio_from_queue produces after earlier cycles).
        vr.desc[2].next = 255;

        let mut q = VirtioQueue {
            vring: vr,
            paddr: 0,
            free_num: 4,
            free_head: 255,
            free_tail: 2,
            last_used: 0,
        };

        // Submit a 3-buffer chain starting at head 255 (wraps the ring).
        let bufs = [
            VirtioPhysBuf {
                addr: 0x6000,
                size: 256,
            },
            VirtioPhysBuf {
                addr: 0x7001,
                size: 512,
            },
            VirtioPhysBuf {
                addr: 0x8000,
                size: 128,
            },
        ];
        let vring = &mut q.vring;
        let head = q.free_head;
        fill_descriptors(vring, head, &bufs);
        let new_head = chain_descriptors(vring, head, 3);
        q.free_head = new_head;
        q.free_num -= 3;

        let avail_idx = vring.avail.idx % vring.num;
        vring.avail.ring[avail_idx as usize] = head;
        vring.avail.idx = vring.avail.idx.wrapping_add(1);

        assert_eq!(q.free_num, 1);
        assert_eq!(q.free_head, 2);

        // Simulate the host completing the chain.
        let used_idx = vring.used.idx as usize;
        vring.used.ring[used_idx] = VringUsedElem {
            id: head as u32,
            len: 256,
        };
        vring.used.idx = vring.used.idx.wrapping_add(1);

        // Reclaim (mirrors virtio_from_queue logic).
        let num = vring.num;
        let new_used_idx = vring.used.idx % num;
        assert_ne!(q.last_used, new_used_idx);

        let uel = &vring.used.ring[q.last_used as usize];
        q.last_used = (q.last_used + 1) % num;

        let idx = (uel.id as u16) % num;
        let mut count: u16 = 0;
        vring.desc[q.free_tail as usize].next = idx;
        let mut cur = idx;
        loop {
            count += 1;
            let vd = &vring.desc[cur as usize];
            if vd.flags & VRING_DESC_F_NEXT == 0 {
                break;
            }
            cur = vd.next;
        }

        q.free_tail = cur;
        vring.desc[q.free_tail as usize].next = q.free_head;
        vring.desc[q.free_tail as usize].flags = VRING_DESC_F_NEXT;
        q.free_num = q.free_num.wrapping_add(count);

        // All four slots are free again and the list is circular:
        // 2 → 255 → 0 → 1 → 2.
        assert_eq!(q.free_num, 4);
        assert_eq!(q.free_tail, 1);
        assert_eq!(q.free_head, 2);
        assert_eq!(vring.desc[2].next, 255);
        assert_eq!(vring.desc[255].next, 0);
        assert_eq!(vring.desc[0].next, 1);
        assert_eq!(vring.desc[1].next, 2);
        for i in [2u16, 255, 0, 1] {
            assert_eq!(
                vring.desc[i as usize].flags & VRING_DESC_F_NEXT,
                VRING_DESC_F_NEXT
            );
        }
    }

    #[test]
    fn test_virtio_host_supports_with_bitmap() {
        let dev = VirtioDevice {
            transport: VirtioTransport::Pci,
            base: 0,
            name: "test",
            features: &[],
            host_features: 1u64 << 28,
            queues: [VirtioQueue {
                vring: Vring {
                    num: 0,
                    desc: &mut [],
                    avail: unsafe {
                        &mut *((Q0_RING.get() as usize + RING_AVAIL_OFF) as *mut VringAvail)
                    },
                    used: unsafe {
                        &mut *((Q0_RING.get() as usize + RING_USED_OFF) as *mut VringUsed)
                    },
                },
                paddr: 0,
                free_num: 0,
                free_head: 0,
                free_tail: 0,
                last_used: 0,
            }],
            num_queues: 0,
            irq: 0,
            msi: false,
            initialized: true,
        };

        assert!(virtio_host_supports(&dev, 28));
        assert!(!virtio_host_supports(&dev, 29));
        assert!(!virtio_host_supports(&dev, 0));
    }

    #[test]
    fn test_virtio_error_is_copy() {
        let e = VirtioError;
        let _e2 = e;
        assert_eq!(e, _e2);
    }

    #[test]
    fn test_virtio_error_debug() {
        fn assert_debug<T: core::fmt::Debug>(_: &T) {}
        let e = VirtioError;
        assert_debug(&e);
    }

    #[test]
    fn test_pci_config_addr() {
        let addr = pci_config_addr(0, 0, 0, 0x00);
        assert_eq!(addr, 0x8000_0000);

        let addr = pci_config_addr(0, 1, 0, 0x10);
        // The function includes the dword-aligned register offset.
        assert_eq!(addr, 0x8000_0810);

        let addr = pci_config_addr(1, 2, 3, 0x2E);
        // reg=0x2E is aligned to 0x2C (0x2E & 0xFC).
        assert_eq!(addr, 0x8001_132C);
    }

    #[test]
    fn test_vringdesc_default_is_zeroed() {
        let d = VringDesc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        };
        assert_eq!(d.addr, 0);
        assert_eq!(d.len, 0);
        assert_eq!(d.flags, 0);
        assert_eq!(d.next, 0);
    }

    #[test]
    fn test_virtiophysical_default() {
        let b = VirtioPhysBuf {
            addr: 0xABCD0011,
            size: 1024,
        };
        assert_eq!(b.addr, 0xABCD0011);
        assert_eq!(b.size, 1024);
    }
}
