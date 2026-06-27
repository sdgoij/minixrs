//! x86_64 RFLAGS register bits — adapted from `psl.h`
//!
//! **x86_64 differences from i386:**
//! - Same bit layout for the lower 32 bits (compatible with i386)
//! - Upper 32 bits are reserved on x86_64 (must be 0)
//! - No virtual 8086 mode (VM flag) in long mode — always 0
//! - RFLAGS is 64-bit wide on the stack/register

/// x86_64 RFLAGS register bit definitions.
pub mod rflags {
    pub const C: u64 = 0x00000001; // Carry
    pub const PF: u64 = 0x00000004; // Parity
    pub const AF: u64 = 0x00000010; // Auxiliary carry
    pub const Z: u64 = 0x00000040; // Zero
    pub const N: u64 = 0x00000080; // Sign
    pub const T: u64 = 0x00000100; // Trap (single-step)
    pub const I: u64 = 0x00000200; // Interrupt enable
    pub const D: u64 = 0x00000400; // Direction
    pub const V: u64 = 0x00000800; // Overflow
    pub const IOPL: u64 = 0x00003000; // I/O privilege level (2 bits)
    pub const NT: u64 = 0x00004000; // Nested task
    pub const RF: u64 = 0x00010000; // Resume
    // VM: 0x00020000 — not valid in long mode
    pub const AC: u64 = 0x00040000; // Alignment check
    pub const VIF: u64 = 0x00080000; // Virtual interrupt enable
    pub const VIP: u64 = 0x00100000; // Virtual interrupt pending
    pub const ID: u64 = 0x00200000; // Identification

    /// Must-be-one bits.
    pub const MBO: u64 = 0x00000002;
    /// Must-be-zero bits (upper 32 bits must be zero).
    pub const MBZ: u64 = 0xFFC08028;
}

/// Default RFLAGS value for user space (interrupts enabled).
pub const PSL_USERSET: u64 = rflags::MBO | rflags::I;

/// RFLAGS bits that user space can change.
pub const PSL_USER: u64 = rflags::C
    | rflags::PF
    | rflags::AF
    | rflags::Z
    | rflags::N
    | rflags::T
    | rflags::V
    | rflags::D
    | rflags::AC;

/// RFLAGS bits cleared on signal delivery.
pub const PSL_CLEARSIG: u64 = rflags::T | rflags::AC | rflags::D;

/// Static bits user space cannot change.
pub const PSL_USERSTATIC: u64 =
    rflags::MBO | rflags::MBZ | rflags::I | rflags::IOPL | rflags::NT | rflags::VIF | rflags::VIP;

/// Extract the IOPL from RFLAGS.
pub const fn iopl(rf: u64) -> u64 {
    (rf & rflags::IOPL) >> 12
}

/// Check if interrupts are enabled.
pub const fn intr_enabled(rf: u64) -> bool {
    (rf & rflags::I) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rflags_bits() {
        assert_eq!(rflags::C, 0x00000001);
        assert_eq!(rflags::I, 0x00000200);
        assert_eq!(rflags::IOPL, 0x00003000);
        assert_eq!(rflags::AC, 0x00040000);
    }

    #[test]
    fn test_mbo() {
        // Bit 1 must always be set
        assert_eq!(rflags::MBO, 0x00000002);
    }

    #[test]
    fn test_iopl_extract() {
        assert_eq!(iopl(0), 0);
        assert_eq!(iopl(0x00003000), 3);
    }

    #[test]
    fn test_intr_enabled() {
        assert!(intr_enabled(0x00000202)); // MBO | I
        assert!(!intr_enabled(0x00000002)); // MBO only
    }
}
