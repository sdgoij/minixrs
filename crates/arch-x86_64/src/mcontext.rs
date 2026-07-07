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
        // GPRs: 16 x 8 = 128
        // rip/rsp/rflags: 3 x 8 = 24
        // Seg regs: 6 x 8 = 48
        // fpstate: 512
        // Total: 712
        assert!(size_of::<Mcontext>() >= 700 && size_of::<Mcontext>() <= 720);
    }

    #[test]
    fn test_mcontext_field_offsets() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(Mcontext, mc_rax), 0);
        assert_eq!(offset_of!(Mcontext, mc_rbx), 8);
        assert_eq!(offset_of!(Mcontext, mc_rcx), 16);
        assert_eq!(offset_of!(Mcontext, mc_rdi), 40);
        assert_eq!(offset_of!(Mcontext, mc_rip), 120);
        assert_eq!(offset_of!(Mcontext, mc_rsp), 128);
        assert_eq!(offset_of!(Mcontext, mc_rflags), 136);
        assert_eq!(offset_of!(Mcontext, mc_cs), 144);
        assert_eq!(offset_of!(Mcontext, mc_gs), 184);
        assert_eq!(offset_of!(Mcontext, mc_ss), 152);
        assert_eq!(offset_of!(Mcontext, mc_fpstate), 192);
    }

    #[test]
    fn test_mcontext_accessors() {
        let ctx = Mcontext {
            mc_rax: 0x42,
            mc_rbx: 0,
            mc_rcx: 0,
            mc_rdx: 0,
            mc_rsi: 0,
            mc_rdi: 0,
            mc_rbp: 0,
            mc_r8: 0,
            mc_r9: 0,
            mc_r10: 0,
            mc_r11: 0,
            mc_r12: 0,
            mc_r13: 0,
            mc_r14: 0,
            mc_r15: 0,
            mc_rip: 0x1000,
            mc_rsp: 0x2000,
            mc_rflags: 0x202,
            mc_cs: 0x08,
            mc_ss: 0x10,
            mc_ds: 0,
            mc_es: 0,
            mc_fs: 0,
            mc_gs: 0,
            mc_fpstate: [0u8; 512],
        };
        assert_eq!(ctx.mc_rax, 0x42);
        assert_eq!(ctx.mc_rip, 0x1000);
        assert_eq!(ctx.mc_rsp, 0x2000);
        assert_eq!(ctx.mc_rflags, 0x202);
        assert_eq!(ctx.mc_cs, 0x08);
    }
}
