//! PCI configuration space access via x86 I/O ports (0xCF8/0xCFC).
//!
//! Ported from `.refs/minix-3.3.0/minix/include/arch/i386/include/pci.h`
//! and the standard x86 PCI configuration mechanism.
//!
//! # PCI Configuration Mechanism
//!
//! Write (bus, dev, func, reg) to CONFIG_ADDRESS (0xCF8), then read/write
//! from CONFIG_DATA (0xCFC). The format of CONFIG_ADDRESS is:
//!
//!   bit 31    = Enable bit (must be 1)
//!   bits 30:24= Reserved (0)
//!   bits 23:16= Bus number
//!   bits 15:11= Device number
//!   bits 10:8 = Function number
//!   bits 7:0  = Register offset (must be aligned for 8/16/32-bit access)

use core::arch::asm;

// ── Constants ───────────────────────────────────────────────────────────────

/// PCI configuration address port.
pub const PCI_CONFIG_ADDRESS: u16 = 0xCF8;

/// PCI configuration data port.
pub const PCI_CONFIG_DATA: u16 = 0xCFC;

/// Maximum buses (PCI spec allows 256).
pub const PCI_MAX_BUSES: usize = 256;

/// Maximum devices per bus (32).
pub const PCI_MAX_DEVICES: u8 = 32;

/// Maximum functions per device (8).
pub const PCI_MAX_FUNCTIONS: u8 = 8;

/// Maximum BARs per device (6).
pub const PCI_BAR_MAX: usize = 6;

/// Standard PCI vendor/device ID register.
pub const PCI_VENDOR_ID: u8 = 0x00;

/// Standard PCI command register.
pub const PCI_COMMAND: u8 = 0x04;

/// Standard PCI status register.
pub const PCI_STATUS: u8 = 0x06;

/// Standard PCI revision ID register.
pub const PCI_REVISION: u8 = 0x08;

/// Standard PCI class register (3 bytes: base, sub, interface).
pub const PCI_CLASS: u8 = 0x0A;

/// Standard PCI header type register.
pub const PCI_HEADER_TYPE: u8 = 0x0E;

/// Standard PCI BAR0 register.
pub const PCI_BASE_ADDRESS_0: u8 = 0x10;

/// Standard PCI interrupt line register.
pub const PCI_INTERRUPT_LINE: u8 = 0x3C;

/// Standard PCI secondary bus register (for PCI-to-PCI bridges).
pub const PCI_SECONDARY_BUS: u8 = 0x09;

/// Device 0xFFFF = invalid (no device).
pub const PCI_INVALID_DEV: u16 = 0xFFFF;

/// Class code for PCI-to-PCI bridge.
pub const PCI_CLASS_BRIDGE: u8 = 0x06;

/// Subclass for PCI-to-PCI bridge.
pub const PCI_SUBCLASS_BRIDGE: u8 = 0x04;

/// Header type: multi-function device.
pub const PCI_HEADER_MULTI: u8 = 0x80;

/// Header type: PCI-to-PCI bridge.
pub const PCI_HEADER_BRIDGE: u8 = 0x01;

/// Command register: I/O space enable.
pub const PCI_CMD_IO: u16 = 0x0001;

/// Command register: memory space enable.
pub const PCI_CMD_MEM: u16 = 0x0002;

/// Command register: bus master enable.
pub const PCI_CMD_MASTER: u16 = 0x0004;

// ── Address encoding ────────────────────────────────────────────────────────

/// Build a PCI config address.
///
/// `reg` must be 4-byte aligned for 32-bit access, but the resulting
/// address can be used for 8/16/32-bit reads by adjusting the data port.
pub const fn pci_make_addr(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    0x8000_0000 // enable bit
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | (reg as u32)
}

// ── Low-level access ────────────────────────────────────────────────────────

/// Read 8 bits from PCI config space.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config ports.
pub unsafe fn pci_read8(bus: u8, dev: u8, func: u8, reg: u8) -> u8 {
    let addr = pci_make_addr(bus, dev, func, reg & 0xFC);
    unsafe {
        write_addr(addr);
        let raw: u32;
        asm!(
            "in eax, dx",
            out("eax") raw,
            in("dx") PCI_CONFIG_DATA,
            options(nomem, nostack, preserves_flags),
        );
        ((raw >> ((reg & 0x03) * 8)) & 0xFF) as u8
    }
}

/// Read 16 bits from PCI config space.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config ports.
pub unsafe fn pci_read16(bus: u8, dev: u8, func: u8, reg: u8) -> u16 {
    let addr = pci_make_addr(bus, dev, func, reg & 0xFC);
    unsafe {
        write_addr(addr);
        let val: u16;
        asm!(
            "in ax, dx",
            out("ax") val,
            in("dx") PCI_CONFIG_DATA,
            options(nomem, nostack, preserves_flags),
        );
        val
    }
}

/// Read 32 bits from PCI config space.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config ports.
pub unsafe fn pci_read32(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    let addr = pci_make_addr(bus, dev, func, reg & 0xFC);
    unsafe {
        write_addr(addr);
        let val: u32;
        asm!(
            "in eax, dx",
            out("eax") val,
            in("dx") PCI_CONFIG_DATA,
            options(nomem, nostack, preserves_flags),
        );
        val
    }
}

/// Write 8 bits to PCI config space.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config ports.
pub unsafe fn pci_write8(bus: u8, dev: u8, func: u8, reg: u8, val: u8) {
    let addr = pci_make_addr(bus, dev, func, reg & 0xFC);
    unsafe {
        write_addr(addr);
        let shift = (reg & 0x03) * 8;
        let old = pci_read32(bus, dev, func, reg & 0xFC);
        let new = (old & !(0xFFu32 << shift)) | ((val as u32) << shift);
        pci_write32_raw(new);
    }
}

/// Write 32 bits to PCI config space.
///
/// # Safety
///
/// Must be called with exclusive access to PCI config ports.
pub unsafe fn pci_write32(bus: u8, dev: u8, func: u8, reg: u8, val: u32) {
    let addr = pci_make_addr(bus, dev, func, reg & 0xFC);
    unsafe {
        write_addr(addr);
        pci_write32_raw(val);
    }
}

/// Write the address to CONFIG_ADDRESS.
unsafe fn write_addr(addr: u32) {
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") PCI_CONFIG_ADDRESS,
            in("eax") addr,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Write a 32-bit value to CONFIG_DATA (address already set).
unsafe fn pci_write32_raw(val: u32) {
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") PCI_CONFIG_DATA,
            in("eax") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Check if a PCI device exists (vendor ID != 0xFFFF).
pub fn pci_device_exists(vendor: u16) -> bool {
    vendor != PCI_INVALID_DEV && vendor != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pci_make_addr() {
        let addr = pci_make_addr(0, 0, 0, 0);
        assert_eq!(addr, 0x8000_0000, "bus 0 dev 0 func 0 reg 0");

        let addr = pci_make_addr(1, 2, 3, 0x10);
        assert_eq!(addr, 0x8000_0000 | (1 << 16) | (2 << 11) | (3 << 8) | 0x10);
    }

    #[test]
    fn test_pci_make_addr_alignment() {
        // pci_make_addr takes reg as-is; alignment is the caller's
        // responsibility (pci_read* functions mask reg & 0xFC).
        let addr = pci_make_addr(0, 0, 0, 0x05);
        assert_eq!(addr & 0xFF, 0x05, "reg value is passed through as-is");
        // The caller masks to 0xFC for 32-bit alignment
        let addr_aligned = pci_make_addr(0, 0, 0, 0x05 & 0xFC);
        assert_eq!(
            addr_aligned & 0xFF,
            0x04,
            "mask to 0xFC gives aligned address"
        );
    }

    #[test]
    fn test_pci_constants() {
        assert_eq!(PCI_CONFIG_ADDRESS, 0xCF8);
        assert_eq!(PCI_CONFIG_DATA, 0xCFC);
        assert_eq!(PCI_MAX_DEVICES, 32);
        assert_eq!(PCI_MAX_FUNCTIONS, 8);
        assert_eq!(PCI_BAR_MAX, 6);
    }

    #[test]
    fn test_pci_device_exists() {
        assert!(!pci_device_exists(0xFFFF));
        assert!(!pci_device_exists(0));
        assert!(pci_device_exists(0x8086));
    }

    #[test]
    fn test_invalid_vendor() {
        assert_eq!(PCI_INVALID_DEV, 0xFFFF);
    }

    #[test]
    fn test_register_offsets() {
        assert_eq!(PCI_VENDOR_ID, 0x00);
        assert_eq!(PCI_COMMAND, 0x04);
        assert_eq!(PCI_STATUS, 0x06);
        assert_eq!(PCI_CLASS, 0x0A);
        assert_eq!(PCI_HEADER_TYPE, 0x0E);
        assert_eq!(PCI_BASE_ADDRESS_0, 0x10);
        assert_eq!(PCI_INTERRUPT_LINE, 0x3C);
    }

    #[test]
    fn test_command_bits() {
        assert_eq!(PCI_CMD_IO, 0x0001);
        assert_eq!(PCI_CMD_MEM, 0x0002);
        assert_eq!(PCI_CMD_MASTER, 0x0004);
    }

    #[test]
    fn test_class_bridge() {
        assert_eq!(PCI_CLASS_BRIDGE, 0x06);
        assert_eq!(PCI_SUBCLASS_BRIDGE, 0x04);
    }

    #[test]
    fn test_functions_compile() {
        fn _is_fn<T>(_: T) {}
        _is_fn(pci_read8 as unsafe fn(u8, u8, u8, u8) -> u8);
        _is_fn(pci_read16 as unsafe fn(u8, u8, u8, u8) -> u16);
        _is_fn(pci_read32 as unsafe fn(u8, u8, u8, u8) -> u32);
        _is_fn(pci_write8 as unsafe fn(u8, u8, u8, u8, u8));
        _is_fn(pci_write32 as unsafe fn(u8, u8, u8, u8, u32));
    }
}
