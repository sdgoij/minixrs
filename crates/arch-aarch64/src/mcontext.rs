//! AArch64 machine context (for future signal handling).
//!
//! Matches the `arch-x86_64/src/mcontext.rs` pattern.
//! The register layout follows the AArch64 calling convention:
//! 31 GPRs (x0-x30), SP, PC, PSTATE, and FPU state.

use core::fmt;

/// AArch64 machine context (signal context).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Mcontext {
    /// General purpose registers x0-x30.
    pub mc_x0: u64,
    pub mc_x1: u64,
    pub mc_x2: u64,
    pub mc_x3: u64,
    pub mc_x4: u64,
    pub mc_x5: u64,
    pub mc_x6: u64,
    pub mc_x7: u64,
    pub mc_x8: u64,
    pub mc_x9: u64,
    pub mc_x10: u64,
    pub mc_x11: u64,
    pub mc_x12: u64,
    pub mc_x13: u64,
    pub mc_x14: u64,
    pub mc_x15: u64,
    pub mc_x16: u64,
    pub mc_x17: u64,
    pub mc_x18: u64,
    pub mc_x19: u64,
    pub mc_x20: u64,
    pub mc_x21: u64,
    pub mc_x22: u64,
    pub mc_x23: u64,
    pub mc_x24: u64,
    pub mc_x25: u64,
    pub mc_x26: u64,
    pub mc_x27: u64,
    pub mc_x28: u64,
    pub mc_x29: u64, // frame pointer
    pub mc_x30: u64, // link register
    /// Stack pointer, program counter, processor state.
    pub mc_sp: u64,
    pub mc_pc: u64,
    pub mc_pstate: u64,
    /// FPU state (512 bytes, NEON/VFP).
    pub mc_fpstate: [u8; 512],
}

impl fmt::Debug for Mcontext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mcontext")
            .field("mc_pc", &self.mc_pc)
            .field("mc_sp", &self.mc_sp)
            .finish()
    }
}

impl Default for Mcontext {
    fn default() -> Self {
        // SAFETY: all-zero is a valid (if degenerate) machine context.
        unsafe { core::mem::zeroed() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_mcontext_size() {
        // 31 GPRs (248) + sp/pc/pstate (24) + fpstate (512) = 784
        assert!(size_of::<Mcontext>() >= 780 && size_of::<Mcontext>() <= 790);
    }
}
