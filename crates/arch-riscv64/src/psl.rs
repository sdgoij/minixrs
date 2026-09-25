//! RISC-V64 sstatus and sie CSR bit definitions.
//!
//! These are the RISC-V equivalents of x86_64's PSL (RFLAGS) register bits.

/// Supervisor Status Register (sstatus) bits.
pub mod sstatus {
    /// Supervisor Interrupt Enable (SIE).
    pub const SIE: u64 = 1 << 1;
    /// Supervisor Previous Interrupt Enable (SPIE).
    pub const SPIE: u64 = 1 << 5;
    /// Supervisor Previous Privilege (SPP): 1 = S-mode, 0 = U-mode.
    pub const SPP: u64 = 1 << 8;
    /// The FS field (bits 13-14): off, initial, clean, dirty.
    pub const FS_OFF: u64 = 0;
    pub const FS_INITIAL: u64 = 1 << 13;
    pub const FS_CLEAN: u64 = 2 << 13;
    pub const FS_DIRTY: u64 = 3 << 13;
    /// The XS field (bits 15-16): additional user-mode extensions.
    pub const XS_OFF: u64 = 0;
    pub const XS_INITIAL: u64 = 1 << 15;
    pub const XS_CLEAN: u64 = 2 << 15;
    pub const XS_DIRTY: u64 = 3 << 15;
    /// Supervisor User Memory access (SUM): allow S-mode to access U-mode pages.
    pub const SUM: u64 = 1 << 18;
    /// Make eXecutable (MX): make executable pages readable.
    pub const MXR: u64 = 1 << 19;
    /// User mode (UXL) — set to 64-bit (UXLEN = 2).
    pub const UXL64: u64 = 2 << 32;
}

/// Supervisor Interrupt Enable (sie) register bits.
pub mod sie {
    /// Supervisor Software Interrupt Enable (SSIE) — IPI.
    pub const SSIE: u64 = 1 << 1;
    /// Supervisor Timer Interrupt Enable (STIE).
    pub const STIE: u64 = 1 << 5;
    /// Supervisor External Interrupt Enable (SEIE) — PLIC.
    pub const SEIE: u64 = 1 << 9;
}

/// Default sstatus value for user space (interrupts enabled after sret, U-mode).
///
/// `SIE=0` is CRITICAL: prevents supervisor interrupts from firing between `csrw sstatus`
/// and `sret` in switch_to_user. The sret atomically copies SPIE to SIE.
///
/// The FS field is `Dirty` rather than `Initial`. Both mean "the FP unit is on",
/// but they make different promises about *where the state is*: `Initial` allows
/// an implementation to treat the write as `Off` and lets it drop the registers,
/// while `Dirty` says the registers hold the current state and must be preserved.
/// The second is what is true here — the registers are the state, and
/// `kernel::fpu` keeps a per-process image of them (saved at each context switch
/// and restored on the way back in, see that module) — and an FP unit that is off
/// turns the first floating-point instruction into an illegal instruction, which
/// is what a C program's first `double` hit.
///
/// Also note that this is what the trap return writes for a U-mode return
/// (`trap_asm.rs`), not only the entry paths: a process's own sstatus is not
/// carried reliably enough through fork/exec/syscall-return to be the only place
/// it is set.
pub const PSL_USERSET: u64 = sstatus::SPIE | sstatus::FS_DIRTY;

/// Default sstatus for kernel mode (interrupts disabled, S-mode, FS=initial).
///
/// Unused today: the kernel runs with whatever `sstatus` the trap brought in. If
/// it is ever loaded, the FS field wants the same care `PSL_USERSET` documents —
/// the kernel should use neither floating point nor the `Initial` encoding.
pub const PSL_KERNELSET: u64 = sstatus::SPP | sstatus::FS_INITIAL;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sstatus_bits() {
        assert_eq!(sstatus::SIE, 1 << 1);
        assert_eq!(sstatus::SPP, 1 << 8);
    }

    #[test]
    fn test_sie_bits() {
        assert_eq!(sie::STIE, 1 << 5);
        assert_eq!(sie::SEIE, 1 << 9);
    }

    #[test]
    fn test_psl_userset() {
        // SIE is deliberately zero: prevents interrupts between csrw sstatus
        // and sret in switch_to_user; sret atomically copies SPIE to SIE.
        assert_eq!(PSL_USERSET & sstatus::SIE, 0);
    }
}
