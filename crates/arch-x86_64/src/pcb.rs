//! x86_64 Process Control Block — adapted from i386 `pcb.h`
//!
//! **x86_64 differences from i386:**
//! - All registers are 64-bit (rax, rbx, rcx, etc.)
//! - PCB saves CR0, CR2, CR3, CR4 as 64-bit values
//! - FPU save area is 512 bytes (FXSAVE/FXRSTOR format on x86_64)
//! - Additional registers: R8-R15

use core::fmt;

/// x86_64 Process Control Block.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Pcb {
    /// Saved CR0.
    pub pcb_cr0: u64,
    /// Saved CR2 (page fault linear address).
    pub pcb_cr2: u64,
    /// Saved CR3 (page table root).
    pub pcb_cr3: u64,
    /// Saved CR4.
    pub pcb_cr4: u64,
    /// FPU save area (512 bytes, FXSAVE/FXRSTOR format).
    pub pcb_fpusave: [u8; 512],
    /// Saved kernel stack pointer (for context switch).
    pub pcb_rsp0: u64,
    /// Reserved for future use.
    pub _reserved: [u64; 8],
}

impl fmt::Debug for Pcb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pcb")
            .field("cr3", &self.pcb_cr3)
            .field("rsp0", &self.pcb_rsp0)
            .finish()
    }
}

impl Default for Pcb {
    fn default() -> Self {
        Self {
            pcb_cr0: 0,
            pcb_cr2: 0,
            pcb_cr3: 0,
            pcb_cr4: 0,
            pcb_fpusave: [0u8; 512],
            pcb_rsp0: 0,
            _reserved: [0u64; 8],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_pcb_size() {
        // cr0/2/3/4: 4 × 8 = 32
        // fpusave: 512
        // rsp0: 8
        // reserved: 64
        // Total: 616
        assert_eq!(size_of::<Pcb>(), 616);
    }

    #[test]
    fn test_pcb_default() {
        let pcb = Pcb::default();
        assert_eq!(pcb.pcb_cr3, 0);
        assert!(pcb.pcb_fpusave.iter().all(|&b| b == 0));
    }
}
