//! RISC-V64 machine context (for future signal handling).
//!
//! Matches the `arch-x86_64/src/mcontext.rs` pattern.
//! The register layout follows the RISC-V supervisor ABI:
//! 32 GPRs (x0–x31), then sepc, sstatus, and FPU state.

use core::fmt;

/// RISC-V64 machine context (signal context).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Mcontext {
    /// General purpose registers x1–x31 (x0 = zero, always 0).
    pub mc_ra: u64, // x1
    pub mc_sp: u64,  // x2
    pub mc_gp: u64,  // x3
    pub mc_tp: u64,  // x4
    pub mc_t0: u64,  // x5
    pub mc_t1: u64,  // x6
    pub mc_t2: u64,  // x7
    pub mc_s0: u64,  // x8 (frame pointer)
    pub mc_s1: u64,  // x9
    pub mc_a0: u64,  // x10
    pub mc_a1: u64,  // x11
    pub mc_a2: u64,  // x12
    pub mc_a3: u64,  // x13
    pub mc_a4: u64,  // x14
    pub mc_a5: u64,  // x15
    pub mc_a6: u64,  // x16
    pub mc_a7: u64,  // x17
    pub mc_s2: u64,  // x18
    pub mc_s3: u64,  // x19
    pub mc_s4: u64,  // x20
    pub mc_s5: u64,  // x21
    pub mc_s6: u64,  // x22
    pub mc_s7: u64,  // x23
    pub mc_s8: u64,  // x24
    pub mc_s9: u64,  // x25
    pub mc_s10: u64, // x26
    pub mc_s11: u64, // x27
    pub mc_t3: u64,  // x28
    pub mc_t4: u64,  // x29
    pub mc_t5: u64,  // x30
    pub mc_t6: u64,  // x31
    /// Supervisor exception program counter.
    pub mc_sepc: u64,
    /// Supervisor status register.
    pub mc_sstatus: u64,
    /// FPU state (for F/D extension, 256 bytes).
    pub mc_fpstate: [u8; 256],
}

impl fmt::Debug for Mcontext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mcontext")
            .field("mc_sepc", &self.mc_sepc)
            .field("mc_sp", &self.mc_sp)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_mcontext_size() {
        // 31 GPRs (248) + sepc(8) + sstatus(8) + fpstate(256) = 520
        assert_eq!(size_of::<Mcontext>(), 520);
    }
}
