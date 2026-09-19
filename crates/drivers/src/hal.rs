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
