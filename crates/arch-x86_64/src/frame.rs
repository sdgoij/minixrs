//! x86_64 stack frame structures — adapted from i386 `frame.h`
//!
//! **x86_64 differences from i386:**
//! - 16 general-purpose registers (rax–r15) instead of 8 (eax–edi)
//! - RIP instead of EIP, RSP instead of ESP, RFLAGS instead of EFLAGS
//! - TrapFrame may include CR2 for page fault address
//! - SwitchFrame saves callee-saved registers: rbx, rbp, r12–r15

use core::fmt;

/// Frame saved on kernel stack entry from user space.
/// This is the register state pushed by the `syscall` entry
/// or interrupt handler.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TrapFrame {
    /// General purpose registers.
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    /// Segment registers (not all used in 64-bit mode).
    pub cs: u64,
    pub ss: u64,
    pub ds: u64,
    pub es: u64,
    pub fs: u64,
    pub gs: u64,
    /// Instruction pointer, stack pointer, flags.
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
}

impl fmt::Debug for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrapFrame")
            .field("rip", &self.rip)
            .field("rsp", &self.rsp)
            .field("rflags", &self.rflags)
            .field("rax", &self.rax)
            .field("cs", &self.cs)
            .field("ss", &self.ss)
            .finish()
    }
}

/// Frame saved on interrupt entry (includes error code and vector).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IntrFrame {
    /// The trap frame (registers).
    pub tf: TrapFrame,
    /// Interrupt vector number.
    pub vector: u64,
    /// Error code (0 if none pushed).
    pub error_code: u64,
}

impl fmt::Debug for IntrFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntrFrame")
            .field("vector", &self.vector)
            .field("rip", &self.tf.rip)
            .finish()
    }
}

/// Minimal frame for context switching (callee-saved regs only).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SwitchFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    /// Return address (RIP) for the switch.
    pub retaddr: u64,
}

impl fmt::Debug for SwitchFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SwitchFrame")
            .field("retaddr", &self.retaddr)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_trap_frame_size() {
        // 14 GPRs (112) + 6 seg regs (48) + rip/rsp/rflags (24) = 184
        assert_eq!(size_of::<TrapFrame>(), 184);
    }

    #[test]
    fn test_intr_frame_size() {
        // TrapFrame(184) + vector(8) + error_code(8) = 200
        assert_eq!(size_of::<IntrFrame>(), 200);
    }

    #[test]
    fn test_switch_frame_size() {
        // r15-r12(32) + rbp(8) + rbx(8) + retaddr(8) = 56
        assert_eq!(size_of::<SwitchFrame>(), 56);
    }
}
