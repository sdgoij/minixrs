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
// needs its device will reach it through a host import in M4/M6, not through
// these.
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
