//! x86_64 CPU MSR (Model-Specific Register) constants — adapted from `cpu_msr.h`
//!
//! **x86_64 differences from i386:**
//! - IA32_STAR (0xC0000081), IA32_LSTAR (0xC0000082), IA32_FMASK (0xC0000084)
//!   for `syscall`/`sysret` — not present on i386
//! - IA32_KERNEL_GS_BASE (0xC0000102) for `swapgs` per-CPU data
//! - IA32_APIC_BASE (0x1B) for local APIC base address
//! - EFER MSR (0xC0000080) for Syscall Enable, NXE, Long Mode Active

/// MSR address constants.
pub mod msr {
    /// Machine Check Address.
    pub const MCA: u32 = 0x0000_0000;
    /// Machine Check Type.
    pub const MCT: u32 = 0x0000_0001;

    /// APIC base address (local APIC enable, BSP).
    pub const APIC_BASE: u32 = 0x0000_001B;

    /// Time-Stamp Counter.
    pub const TSC: u32 = 0x0000_0010;

    /// C1E enable/disable (AMD).
    pub const C1E: u32 = 0xC001_0016;

    // ── SYSCALL/SYSRET MSRs (x86_64 only) ────────────────────────────────

    /// Extended Feature Enable Register.
    /// Bit 0: SCE (Syscall Enable)
    /// Bit 8: LME (Long Mode Enable)
    /// Bit 10: LMA (Long Mode Active, read-only)
    /// Bit 11: NXE (No-Execute Enable)
    pub const EFER: u32 = 0xC000_0080;

    /// STAR — Syscall Target Address Register (CS/SS selectors).
    /// Bits 31-0: SYSCALL EIP (unused on x86_64)
    /// Bits 47-32: SYSRET CS/SS selector
    /// Bits 63-48: SYSCALL CS/SS selector
    pub const STAR: u32 = 0xC000_0081;

    /// LSTAR — Long Mode SYSCALL Target Address (RIP for syscall entry).
    pub const LSTAR: u32 = 0xC000_0082;

    /// CSTAR — Compatibility Mode SYSCALL Target Address (unused on native).
    pub const CSTAR: u32 = 0xC000_0083;

    /// SF_MASK — SYSCALL Flag Mask (clears RFLAGS bits on syscall).
    pub const SF_MASK: u32 = 0xC000_0084;

    // ── FS/GS BASE MSRs ───────────────────────────────────────────────────

    /// IA32_FS_BASE — User-mode FS segment base.
    pub const FS_BASE: u32 = 0xC000_0100;
    /// IA32_GS_BASE — User-mode GS segment base.
    pub const GS_BASE: u32 = 0xC000_0101;
    /// IA32_KERNEL_GS_BASE — Kernel-mode GS base (used by `swapgs`).
    /// Per Intel SDM Vol 4 Table 2-7: MSR at 0xC0000102.
    pub const KERNEL_GS_BASE: u32 = 0xC000_0102;

    // ── Other common MSRs ─────────────────────────────────────────────────

    pub const PAT: u32 = 0x0000_0277;
    pub const MTRR_CAP: u32 = 0x0000_00FE;
    pub const MTRR_DEF_TYPE: u32 = 0x0000_02FF;
    pub const MTRR_PHYS_BASE: u32 = 0x0000_0200;
    pub const MTRR_PHYS_MASK: u32 = 0x0000_0201;
    pub const MTRR_FIX64K: u32 = 0x0000_0250;
    pub const MTRR_FIX16K: u32 = 0x0000_0258;
    pub const MTRR_FIX4K: u32 = 0x0000_0268;

    /// Local APIC ID.
    pub const APIC_ID: u32 = 0x0000_0802;
    pub const APIC_VERSION: u32 = 0x0000_0803;
    pub const APIC_TPR: u32 = 0x0000_0808;
    pub const APIC_SPIV: u32 = 0x0000_080F;

    pub const MICROCODE_UPDATE: u32 = 0x0000_0079;
}

// ── EFER bit definitions ────────────────────────────────────────────────

pub mod efer {
    pub const SCE: u64 = 1 << 0; // Syscall Enable
    pub const LME: u64 = 1 << 8; // Long Mode Enable
    pub const LMA: u64 = 1 << 10; // Long Mode Active
    pub const NXE: u64 = 1 << 11; // No-Execute Enable
}

// ── STAR bit layout ─────────────────────────────────────────────────────
// STAR bits 47:32 = SYSRET CS/SS selector (SysCallEIP is unused on x86_64)

/// Extract the SYSRET selector from STAR.
pub const fn star_sysret_sel(star: u64) -> u16 {
    ((star >> 32) & 0xFFFF) as u16
}

/// Extract the SYSCALL selector from STAR.
pub const fn star_syscall_sel(star: u64) -> u16 {
    ((star >> 48) & 0xFFFF) as u16
}

/// Build STAR from SYSCALL and SYSRET selectors.
/// Typical values: SYSCALL CS=0x0008 (kernel code, GDT index 1, RPL=0),
///                 SYSRET CS=0x001B (user code, GDT index 3, RPL=3)
pub const fn make_star(syscall_cs: u16, sysret_cs: u16) -> u64 {
    (sysret_cs as u64) << 32 | (syscall_cs as u64) << 48
}

/// Enable EFER bits needed for long mode operation.
///
/// Sets:
/// - EFER.NXE (bit 11): No-Execute enable — PG_NX is honored
/// - EFER.SCE (bit  0): Syscall Enable — enables `syscall`/`sysret`
///
/// EFER.LME (bit 8) and EFER.LMA (bit 10) are set by the trampoline
/// when entering long mode and should NOT be modified here.
///
/// # Safety
///
/// Must be called in ring 0.
pub unsafe fn enable_nxe_and_sce() {
    unsafe {
        let efer = crate::asm::rdmsr(msr::EFER);
        crate::asm::wrmsr(msr::EFER, efer | efer::NXE | efer::SCE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_msr_constants() {
        assert_eq!(msr::APIC_BASE, 0x001B);
        assert_eq!(msr::EFER, 0xC000_0080);
        assert_eq!(msr::STAR, 0xC000_0081);
        assert_eq!(msr::LSTAR, 0xC000_0082);
        assert_eq!(msr::SF_MASK, 0xC000_0084);
        assert_eq!(msr::KERNEL_GS_BASE, 0xC000_0102);
    }

    #[test]
    fn test_efer_bits() {
        assert_eq!(efer::SCE, 1);
        assert_eq!(efer::LME, 1 << 8);
        assert_eq!(efer::LMA, 1 << 10);
        assert_eq!(efer::NXE, 1 << 11);
    }

    #[test]
    fn test_star_values() {
        // Typical x86_64 kernel: SYSCALL CS=0x08, SYSRET to user CS=0x1B
        let star = make_star(0x08, 0x1B);
        assert_eq!(star_sysret_sel(star), 0x1B);
        assert_eq!(star_syscall_sel(star), 0x08);
    }

    #[test]
    fn test_kernel_gs_base_msr() {
        // Per Intel SDM Vol 4 Table 2-7
        assert_eq!(msr::KERNEL_GS_BASE, 0xC000_0102);
    }
}
