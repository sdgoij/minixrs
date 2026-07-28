//! RISC-V64 stack frame structures.
//!
//! Matches the `arch-x86_64/src/frame.rs` pattern.
//! The TrapFrame is the register state saved on kernel entry from user space.

use core::fmt;

/// Frame saved on kernel stack entry from user space.
/// This is the register state pushed by the trap handler.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TrapFrame {
    /// General purpose registers x1–x31.
    pub ra: u64, // x1
    pub sp: u64,  // x2
    pub gp: u64,  // x3
    pub tp: u64,  // x4
    pub t0: u64,  // x5
    pub t1: u64,  // x6
    pub t2: u64,  // x7
    pub s0: u64,  // x8 (fp)
    pub s1: u64,  // x9
    pub a0: u64,  // x10
    pub a1: u64,  // x11
    pub a2: u64,  // x12
    pub a3: u64,  // x13
    pub a4: u64,  // x14
    pub a5: u64,  // x15
    pub a6: u64,  // x16
    pub a7: u64,  // x17
    pub s2: u64,  // x18
    pub s3: u64,  // x19
    pub s4: u64,  // x20
    pub s5: u64,  // x21
    pub s6: u64,  // x22
    pub s7: u64,  // x23
    pub s8: u64,  // x24
    pub s9: u64,  // x25
    pub s10: u64, // x26
    pub s11: u64, // x27
    pub t3: u64,  // x28
    pub t4: u64,  // x29
    pub t5: u64,  // x30
    pub t6: u64,  // x31
    /// Supervisor exception program counter.
    pub sepc: u64,
    /// Supervisor status register.
    pub sstatus: u64,
}

impl fmt::Debug for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrapFrame")
            .field("sepc", &self.sepc)
            .field("sp", &self.sp)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_trap_frame_size() {
        // 31 GPRs (248) + sepc(8) + sstatus(8) = 264
        assert_eq!(size_of::<TrapFrame>(), 264);
    }
}
