//! AArch64 PSTATE/SPSR bit definitions.
//!
//! These are the AArch64 equivalents of x86_64's PSL (RFLAGS) register bits.

/// Processor State bits (used in SPSR_EL1).
pub mod spsr {
    /// Mode field [3:0]: 0b0000 = EL0t.
    pub const M_EL0T: u64 = 0b0000;
    /// Mode field [3:0]: 0b0100 = EL1t.
    pub const M_EL1T: u64 = 0b0100;
    /// Mode field [3:0]: 0b0101 = EL1h.
    pub const M_EL1H: u64 = 0b0101;

    /// DAIF mask bits [9:6]: Debug, SError, IRQ, FIQ.
    pub const D: u64 = 1 << 9;
    pub const A: u64 = 1 << 8;
    pub const I: u64 = 1 << 7;
    pub const F: u64 = 1 << 6;
    pub const DAIF_ALL: u64 = D | A | I | F;

    /// nRW bit [4]: 0 = AArch64, 1 = AArch32.
    pub const NRW_AARCH32: u64 = 1 << 4;
}

/// Default SPSR value for EL0: AArch64, all masked, EL0t.
/// Interrupts are unmasked via the eret instruction which copies
/// SPSR → PSTATE.
pub const PSL_USERSET: u64 = 0;

/// Default SPSR for EL1: AArch64, DAIF masked, EL1h.
pub const PSL_KERNELSET: u64 = spsr::M_EL1H | spsr::DAIF_ALL;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spsr_bits() {
        assert_eq!(spsr::M_EL0T, 0);
        assert_eq!(spsr::M_EL1H, 5);
        assert_eq!(spsr::DAIF_ALL, 0x3C0);
    }

    #[test]
    fn test_psl_userset() {
        // EL0t, AArch64, all masked initially
        assert_eq!(PSL_USERSET, 0);
    }
}
