//! Machine context, matching the other ports' field shape so the kernel's
//! signal path (`system.rs`) compiles and its struct-copy logic stays honest.

use core::fmt;

/// Saved machine context, as handed to and from user space on signal delivery.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Mcontext {
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
    pub mc_rip: u64,
    pub mc_rsp: u64,
    pub mc_rflags: u64,
    pub mc_cs: u64,
    pub mc_ss: u64,
    pub mc_ds: u64,
    pub mc_es: u64,
    pub mc_fs: u64,
    pub mc_gs: u64,
    pub mc_fpstate: [u8; super::hal::FPU_STATE_SIZE],
}

impl Default for Mcontext {
    fn default() -> Self {
        Self {
            mc_rax: 0,
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
            mc_rip: 0,
            mc_rsp: 0,
            mc_rflags: 0,
            mc_cs: 0,
            mc_ss: 0,
            mc_ds: 0,
            mc_es: 0,
            mc_fs: 0,
            mc_gs: 0,
            mc_fpstate: [0; super::hal::FPU_STATE_SIZE],
        }
    }
}

impl fmt::Debug for Mcontext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mcontext")
            .field("mc_rip", &self.mc_rip)
            .field("mc_rsp", &self.mc_rsp)
            .finish()
    }
}
