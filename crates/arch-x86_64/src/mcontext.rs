//! x86_64 machine context — adapted from i386 `mcontext.h`
//!
//! **x86_64 differences from i386:**
//! - Full 64-bit GPR set: rax, rbx, rcx, rdx, rsi, rdi, rbp, r8–r15
//! - 64-bit RIP, RSP, RFLAGS
//! - FPU/XMM state uses 512-byte FXSAVE/FXRSTOR format (same as i386)
//! - SS is needed explicitly (not implicitly from CS)
//! - Different signal stack frame layout

use core::fmt;

/// x86_64 machine context (signal context).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Mcontext {
    /// General purpose registers.
    pub mc_rax: u64,
    pub mc_rbx: u64,
    pub mc_rcx: u64,
    pub mc_rdx: u64,
    pub mc_rsi: u64,
    pub mc_rdi: u64,
    pub mc_rbp: u64,
    pub mc_r8: u64,
    pub mc_r9: u64,
    pub mc_r10: u64,
    pub mc_r11: u64,
    pub mc_r12: u64,
    pub mc_r13: u64,
    pub mc_r14: u64,
    pub mc_r15: u64,
    /// Program counter, stack pointer, flags.
    pub mc_rip: u64,
    pub mc_rsp: u64,
    pub mc_rflags: u64,
    /// Segment registers.
    pub mc_cs: u64,
    pub mc_ss: u64,
    pub mc_ds: u64,
    pub mc_es: u64,
    pub mc_fs: u64,
    pub mc_gs: u64,
    /// FPU state (512 bytes, FXSAVE/FXRSTOR format).
    pub mc_fpstate: [u8; 512],
}

impl fmt::Debug for Mcontext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mcontext")
            .field("mc_rip", &self.mc_rip)
            .field("mc_rsp", &self.mc_rsp)
            .field("mc_rflags", &self.mc_rflags)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_mcontext_size() {
        // GPRs: 16 × 8 = 128
        // rip/rsp/rflags: 3 × 8 = 24
        // Seg regs: 6 × 8 = 48
        // fpstate: 512
        // Total: 712
        assert!(size_of::<Mcontext>() >= 700 && size_of::<Mcontext>() <= 720);
    }
}
