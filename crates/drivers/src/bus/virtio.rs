//! Virtio PCI legacy transport layer.
//!
//! Ported from `.refs/minix-3.3.0/minix/lib/libvirtio/virtio.c`
//!
//! Provides a safe abstraction for legacy virtio-over-PCI devices using
//! I/O port BARs. Uses fixed-size static arrays for vring storage (no
//! heap allocation required).
//!
//! # Safety
//!
//! This module performs raw I/O port access and manipulates mutable
//! statics via `core::ptr::addr_of_mut!()` per Rust 2024
//! `deny(static_mut_refs)`. All `unsafe` blocks are documented with
//! their invariants.

#![allow(dead_code)]

use core::arch::asm;
use core::ptr::addr_of_mut;

#[cfg(test)]
use core::mem::size_of;

// ── Error type ─────────────────────────────────────────────────────────────────

/// Error type for virtio operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioError;

// ── Constants ──────────────────────────────────────────────────────────────────

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

/// PCI configuration ports.
const PCI_ADDR_PORT: u16 = 0xCF8;
const PCI_DATA_PORT: u16 = 0xCFC;

/// Maximum number of descriptors per queue.
const QUEUE_NUM: u16 = 256;

// ── Types ──────────────────────────────────────────────────────────────────────

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
/// Contains PCI I/O port base, feature list, a single virtqueue backed
/// by static vring storage, IRQ line, and initialization state.
pub struct VirtioDevice {
    pub port: u16,
    pub name: &'static str,
    pub features: &'static [VirtioFeature],
    /// Bitmap of host-supported features (set during `exchange_features`).
    pub host_features: u32,
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

// ── Static vring storage ───────────────────────────────────────────────────────
//
// Pre-allocated storage for one virtqueue. Access is always through
// `core::ptr::addr_of_mut!()` — never create direct references to
// mutable statics per Rust 2024 rules.

static mut Q0_DESCS: [VringDesc; QUEUE_NUM as usize] = [VringDesc {
    addr: 0,
    len: 0,
    flags: 0,
    next: 0,
}; QUEUE_NUM as usize];

static mut Q0_AVAIL: VringAvail = VringAvail {
    flags: 0,
    idx: 0,
    ring: [0; 256],
};

static mut Q0_USED: VringUsed = VringUsed {
    flags: 0,
    idx: 0,
    ring: [VringUsedElem { id: 0, len: 0 }; 256],
};

/// Token storage for the queue — maps descriptor-chain head to the
/// opaque data token given via `virtio_to_queue`.
static mut Q0_DATA: [usize; QUEUE_NUM as usize] = [0; QUEUE_NUM as usize];

// ── I/O port helpers ───────────────────────────────────────────────────────────

/// Write 8 bits to an I/O port.
#[inline]
unsafe fn out8(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val,
             options(nomem, nostack, preserves_flags));
    }
}

/// Write 16 bits to an I/O port.
#[inline]
unsafe fn out16(port: u16, val: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") val,
             options(nomem, nostack, preserves_flags));
    }
}

/// Write 32 bits to an I/O port.
#[inline]
unsafe fn out32(port: u16, val: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") val,
             options(nomem, nostack, preserves_flags));
    }
}

/// Read 8 bits from an I/O port.
#[inline]
unsafe fn in8(port: u16) -> u8 {
    let val: u8;
    unsafe {
        asm!("in al, dx", out("al") val, in("dx") port,
             options(nomem, nostack, preserves_flags));
    }
    val
}

/// Read 16 bits from an I/O port.
#[inline]
unsafe fn in16(port: u16) -> u16 {
    let val: u16;
    unsafe {
        asm!("in ax, dx", out("ax") val, in("dx") port,
             options(nomem, nostack, preserves_flags));
    }
    val
}

/// Read 32 bits from an I/O port.
#[inline]
unsafe fn in32(port: u16) -> u32 {
    let val: u32;
    unsafe {
        asm!("in eax, dx", out("eax") val, in("dx") port,
             options(nomem, nostack, preserves_flags));
    }
    val
}

// ── PCI config space access ────────────────────────────────────────────────────

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
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        asm!("out dx, eax", in("dx") PCI_ADDR_PORT, in("eax") addr,
             options(nomem, nostack, preserves_flags));
        let raw: u32;
        asm!("in eax, dx", out("eax") raw, in("dx") PCI_DATA_PORT,
             options(nomem, nostack, preserves_flags));
        ((raw >> ((reg as u32 & 0x03) * 8)) & 0xFF) as u8
    }
}

/// Read 16 bits from PCI config space.
///
/// # Safety
///
/// May conflict with other PCI config accesses. Must be serialized.
unsafe fn pci_cfg_read16(bus: u8, dev: u8, func: u8, reg: u8) -> u16 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        asm!("out dx, eax", in("dx") PCI_ADDR_PORT, in("eax") addr,
             options(nomem, nostack, preserves_flags));
        let raw: u32;
        asm!("in eax, dx", out("eax") raw, in("dx") PCI_DATA_PORT,
             options(nomem, nostack, preserves_flags));
        ((raw >> ((reg as u32 & 0x02) * 8)) & 0xFFFF) as u16
    }
}

/// Read 32 bits from PCI config space.
///
/// # Safety
///
/// May conflict with other PCI config accesses. Must be serialized.
unsafe fn pci_cfg_read32(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        asm!("out dx, eax", in("dx") PCI_ADDR_PORT, in("eax") addr,
             options(nomem, nostack, preserves_flags));
        let val: u32;
        asm!("in eax, dx", out("eax") val, in("dx") PCI_DATA_PORT,
             options(nomem, nostack, preserves_flags));
        val
    }
}

// ── Virtio register accessors ──────────────────────────────────────────────────

/// Read 32-bit from device register at `offset`.
pub fn virtio_read32(dev: &VirtioDevice, offset: u16) -> u32 {
    unsafe { in32(dev.port + offset) }
}

/// Read 16-bit from device register at `offset`.
pub fn virtio_read16(dev: &VirtioDevice, offset: u16) -> u16 {
    unsafe { in16(dev.port + offset) }
}

/// Read 8-bit from device register at `offset`.
pub fn virtio_read8(dev: &VirtioDevice, offset: u16) -> u8 {
    unsafe { in8(dev.port + offset) }
}

/// Write 32-bit to device register at `offset`.
pub fn virtio_write32(dev: &VirtioDevice, offset: u16, val: u32) {
    unsafe { out32(dev.port + offset, val) }
}

/// Write 16-bit to device register at `offset`.
pub fn virtio_write16(dev: &VirtioDevice, offset: u16, val: u16) {
    unsafe { out16(dev.port + offset, val) }
}

/// Write 8-bit to device register at `offset`.
pub fn virtio_write8(dev: &VirtioDevice, offset: u16, val: u8) {
    unsafe { out8(dev.port + offset, val) }
}

// ── Device-specific reads (with MSI offset compensation) ───────────────────────

/// Device-specific read 32-bit: adds `VIRTIO_DEV_SPECIFIC_OFF` and MSI offset.
pub fn virtio_sread32(dev: &VirtioDevice, offset: u16) -> u32 {
    let off = VIRTIO_DEV_SPECIFIC_OFF + if dev.msi { VIRTIO_MSI_ADD_OFF } else { 0 } + offset;
    unsafe { in32(dev.port + off) }
}

/// Device-specific read 16-bit: adds `VIRTIO_DEV_SPECIFIC_OFF` and MSI offset.
pub fn virtio_sread16(dev: &VirtioDevice, offset: u16) -> u16 {
    let off = VIRTIO_DEV_SPECIFIC_OFF + if dev.msi { VIRTIO_MSI_ADD_OFF } else { 0 } + offset;
    unsafe { in16(dev.port + off) }
}

/// Device-specific read 8-bit: adds `VIRTIO_DEV_SPECIFIC_OFF` and MSI offset.
pub fn virtio_sread8(dev: &VirtioDevice, offset: u16) -> u8 {
    let off = VIRTIO_DEV_SPECIFIC_OFF + if dev.msi { VIRTIO_MSI_ADD_OFF } else { 0 } + offset;
    unsafe { in8(dev.port + off) }
}

// ── Vring initialization ───────────────────────────────────────────────────────

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

// ── Feature helpers ────────────────────────────────────────────────────────────

/// Check if the host supports a specific feature bit.
pub fn virtio_host_supports(dev: &VirtioDevice, bit: u8) -> bool {
    (dev.host_features >> bit) & 1 != 0
}

/// Exchange features between host and device.
///
/// Reads host features from the device, records `host_support` for each
/// feature in the driver's feature list, and writes guest features back.
fn exchange_features(dev: &mut VirtioDevice) {
    let host_val = virtio_read32(dev, VIRTIO_HOST_F_OFF);
    let mut guest_features: u32 = 0;

    for f in dev.features.iter() {
        let bit = f.bit;
        guest_features |= (f.guest_support as u32) << bit;
    }

    // Store host features bitmap for later queries.
    dev.host_features = host_val;

    // Write negotiated guest features to the device.
    virtio_write32(dev, VIRTIO_GUEST_F_OFF, guest_features);
}

// ── Queue management ───────────────────────────────────────────────────────────

/// Allocate and initialize device queues.
///
/// For the single-queue device, this reads the queue size from the
/// device, validates it is a power of two, initialises the vring from
/// static storage, tells the host about the queue, and resets the queue
/// data token store.
pub fn virtio_alloc_queue(dev: &mut VirtioDevice) -> Result<(), VirtioError> {
    // Select queue 0.
    virtio_write16(dev, VIRTIO_QSEL_OFF, 0);

    let qsize = virtio_read16(dev, VIRTIO_QSIZE_OFF);
    if qsize == 0 || qsize & (qsize - 1) != 0 {
        return Err(VirtioError);
    }

    let num = qsize.min(QUEUE_NUM);
    let dev_port = dev.port;

    // SAFETY: This is the only place we initialise the static vring
    // storage. We hold `&mut VirtioDevice` guaranteeing exclusive
    // access. Static storage is accessed via raw pointers.
    unsafe {
        let descs: &'static mut [VringDesc] = &mut *addr_of_mut!(Q0_DESCS);
        let avail: &'static mut VringAvail = &mut *addr_of_mut!(Q0_AVAIL);
        let used: &'static mut VringUsed = &mut *addr_of_mut!(Q0_USED);

        let q = &mut dev.queues[0];
        vring_init(&mut q.vring, num, descs, avail, used);

        q.free_num = num;
        q.free_head = 0;
        q.free_tail = num - 1;
        q.last_used = 0;

        // Write guest-physical page number (paddr >> 12) to host.
        // Use raw port access to avoid borrowing dev while q borrows it.
        out32(dev_port + VIRTIO_QADDR_OFF, (q.paddr >> 12) as u32);

        // Clear token store.
        let data: &mut [usize; QUEUE_NUM as usize] = &mut *addr_of_mut!(Q0_DATA);
        for slot in data.iter_mut() {
            *slot = 0;
        }
    }

    dev.num_queues = 1;
    Ok(())
}

// ── Kick helpers ───────────────────────────────────────────────────────────────

/// Write 16-bit directly to a device port (no borrow).
#[inline]
fn virtio_write16_raw(port: u16, offset: u16, val: u16) {
    unsafe { out16(port + offset, val) }
}

// ── Descriptor chain helpers ───────────────────────────────────────────────────

/// Fill a single vring descriptor from a `VirtioPhysBuf`.
///
/// The LSB of `vp.addr` is used as the writable flag; the actual
/// address is `vp.addr & !1`.
fn use_vring_desc(vd: &mut VringDesc, vp: &VirtioPhysBuf) {
    vd.addr = vp.addr & !1u64;
    vd.len = vp.size;
    vd.flags = VRING_DESC_F_NEXT;
    if vp.addr & 1 != 0 {
        vd.flags |= VRING_DESC_F_WRITE;
    }
}

/// Chain `num_bufs` descriptors starting at `free_head`.
/// Descriptors must already be filled by `fill_descriptors`.
///
/// Returns the new free_head after consuming descriptors.
fn chain_descriptors(vring: &mut Vring, free_head: u16, num_bufs: usize) -> u16 {
    let mut i = free_head;

    for _ in 0..num_bufs {
        let vd = &mut vring.desc[i as usize];
        i = vd.next;
    }

    // Unset NEXT flag on the last descriptor in the chain.
    let last = free_head as usize + num_bufs - 1;
    vring.desc[last].flags &= !VRING_DESC_F_NEXT;

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

// ── Submit / reap ──────────────────────────────────────────────────────────────

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

    let dev_port = dev.port;

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
            (*addr_of_mut!(Q0_DATA))[head as usize] = data;
        }

        // Memory barrier: host must see descriptor writes before
        // the avail index update.
        unsafe {
            asm!("mfence", options(nostack, preserves_flags));
        }

        // Advance the avail index.
        vring.avail.idx = vring.avail.idx.wrapping_add(1);

        // Memory barrier: host must see updated avail index before kick.
        unsafe {
            asm!("mfence", options(nostack, preserves_flags));
        }

        // Check if the host wants notification.
        vring.used.flags & VRING_USED_F_NO_NOTIFY == 0
    };

    // Kick outside the queue borrow to avoid aliasing with `dev`.
    if need_kick {
        virtio_write16_raw(dev_port, VIRTIO_QNOTIFY_OFF, 0);
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
        asm!("mfence", options(nostack, preserves_flags));
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
        let tok = unsafe { (*addr_of_mut!(Q0_DATA))[idx as usize] };
        unsafe {
            (*addr_of_mut!(Q0_DATA))[idx as usize] = 0;
        }

        Some(tok)
    }
}

// ── IRQ helpers ────────────────────────────────────────────────────────────────

/// Check if the device has asserted an interrupt.
///
/// Reads the ISR status register. Returns `true` if the interrupt was
/// for this device.
pub fn virtio_had_irq(dev: &VirtioDevice) -> bool {
    virtio_read8(dev, VIRTIO_ISR_STATUS_OFF) & 1 != 0
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

// ── Device lifecycle ───────────────────────────────────────────────────────────

/// Probe for a virtio device with the given subsystem device ID.
///
/// Scans PCI bus 0 for vendor `0x1AF4` devices matching `subdevid`.
/// Returns an initialized `VirtioDevice` on success.
///
/// The `skip` parameter allows selecting the Nth matching device.
pub fn virtio_probe(
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

            let port = (bar0 & !0x3) as u16;

            // Read IRQ line (PCI offset 0x3F).
            let irq = unsafe { pci_cfg_read8(0, dev, func, 0x3F) };

            // Build a temporary device for register access.
            let mut device = VirtioDevice {
                port,
                name,
                features,
                host_features: 0,
                queues: [VirtioQueue {
                    vring: Vring {
                        num: 0,
                        desc: &mut [],
                        avail: unsafe { &mut *addr_of_mut!(Q0_AVAIL) },
                        used: unsafe { &mut *addr_of_mut!(Q0_USED) },
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

/// Set the device ready.
///
/// Sets the `DRV_OK` status bit, signalling to the host that the driver
/// is fully operational.
pub fn virtio_device_ready(dev: &mut VirtioDevice) {
    virtio_write8(dev, VIRTIO_DEV_STATUS_OFF, VIRTIO_STATUS_DRV_OK);
}

/// Reset the device.
///
/// Clears the device status (writing 0), which triggers a device reset.
pub fn virtio_reset_device(dev: &mut VirtioDevice) {
    virtio_write8(dev, VIRTIO_DEV_STATUS_OFF, 0);
    dev.initialized = false;
}

// ── Tests ──────────────────────────────────────────────────────────────────────

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
        let dummy = unsafe { &mut *addr_of_mut!(Q0_AVAIL) };
        let mut vr = Vring {
            num: 0,
            desc: &mut [], // vring_init overwrites this
            avail: dummy,  // vring_init overwrites this
            used: unsafe { &mut *addr_of_mut!(Q0_USED) },
        };
        vring_init(&mut vr, num, descs, avail, used);
        vr
    }

    // ── Constants ──────────────────────────────────────────────────────────

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

    // ── Type layout ────────────────────────────────────────────────────────

    #[test]
    fn test_type_sizes() {
        assert_eq!(size_of::<VringDesc>(), 16);
        assert_eq!(size_of::<VringAvail>(), 516);
        assert_eq!(size_of::<VringUsedElem>(), 8);
        assert_eq!(size_of::<VringUsed>(), 4 + 256 * 8);
        assert_eq!(size_of::<VirtioPhysBuf>(), 16);
    }

    // ── Vring initialisation ───────────────────────────────────────────────

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

    // ── Descriptor chain management ────────────────────────────────────────

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
            avail: unsafe { &mut *addr_of_mut!(Q0_AVAIL) },
            used: unsafe { &mut *addr_of_mut!(Q0_USED) },
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

    // ── Feature helpers ────────────────────────────────────────────────────

    #[test]
    fn test_virtio_host_supports_with_bitmap() {
        let dev = VirtioDevice {
            port: 0,
            name: "test",
            features: &[],
            host_features: 1u32 << 28,
            queues: [VirtioQueue {
                vring: Vring {
                    num: 0,
                    desc: &mut [],
                    avail: unsafe { &mut *addr_of_mut!(Q0_AVAIL) },
                    used: unsafe { &mut *addr_of_mut!(Q0_USED) },
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

    // ── VirtioError ────────────────────────────────────────────────────────

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

    // ── PCI config address builder ─────────────────────────────────────────

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

    // ── Default initialisers ───────────────────────────────────────────────

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
