//! RISC-V64 sstatus and sie CSR bit definitions.
//!
//! These are the RISC-V equivalents of x86_64's PSL (RFLAGS) register bits.

/// Supervisor Status Register (sstatus) bits.
pub mod sstatus {
    /// Supervisor Interrupt Enable (SIE).
    pub const SIE: u64 = 1 << 1;
    /// Supervisor Previous Interrupt Enable (SPIE).
    pub const SPIE: u64 = 1 << 5;
    /// Supervisor Previous Privilege (SPP): 1 = S-mode, 0 = U-mode.
    pub const SPP: u64 = 1 << 8;
    /// The FS field (bits 13-14): off, initial, clean, dirty.
    pub const FS_OFF: u64 = 0;
    pub const FS_INITIAL: u64 = 1 << 13;
    pub const FS_CLEAN: u64 = 2 << 13;
    pub const FS_DIRTY: u64 = 3 << 13;
    /// The XS field (bits 15-16): additional user-mode extensions.
    pub const XS_OFF: u64 = 0;
    pub const XS_INITIAL: u64 = 1 << 15;
    pub const XS_CLEAN: u64 = 2 << 15;
    pub const XS_DIRTY: u64 = 3 << 15;
    /// Supervisor User Memory access (SUM): allow S-mode to access U-mode pages.
    pub const SUM: u64 = 1 << 18;
    /// Make eXecutable (MX): make executable pages readable.
    pub const MXR: u64 = 1 << 19;
    /// User mode (UXL) — set to 64-bit (UXLEN = 2).
    pub const UXL64: u64 = 2 << 32;
}

/// Supervisor Interrupt Enable (sie) register bits.
pub mod sie {
    /// Supervisor Software Interrupt Enable (SSIE) — IPI.
    pub const SSIE: u64 = 1 << 1;
    /// Supervisor Timer Interrupt Enable (STIE).
    pub const STIE: u64 = 1 << 5;
    /// Supervisor External Interrupt Enable (SEIE) — PLIC.
    pub const SEIE: u64 = 1 << 9;
}

/// Default sstatus value for user space (interrupts enabled after sret, U-mode, FS=initial).
/// SIE=0 is CRITICAL: prevents supervisor interrupts from firing between `csrw sstatus`
/// and `sret` in switch_to_user. The sret atomically copies SPIE to SIE.
pub const PSL_USERSET: u64 = sstatus::SPIE | sstatus::FS_INITIAL;

/// Default sstatus for kernel mode (interrupts disabled, S-mode, FS=initial).
pub const PSL_KERNELSET: u64 = sstatus::SPP | sstatus::FS_INITIAL;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sstatus_bits() {
        assert_eq!(sstatus::SIE, 1 << 1);
        assert_eq!(sstatus::SPP, 1 << 8);
    }

    #[test]
    fn test_sie_bits() {
        assert_eq!(sie::STIE, 1 << 5);
        assert_eq!(sie::SEIE, 1 << 9);
    }

    #[test]
    fn test_psl_userset() {
        assert!(PSL_USERSET & sstatus::SIE != 0);
    }
}
