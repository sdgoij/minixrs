//! Architecture I/O HAL for the drivers crate.
//!
//! This is THE ONLY file in the drivers crate that uses `#[cfg(target_arch)]`.
//! It re-exports the correct arch-specific I/O functions.
//! All driver code calls `hal::*()` unconditionally.

#[cfg(target_arch = "x86_64")]
pub use arch_x86_64::hal::{
    PCI_ADDR_PORT, PCI_DATA_PORT, RTC_INDEX, cmos_read, cmos_write, inb, inl, inw, mfence, outb,
    outl, outw, pci_cfg_read8, pci_cfg_read16, pci_cfg_read32, pci_cfg_write32, pci_config_addr,
};

#[cfg(target_arch = "riscv64")]
pub use arch_riscv64::hal::{
    PCI_ADDR_PORT, PCI_DATA_PORT, RTC_INDEX, cmos_read, cmos_write, inb, inl, inw, mfence, outb,
    outl, outw, pci_cfg_read8, pci_cfg_read16, pci_cfg_read32, pci_cfg_write32, pci_config_addr,
};

#[cfg(target_arch = "aarch64")]
pub use arch_aarch64::hal::{
    PCI_ADDR_PORT, PCI_DATA_PORT, RTC_INDEX, cmos_read, cmos_write, inb, inl, inw, mfence, outb,
    outl, outw, pci_cfg_read8, pci_cfg_read16, pci_cfg_read32, pci_cfg_write32, pci_config_addr,
};

// Devices in the wasm port come from the host, so every port-I/O and PCI entry
// point here is inert — the disposition `ARCH_WASM32.md` §8 already prescribes.
// The names exist so driver code compiles unchanged; a driver that actually
// needs its device reaches it through a host import instead, which is what M4
// (block), M5 (display, input) and M6 (network) each added below.
#[cfg(target_arch = "wasm32")]
pub use arch_wasm32::hal::{
    PCI_ADDR_PORT, PCI_DATA_PORT, RTC_INDEX, cmos_read, cmos_write, inb, inl, inw, mfence, outb,
    outl, outw, pci_cfg_read8, pci_cfg_read16, pci_cfg_read32, pci_cfg_write32, pci_config_addr,
};

// The block device, and the first entry here that is a real device rather than an
// inert name: on this arch the driver's hardware *is* the host (M4). `virtio_blk`
// asks these three instead of scanning a bus, which is why the driver can keep one
// request path and two transports.
//
// A hardware arch has no host to ask, and a block-device driver there reaches its
// device through the virtio transport — so these answer "no device" and the call
// site is unreachable by construction. They exist so driver code compiles, the same
// disposition the port-I/O names above have.
#[cfg(target_arch = "wasm32")]
pub use arch_wasm32::hal::{block_capacity, block_read, block_write};

#[cfg(not(target_arch = "wasm32"))]
mod no_host_device {
    /// There is no host, so there is no device to ask about.
    pub fn block_capacity() -> u64 {
        0
    }

    /// `ENODEV`. Reached only if a driver calls the host transport on an arch that has none, which
    /// `virtio_blk`'s `cfg` prevents — the answer is a refusal rather than a success with no bytes,
    /// because a silent zero-length read is a hole in a filesystem.
    pub fn block_read(_offset: u64, _buf: &mut [u8]) -> i32 {
        -19
    }

    /// `ENODEV`, as above.
    pub fn block_write(_offset: u64, _buf: &[u8]) -> i32 {
        -19
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use no_host_device::{block_capacity, block_read, block_write};

// The display, and the second device here whose hardware is the host (M5). Same disposition as
// the block device above: on wasm the driver asks the host for its mode and flushes to it; on a
// hardware arch there is no host, so the answers are "no display" and the call sites are
// unreachable by construction — a driver there reaches its framebuffer through the VGA registers
// or the virtio-gpu transport instead.
#[cfg(target_arch = "wasm32")]
pub use arch_wasm32::hal::{fb_geometry, fb_present};

#[cfg(not(target_arch = "wasm32"))]
mod no_host_display {
    /// No host, so no display to ask about: `fb`'s backends there are bochs and virtio-gpu, and
    /// the canvas one is not compiled in.
    pub fn fb_geometry() -> (u32, u32) {
        (0, 0)
    }

    /// `ENODEV`, as above: a flush that reached here has no display to reach, and answering zero
    /// would report a frame that nobody saw.
    pub fn fb_present(_buf: &[u8]) -> i32 {
        -19
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use no_host_display::{fb_geometry, fb_present};

// Input, and the third device here whose hardware is the host (M5c). The disposition is the
// display's, with one difference: this is the one host device the guest *pulls* rather than the one
// that pushes. The input server drains the host's queue on the notification its IRQ hook produced,
// so the queue lives on the host's side of the boundary and the drain on the guest's — which is why
// there is a reader here and not, say, a `host_input_pending` the kernel polls.
//
// On a hardware arch the answer is always `None`: the keyboard and the pointer there are an 8042, a
// virtio-input or a USB HID device, and the input server's other two backends are where those
// events come from.
#[cfg(target_arch = "wasm32")]
pub use arch_wasm32::hal::input_event;

#[cfg(not(target_arch = "wasm32"))]
mod no_host_input {
    /// No host, so no queue: the events come from the device backends instead.
    pub fn input_event() -> Option<(u16, u16, i32)> {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use no_host_input::input_event;

// Network, and the fourth device here whose hardware is the host (M6). Same disposition as the
// block device: on wasm the driver's wire *is* the host, and on a hardware arch there is no host to
// ask — `virtio_net`'s transport there is the virtio queue, so the call sites are unreachable by
// construction and the answers below are refusals rather than successes with no packets.
//
// The shape is the block device's, with the read/write direction made explicit: a received frame is
// *pulled* (the DL client's read drives it, and an empty queue is the ordinary answer), while a
// transmit is handed over whole. `net_pending` is the used-ring walk's replacement — a driver has to
// ask before it reads, because "nothing yet" is what a NIC answers most of the time.
#[cfg(target_arch = "wasm32")]
pub use arch_wasm32::hal::{net_mac, net_pending, net_recv, net_send};

#[cfg(not(target_arch = "wasm32"))]
mod no_host_network {
    /// No host, so no link to ask for an address: `virtio_net`'s probe there reads the device
    /// config instead.
    pub fn net_mac() -> Option<[u8; 6]> {
        None
    }

    /// No link, so nothing queued — and `None` rather than `Some(0)`, because the two are different
    /// questions and only one of them is "there is no wire".
    pub fn net_pending() -> Option<usize> {
        None
    }

    /// No link, so no frame. `None` rather than a short read, because a zero-length frame is not a
    /// frame — the same reason the driver's own path reports an empty queue as "nothing pending".
    pub fn net_recv(_buf: &mut [u8]) -> Option<usize> {
        None
    }

    /// `false`, as above: a transmit that reached here has no wire to put a frame on, and answering
    /// success would report a packet nobody sent.
    pub fn net_send(_frame: &[u8]) -> bool {
        false
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use no_host_network::{net_mac, net_pending, net_recv, net_send};
