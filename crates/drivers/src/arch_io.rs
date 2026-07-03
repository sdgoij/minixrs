//! Architecture-specific I/O operations.
//!
//! On x86_64, provides real port I/O (`in`/`out` instructions) and
//! PCI legacy config access (CF8/CFC ports). On non-x86_64 targets,
//! provides stubs that return zero / do nothing.

/// Read a byte from an I/O port.
///
/// # Safety
/// Caller must ensure the I/O port is mapped and accessible, and that the
/// operation does not violate memory safety or system stability.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port,
            options(nomem, nostack, preserves_flags));
    }
    val
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: always returns 0 on non-x86_64 targets. Port I/O is not available.
#[inline]
pub unsafe fn inb(_port: u16) -> u8 {
    0
}

/// Read a 16-bit word from an I/O port.
///
/// # Safety
/// Caller must ensure the I/O port is mapped and accessible, and that the
/// operation does not violate memory safety or system stability.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let val: u16;
    unsafe {
        core::arch::asm!("in ax, dx", out("ax") val, in("dx") port,
            options(nomem, nostack, preserves_flags));
    }
    val
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: always returns 0 on non-x86_64 targets. Port I/O is not available.
#[inline]
pub unsafe fn inw(_port: u16) -> u16 {
    0
}

/// Read a 32-bit dword from an I/O port.
///
/// # Safety
/// Caller must ensure the I/O port is mapped and accessible, and that the
/// operation does not violate memory safety or system stability.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        core::arch::asm!("in eax, dx", out("eax") val, in("dx") port,
            options(nomem, nostack, preserves_flags));
    }
    val
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: no-op on non-x86_64 targets. Caller must ensure port I/O is
/// not required before calling this function.
#[inline]
pub unsafe fn inl(_port: u16) -> u32 {
    0
}

/// Write a byte to an I/O port.
///
/// # Safety
/// Caller must ensure the I/O port is mapped and accessible, and that the
/// operation does not violate memory safety or system stability.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val,
            options(nomem, nostack, preserves_flags));
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: no-op on non-x86_64 targets. Caller must ensure port I/O is
/// not required before calling this function.
#[inline]
pub unsafe fn outb(_port: u16, _val: u8) {}

/// Write a 16-bit word to an I/O port.
///
/// # Safety
/// Caller must ensure the I/O port is mapped and accessible, and that the
/// operation does not violate memory safety or system stability.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn outw(port: u16, val: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") val,
            options(nomem, nostack, preserves_flags));
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: no-op on non-x86_64 targets. Caller must ensure port I/O is
/// not required before calling this function.
#[inline]
pub unsafe fn outw(_port: u16, _val: u16) {}

/// Write a 32-bit dword to an I/O port.
///
/// # Safety
/// Caller must ensure the I/O port is mapped and accessible, and that the
/// operation does not violate memory safety or system stability.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn outl(port: u16, val: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") val,
            options(nomem, nostack, preserves_flags));
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: no-op on non-x86_64 targets. Caller must ensure port I/O is
/// not required before calling this function.
#[inline]
pub unsafe fn outl(_port: u16, _val: u32) {}

/// Memory fence.
///
/// # Safety
/// Caller must ensure the fence is used correctly to enforce memory ordering
/// constraints between threads and device memory.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn mfence() {
    unsafe {
        core::arch::asm!("mfence", options(nostack, preserves_flags));
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: issues a compiler fence on non-x86_64 targets. Caller must ensure
/// the fence is sufficient for the required memory ordering.
#[inline]
pub unsafe fn mfence() {
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}

/// PCI configuration address port.
pub const PCI_ADDR_PORT: u16 = 0xCF8;
/// PCI configuration data port.
pub const PCI_DATA_PORT: u16 = 0xCFC;

/// Build a PCI config address.
#[inline]
pub fn pci_config_addr(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | (reg as u32 & 0xFC)
}

/// Read 8 bits from PCI config space.
///
/// # Safety
/// Caller must ensure the PCI bus/device/function is valid and accessible, and
/// that the PCI config read does not cause undefined behavior.
#[cfg(target_arch = "x86_64")]
pub unsafe fn pci_cfg_read8(bus: u8, dev: u8, func: u8, reg: u8) -> u8 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        outl(PCI_ADDR_PORT, addr);
        let raw = inl(PCI_DATA_PORT);
        ((raw >> ((reg as u32 & 0x03) * 8)) & 0xFF) as u8
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: always returns 0xFF on non-x86_64 targets. PCI config space is
/// not accessible via port I/O on this architecture.
pub unsafe fn pci_cfg_read8(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u8 {
    0xFF
}

/// Read 16 bits from PCI config space.
///
/// # Safety
/// Caller must ensure the PCI bus/device/function is valid and accessible, and
/// that the PCI config read does not cause undefined behavior.
#[cfg(target_arch = "x86_64")]
pub unsafe fn pci_cfg_read16(bus: u8, dev: u8, func: u8, reg: u8) -> u16 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        outl(PCI_ADDR_PORT, addr);
        let raw = inl(PCI_DATA_PORT);
        ((raw >> ((reg as u32 & 0x02) * 8)) & 0xFFFF) as u16
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: always returns 0xFFFF on non-x86_64 targets. PCI config space is
/// not accessible via port I/O on this architecture.
pub unsafe fn pci_cfg_read16(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u16 {
    0xFFFF
}

/// Read 32 bits from PCI config space.
///
/// # Safety
/// Caller must ensure the PCI bus/device/function is valid and accessible, and
/// that the PCI config read does not cause undefined behavior.
#[cfg(target_arch = "x86_64")]
pub unsafe fn pci_cfg_read32(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        outl(PCI_ADDR_PORT, addr);
        inl(PCI_DATA_PORT)
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: always returns 0xFFFF_FFFF on non-x86_64 targets. PCI config
/// space is not accessible via port I/O on this architecture.
pub unsafe fn pci_cfg_read32(_bus: u8, _dev: u8, _func: u8, _reg: u8) -> u32 {
    0xFFFF_FFFF
}

/// Write 32 bits to PCI config space.
///
/// # Safety
/// Caller must ensure the PCI bus/device/function is valid and accessible, and
/// that the PCI config write does not cause undefined behavior.
#[cfg(target_arch = "x86_64")]
pub unsafe fn pci_cfg_write32(bus: u8, dev: u8, func: u8, reg: u8, val: u32) {
    let addr = pci_config_addr(bus, dev, func, reg);
    unsafe {
        outl(PCI_ADDR_PORT, addr);
        outl(PCI_DATA_PORT, val);
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: no-op on non-x86_64 targets. PCI config space is not accessible
/// via port I/O on this architecture.
pub unsafe fn pci_cfg_write32(_bus: u8, _dev: u8, _func: u8, _reg: u8, _val: u32) {}

/// RTC CMOS index port.
pub const RTC_INDEX: u16 = 0x70;

/// Read a CMOS register value.
///
/// # Safety
/// Caller must ensure the CMOS/RTC register is accessed correctly and that the
/// operation does not interfere with other system components or cause undefined
/// behavior.
#[cfg(target_arch = "x86_64")]
pub unsafe fn cmos_read(reg: u8) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm! {
            "out dx, al",
            "mov dl, 0x71",
            "in al, dx",
            inout("dx") RTC_INDEX => _,
            inout("al") reg => val,
            options(nomem, nostack),
        };
    }
    val
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: always returns 0 on non-x86_64 targets. CMOS/RTC is not
/// accessible via port I/O on this architecture.
pub unsafe fn cmos_read(_reg: u8) -> u8 {
    0
}

/// Write a value to a CMOS register.
///
/// # Safety
/// Caller must ensure the CMOS/RTC register is accessed correctly and that the
/// operation does not interfere with other system components or cause undefined
/// behavior.
#[cfg(target_arch = "x86_64")]
pub unsafe fn cmos_write(reg: u8, val: u8) {
    unsafe {
        core::arch::asm! {
            "mov dx, 0x70",
            "mov al, al",
            "out dx, al",
            "mov dx, 0x71",
            "mov al, cl",
            "out dx, al",
            in("eax") reg as u32,
            in("ecx") val as u32,
            options(nomem, nostack, preserves_flags),
        };
    }
}

#[cfg(not(target_arch = "x86_64"))]
/// # Safety
///
/// Stub: no-op on non-x86_64 targets. CMOS/RTC is not accessible via
/// port I/O on this architecture.
pub unsafe fn cmos_write(_reg: u8, _val: u8) {}
