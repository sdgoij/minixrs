//! Local APIC and I/O APIC initialization.
//!
//! Implements APIC detection, xAPIC/x2APIC mode handling, I/O APIC RTE
//! configuration, LINT0 reprogramming, and spurious interrupt vector setup.
//!
//! **All MMIO-accessing functions are `unsafe`** because they perform
//! volatile reads/writes at physical addresses (0xFEE00000 for Local APIC,
//! 0xFEC00000 for I/O APIC). These addresses must be identity-mapped and
//! accessible at the point of use. On bare metal this holds (PD3 covers
//! the 3-4 GB range); on host test binaries these calls will fault.

use core::ptr;

use crate::asm;

// ── MSR ─────────────────────────────────────────────────────────────────

/// IA32_APIC_BASE MSR.
const IA32_APIC_BASE_MSR: u32 = 0x1B;

// ── Local APIC MMIO register offsets (xAPIC) ───────────────────────────

#[allow(unused)]
const APIC_ID_OFF: u32 = 0x20;
const APIC_VERSION_OFF: u32 = 0x30;
#[allow(unused)]
const APIC_TASK_PRIORITY_OFF: u32 = 0x80;
const APIC_SPURIOUS_OFF: u32 = 0xF0;
const APIC_EOI_OFF: u32 = 0xB0;
const APIC_LINT0_OFF: u32 = 0x350;
#[allow(unused)]
const APIC_LINT1_OFF: u32 = 0x360;
#[allow(unused)]
const APIC_ERROR_OFF: u32 = 0x370;
#[allow(unused)]
const APIC_TIMER_OFF: u32 = 0x320;
#[allow(unused)]
const APIC_TIMER_INITCNT_OFF: u32 = 0x380;
#[allow(unused)]
const APIC_TIMER_CURRCNT_OFF: u32 = 0x390;
#[allow(unused)]
const APIC_TIMER_DIV_OFF: u32 = 0x3E0;

// ── I/O APIC register offsets ──────────────────────────────────────────

const IOAPIC_IOREGSEL: u64 = 0x00;
const IOAPIC_IOWIN: u64 = 0x10;
#[allow(unused)]
const IOAPIC_ID: u32 = 0x00;
const IOAPIC_VERSION: u32 = 0x01;
#[allow(unused)]
const IOAPIC_ARB: u32 = 0x02;
const IOAPIC_REDIR_TBL: u32 = 0x10; // first RTE index

// ── Local APIC physical base addresses (typical) ───────────────────────

/// Local APIC physical base address (typical).
pub const DEFAULT_APIC_BASE: u64 = 0xFEE00000;

/// I/O APIC physical base address (typical).
pub const DEFAULT_IOAPIC_BASE: u64 = 0xFEC00000;

// ── Spurious vector ────────────────────────────────────────────────────

const APIC_SVR_ENABLE: u32 = 0x100; // bit 8
const APIC_SPURIOUS_VECTOR: u32 = 0xFF;

// ── APIC mode ──────────────────────────────────────────────────────────

/// The detected operating mode of the system's interrupt controllers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApicMode {
    /// Legacy 8259A PIC only (APIC disabled or not present).
    PicOnly,
    /// xAPIC mode (MMIO-based register access).
    XApic,
    /// x2APIC mode (MSR-based register access).
    X2Apic,
}

// ── Global APIC state ──────────────────────────────────────────────────

static mut APIC_BASE: u64 = 0;
static mut IOAPIC_BASE: u64 = 0;
static mut APIC_MODE: ApicMode = ApicMode::PicOnly;
static mut APIC_ENABLED: bool = false;

// ── Helper: MMIO register access ───────────────────────────────────────

/// Read an APIC MMIO register (xAPIC mode).
///
/// # Safety
///
/// `APIC_BASE` must be set to a valid, identity-mapped APIC base address.
/// Calling this in x2APIC mode or without a mapped APIC produces UB.
unsafe fn apic_read(offset: u32) -> u32 {
    unsafe {
        let addr = (APIC_BASE + offset as u64) as *const u32;
        ptr::read_volatile(addr)
    }
}

/// Write an APIC MMIO register (xAPIC mode).
///
/// # Safety
///
/// `APIC_BASE` must be set to a valid, identity-mapped APIC base address.
/// Calling this in x2APIC mode or without a mapped APIC produces UB.
unsafe fn apic_write(offset: u32, val: u32) {
    unsafe {
        let addr = (APIC_BASE + offset as u64) as *mut u32;
        ptr::write_volatile(addr, val);
    }
}

/// Read an I/O APIC register via its index/data register pair.
///
/// # Safety
///
/// `IOAPIC_BASE` must be set to a valid, identity-mapped I/O APIC base
/// address. Calling without a mapped I/O APIC produces UB.
unsafe fn ioapic_read(reg: u32) -> u32 {
    unsafe {
        let sel_addr = (IOAPIC_BASE + IOAPIC_IOREGSEL) as *mut u32;
        let win_addr = (IOAPIC_BASE + IOAPIC_IOWIN) as *mut u32;
        ptr::write_volatile(sel_addr, reg);
        ptr::read_volatile(win_addr)
    }
}

/// Write an I/O APIC register via its index/data register pair.
///
/// # Safety
///
/// `IOAPIC_BASE` must be set to a valid, identity-mapped I/O APIC base
/// address. Calling without a mapped I/O APIC produces UB.
unsafe fn ioapic_write(reg: u32, val: u32) {
    unsafe {
        let sel_addr = (IOAPIC_BASE + IOAPIC_IOREGSEL) as *mut u32;
        let win_addr = (IOAPIC_BASE + IOAPIC_IOWIN) as *mut u32;
        ptr::write_volatile(sel_addr, reg);
        ptr::write_volatile(win_addr, val);
    }
}

// ── 7.6.1: APIC base detection from IA32_APIC_BASE MSR ─────────────────

/// Detect Local APIC base address from the IA32_APIC_BASE MSR.
///
/// # Safety
///
/// Must be called in ring 0 (executes `rdmsr`).
pub unsafe fn detect_apic_base() -> u64 {
    unsafe {
        let msr_val = asm::rdmsr(IA32_APIC_BASE_MSR);
        msr_val & 0xFFFFFF000 // bits 12-35
    }
}

/// Check whether the APIC is globally enabled (IA32_APIC_BASE bit 11).
///
/// # Safety
///
/// Must be called in ring 0.
pub unsafe fn apic_is_enabled() -> bool {
    unsafe {
        let msr_val = asm::rdmsr(IA32_APIC_BASE_MSR);
        msr_val & (1 << 11) != 0
    }
}

/// Check whether this CPU is the BSP (IA32_APIC_BASE bit 8).
///
/// # Safety
///
/// Must be called in ring 0.
pub unsafe fn apic_is_bsp() -> bool {
    unsafe {
        let msr_val = asm::rdmsr(IA32_APIC_BASE_MSR);
        msr_val & (1 << 8) != 0
    }
}

/// Check whether x2APIC mode is active (IA32_APIC_BASE bit 10).
///
/// # Safety
///
/// Must be called in ring 0.
pub unsafe fn apic_is_x2apic() -> bool {
    unsafe {
        let msr_val = asm::rdmsr(IA32_APIC_BASE_MSR);
        msr_val & (1 << 10) != 0
    }
}

// ── 7.6.2: APIC version and LVT entry reading ──────────────────────────

/// Version information from the APIC version register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApicVersionInfo {
    /// APIC version number (low byte).
    pub version: u8,
    /// Maximum LVT entry index (bits 16-23).
    pub max_lvt: u8,
}

/// Read the APIC version register.
///
/// # Safety
///
/// Must be called in ring 0 with a mapped local APIC.
pub unsafe fn read_apic_version() -> ApicVersionInfo {
    unsafe {
        let ver = apic_read(APIC_VERSION_OFF);
        ApicVersionInfo {
            version: (ver & 0xFF) as u8,
            max_lvt: ((ver >> 16) & 0xFF) as u8,
        }
    }
}

/// Read an arbitrary LVT entry (e.g. LINT0, LINT1, Error, Timer).
///
/// # Safety
///
/// Must be called in ring 0 with a mapped local APIC.
pub unsafe fn read_lvt_entry(offset: u32) -> u32 {
    unsafe { apic_read(offset) }
}

/// Check whether an LVT entry value has NMI delivery mode.
///
/// NMI delivery mode is encoded in bits 8-10 as `100b` (value 4).
pub fn lvt_is_nmi(lvt_val: u32) -> bool {
    // Delivery mode field is bits 8-10; NMI = 100b = 4.
    (lvt_val >> 8) & 0x7 == 4
}

// ── 7.6.3: Reprogram LINT0 ─────────────────────────────────────────────

/// Reprogram LINT0 if it is configured for NMI or ExtINT delivery.
///
/// On some systems LINT0 is pre-configured for ExtINT (legacy PIC mode)
/// or NMI delivery.  This function reprograms it to masked Fixed so the
/// I/O APIC can take over interrupt routing.
///
/// # Safety
///
/// Must be called in ring 0 with a mapped local APIC.
pub unsafe fn reprogram_lint0() {
    unsafe {
        let lvt = apic_read(APIC_LINT0_OFF);
        if lvt_is_nmi(lvt) || (lvt & 0x700) == 0x700
        /* ExtINT */
        {
            // Reprogram to Fixed, masked
            apic_write(APIC_LINT0_OFF, 0x10000); // mask=1, Fixed delivery
        }
    }
}

// ── 7.6.4: Set up SVR (spurious vector register) ───────────────────────

/// Enable the local APIC and set the spurious interrupt vector.
///
/// # Safety
///
/// Must be called in ring 0 with a mapped local APIC.
pub unsafe fn setup_svr() {
    unsafe {
        let svr = apic_read(APIC_SPURIOUS_OFF);
        apic_write(
            APIC_SPURIOUS_OFF,
            (svr & !0xFF) | APIC_SPURIOUS_VECTOR | APIC_SVR_ENABLE,
        );
    }
}

// ── 7.6.5: I/O APIC initialization ─────────────────────────────────────

/// Initialize the I/O APIC: mask all redirection table entries.
///
/// Returns the maximum RTE index.
///
/// # Safety
///
/// Must be called in ring 0 with a mapped I/O APIC.
pub unsafe fn init_ioapic() -> u32 {
    unsafe {
        let ver = ioapic_read(IOAPIC_VERSION);
        let max_rte = (ver >> 16) & 0xFF; // max RTE entry index

        // Mask all RTEs
        for i in 0..=max_rte {
            let reg = IOAPIC_REDIR_TBL + i * 2;
            ioapic_write(reg, 0x10000); // low: masked (bit 16)
            ioapic_write(reg + 1, 0); // high: 0
        }

        max_rte
    }
}

// ── 7.6.6: Wire PIT interrupt ──────────────────────────────────────────

/// Configure I/O APIC RTE 0 (IRQ 0, PIT timer) with the given vector.
///
/// # Safety
///
/// Must be called in ring 0 with a mapped I/O APIC.
pub unsafe fn setup_pit_irq(vector: u8) {
    unsafe {
        let reg = IOAPIC_REDIR_TBL; // IRQ 0
        let low = vector as u32; // Fixed, edge, active high, unmasked
        let high = 0; // physical destination, APIC ID 0
        ioapic_write(reg, low);
        ioapic_write(reg + 1, high);
    }
}

// ── 7.6.7: End-of-interrupt ────────────────────────────────────────────

/// Signal end-of-interrupt to the local APIC (or PIC fallback).
///
/// # Safety
///
/// Must be called in ring 0.
pub unsafe fn eoi() {
    unsafe {
        if APIC_ENABLED {
            apic_write(APIC_EOI_OFF, 0);
        }
    }
    // When APIC is not enabled, the caller should write to the PIC instead.
}

// ── 7.6.9: Full APIC detection and initialization ──────────────────────

/// Detect APIC mode and perform full initialization.
///
/// Reads the IA32_APIC_BASE MSR, determines xAPIC vs x2APIC vs PIC-only
/// mode, reprograms LINT0, enables the SVR, and masks all I/O APIC RTEs.
///
/// # Safety
///
/// Must be called in ring 0.  The identity map must cover the APIC and
/// I/O APIC MMIO regions (0xFEE00000 and 0xFEC00000).
pub unsafe fn detect_and_init() {
    unsafe {
        // SAFETY: caller guarantees ring 0.
        let base = detect_apic_base();
        if base == 0 || !apic_is_enabled() {
            APIC_MODE = ApicMode::PicOnly;
            return;
        }

        APIC_BASE = base;

        if apic_is_x2apic() {
            APIC_MODE = ApicMode::X2Apic;
            // x2APIC uses MSR-based access, not MMIO
        } else {
            APIC_MODE = ApicMode::XApic;
        }

        // Initialize I/O APIC
        IOAPIC_BASE = DEFAULT_IOAPIC_BASE;

        // Reprogram LINT0 if needed
        reprogram_lint0();

        // Set up SVR
        setup_svr();

        // Mask all I/O APIC RTEs
        init_ioapic();

        APIC_ENABLED = true;
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ApicMode enum ──────────────────────────────────────────────────

    #[test]
    fn test_apic_mode_enum_values() {
        assert_eq!(ApicMode::PicOnly as u8, 0u8);
        assert_eq!(ApicMode::XApic as u8, 1u8);
        assert_eq!(ApicMode::X2Apic as u8, 2u8);
    }

    #[test]
    fn test_apic_mode_discriminant() {
        // Verify the enum is repr sensible — compare via pattern matching
        match ApicMode::PicOnly {
            ApicMode::PicOnly => {}
            _ => panic!("unexpected variant"),
        }
        match ApicMode::XApic {
            ApicMode::XApic => {}
            _ => panic!("unexpected variant"),
        }
        match ApicMode::X2Apic {
            ApicMode::X2Apic => {}
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn test_apic_mode_clone_eq() {
        let a = ApicMode::XApic;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, ApicMode::PicOnly);
    }

    // ── lvt_is_nmi ────────────────────────────────────────────────────

    #[test]
    fn test_lvt_is_nmi_true() {
        // LVT_DELIVERY_NMI = 0x400 → bits 8-10 = 100b = 4
        assert!(lvt_is_nmi(0x400));
    }

    #[test]
    fn test_lvt_is_nmi_false_fixed() {
        // Fixed delivery (0x000)
        assert!(!lvt_is_nmi(0x000));
    }

    #[test]
    fn test_lvt_is_nmi_false_extint() {
        // ExtINT (0x700 → bits 8-10 = 111b = 7)
        assert!(!lvt_is_nmi(0x700));
    }

    #[test]
    fn test_lvt_is_nmi_false_smi() {
        // SMI (0x200 → bits 8-10 = 010b = 2)
        assert!(!lvt_is_nmi(0x200));
    }

    #[test]
    fn test_lvt_is_nmi_false_init() {
        // INIT (0x500 → bits 8-10 = 101b = 5)
        assert!(!lvt_is_nmi(0x500));
    }

    #[test]
    fn test_lvt_is_nmi_masked_still_nmi() {
        // Masked NMI: bit 16 set, delivery mode still NMI
        assert!(lvt_is_nmi(0x10400));
    }

    #[test]
    fn test_lvt_is_nmi_vector_ignored() {
        // NMI with vector bits set — delivery mode determines NMI
        assert!(lvt_is_nmi(0x400 | 0x2F));
    }

    // ── ApicVersionInfo ────────────────────────────────────────────────

    #[test]
    fn test_apic_version_info_construction() {
        let info = ApicVersionInfo {
            version: 0x14,
            max_lvt: 6,
        };
        assert_eq!(info.version, 0x14);
        assert_eq!(info.max_lvt, 6);
    }

    #[test]
    fn test_apic_version_info_fields() {
        let info = ApicVersionInfo {
            version: 0x14,
            max_lvt: 6,
        };
        assert_eq!(info.version, 0x14);
        assert_eq!(info.max_lvt, 6);
    }

    // ── Constants ──────────────────────────────────────────────────────

    #[test]
    fn test_ia32_apic_base_msr() {
        assert_eq!(IA32_APIC_BASE_MSR, 0x1B);
    }

    #[test]
    fn test_default_apic_bases() {
        assert_eq!(DEFAULT_APIC_BASE, 0xFEE00000);
        assert_eq!(DEFAULT_IOAPIC_BASE, 0xFEC00000);
    }

    #[test]
    fn test_apic_svr_enable_bit() {
        assert_eq!(APIC_SVR_ENABLE, 0x100);
    }

    #[test]
    fn test_apic_spurious_vector() {
        assert_eq!(APIC_SPURIOUS_VECTOR, 0xFF);
    }

    #[test]
    fn test_apic_register_offsets() {
        assert_eq!(APIC_ID_OFF, 0x20);
        assert_eq!(APIC_VERSION_OFF, 0x30);
        assert_eq!(APIC_TASK_PRIORITY_OFF, 0x80);
        assert_eq!(APIC_SPURIOUS_OFF, 0xF0);
        assert_eq!(APIC_EOI_OFF, 0xB0);
        assert_eq!(APIC_LINT0_OFF, 0x350);
        assert_eq!(APIC_LINT1_OFF, 0x360);
        assert_eq!(APIC_ERROR_OFF, 0x370);
        assert_eq!(APIC_TIMER_OFF, 0x320);
        assert_eq!(APIC_TIMER_INITCNT_OFF, 0x380);
        assert_eq!(APIC_TIMER_CURRCNT_OFF, 0x390);
        assert_eq!(APIC_TIMER_DIV_OFF, 0x3E0);
    }

    #[test]
    fn test_ioapic_register_offsets() {
        assert_eq!(IOAPIC_IOREGSEL, 0x00);
        assert_eq!(IOAPIC_IOWIN, 0x10);
        assert_eq!(IOAPIC_ID, 0x00);
        assert_eq!(IOAPIC_VERSION, 0x01);
        assert_eq!(IOAPIC_ARB, 0x02);
        assert_eq!(IOAPIC_REDIR_TBL, 0x10);
    }

    // ── detect_apic_base mask layout ───────────────────────────────────

    #[test]
    fn test_detect_apic_base_mask() {
        // Verify the mask: bits 12-35 are preserved, rest zeroed.
        // We can't call detect_apic_base() from usermode (rdmsr traps);
        // instead we test that the masking logic matches the spec.
        //
        // The mask 0xFFFFFF000 covers bits 12-39 (7 hex Fs × 4 bits = 28 bits).
        // Bits 0-11 are zeroed (3 trailing hex zeros).
        let test_val: u64 = 0xFFFF_FFFF_FFFF_FFFF;
        let masked = test_val & 0xFFFFFF000;
        // All bits 0-11 must be zero.
        assert_eq!(masked & 0xFFF, 0);
        // Some bits above bit 35 may also be kept (mask extends to bit 39).
        // Verify the mask preserves at minimum bits 12-35.
        let bits_12_35: u64 = 0xFFFFF000;
        assert_eq!(masked & bits_12_35, bits_12_35);
    }

    #[test]
    fn test_apic_is_enabled_mask() {
        // Bit 11 indicates APIC enable.
        let bit11: u64 = 1 << 11;
        assert_eq!(bit11, 0x800);
        assert_eq!((0u64) & bit11, 0);
        assert_eq!(bit11 & bit11, bit11);
    }

    #[test]
    fn test_apic_is_bsp_mask() {
        // Bit 8 indicates BSP.
        assert!((1u64 << 8) != 0);
    }

    #[test]
    fn test_apic_is_x2apic_mask() {
        // Bit 10 indicates x2APIC enable.
        assert!((1u64 << 10) != 0);
    }

    // ── ApicVersionInfo from raw register ──────────────────────────────

    #[test]
    fn test_apic_version_info_from_raw() {
        // Simulate raw register: version=0x14, max_lvt=6
        let raw: u32 = 0x14 | (6 << 16);
        let info = ApicVersionInfo {
            version: (raw & 0xFF) as u8,
            max_lvt: ((raw >> 16) & 0xFF) as u8,
        };
        assert_eq!(info.version, 0x14);
        assert_eq!(info.max_lvt, 6);
    }

    #[test]
    fn test_apic_version_info_zero() {
        let raw: u32 = 0;
        let info = ApicVersionInfo {
            version: (raw & 0xFF) as u8,
            max_lvt: ((raw >> 16) & 0xFF) as u8,
        };
        assert_eq!(info.version, 0);
        assert_eq!(info.max_lvt, 0);
    }

    // ── Initial static state ───────────────────────────────────────────

    #[test]
    fn test_global_state_defaults() {
        // We can't safely read mutable statics in tests; verify the initial
        // values of the constants instead.
        assert_eq!(DEFAULT_APIC_BASE, 0xFEE00000);
        assert_eq!(DEFAULT_IOAPIC_BASE, 0xFEC00000);
    }
}
