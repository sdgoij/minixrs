//! AArch64 stack frame structures.
//!
//! Matches the `arch-x86_64/src/frame.rs` pattern.
//! The TrapFrame is the register state saved on kernel entry from EL0.

use core::fmt;

/// Frame saved on kernel stack entry from EL0.
/// This matches the register save order in the exception vector handler.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TrapFrame {
    /// General purpose registers x0-x30.
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,
    pub x19: u64,
    pub x20: u64,
    pub x21: u64,
    pub x22: u64,
    pub x23: u64,
    pub x24: u64,
    pub x25: u64,
    pub x26: u64,
    pub x27: u64,
    pub x28: u64,
    pub x29: u64, // frame pointer
    pub x30: u64, // link register
    /// User stack pointer (SP_EL0).
    pub sp_el0: u64,
    /// Exception Link Register (return address).
    pub elr_el1: u64,
    /// Saved Program Status Register.
    pub spsr_el1: u64,
}

impl fmt::Debug for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrapFrame")
            .field("elr_el1", &self.elr_el1)
            .field("sp_el0", &self.sp_el0)
            .field("spsr_el1", &self.spsr_el1)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_trap_frame_size() {
        // 31 GPRs (248) + sp_el0(8) + elr_el1(8) + spsr_el1(8) = 272
        assert_eq!(size_of::<TrapFrame>(), 272);
    }
}
