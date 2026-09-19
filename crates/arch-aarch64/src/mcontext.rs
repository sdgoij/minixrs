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

/// The saved-frame byte layout this type converts to and from.
///
/// AArch64 has a *single* layout: `p_reg` is the exception frame (the post-syscall
/// hook in `kernel-boot/src/aarch64.rs` copies 288 bytes straight across, and
/// `switch_to_user` and `set_initial_regs` name the same offsets). So x0..x30 sit at
/// 0..248, `SP_EL0` at 248, `ELR_EL1` at 256 and `SPSR_EL1` at 264. `tpidr_el0` is not
/// in the frame at all — the kernel keeps it in `Proc::p_tls` and reloads it on
/// every switch.
pub mod frame {
    /// Number of general-purpose register slots (x0..x30).
    pub const NR_GPRS: usize = 31;
    /// Offset of `SP_EL0`.
    pub const SP: usize = 248;
    /// Offset of `ELR_EL1` — the program counter.
    pub const PC: usize = 256;
    /// Offset of `SPSR_EL1` — the processor state.
    pub const PSTATE: usize = 264;
}

fn read_u64(frame: &[u8; 288], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&frame[offset..offset + 8]);
    u64::from_ne_bytes(bytes)
}

fn write_u64(frame: &mut [u8; 288], offset: usize, val: u64) {
    frame[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}

impl Mcontext {
    /// Read a machine context out of a saved frame.
    ///
    /// The FPU half comes back zeroed: the frame holds no FPU state, and the
    /// kernel's `do_getmcontext_handler` fills that half in when there is any.
    #[must_use]
    pub fn from_frame(frame: &[u8; 288]) -> Self {
        let mut mc = Self::default();
        // SAFETY: `Mcontext` is `repr(C)` and opens with its register fields
        // (x0..x30) as adjacent `u64`s — pinned by
        // `test_register_fields_are_adjacent` — so 31 words are writable at its
        // start, and the frame holds exactly those 31 register words at offset 0.
        // The two ranges cannot overlap: they are distinct objects.
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                (&mut mc as *mut Mcontext).cast::<u8>(),
                frame::NR_GPRS * core::mem::size_of::<u64>(),
            );
        }
        mc.mc_sp = read_u64(frame, frame::SP);
        mc.mc_pc = read_u64(frame, frame::PC);
        mc.mc_pstate = read_u64(frame, frame::PSTATE);
        mc
    }

    /// Write this context back into a saved frame.
    pub fn write_into_frame(&self, frame: &mut [u8; 288]) {
        // SAFETY: as in `from_frame`, the same 31 adjacent words, copied the other
        // way into a distinct object.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (self as *const Mcontext).cast::<u8>(),
                frame.as_mut_ptr(),
                frame::NR_GPRS * core::mem::size_of::<u64>(),
            );
        }
        write_u64(frame, frame::SP, self.mc_sp);
        write_u64(frame, frame::PC, self.mc_pc);
        write_u64(frame, frame::PSTATE, self.mc_pstate);
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

    /// The pointer-copy in `from_frame`/`write_into_frame` reads and writes the
    /// struct's opening words, so the register fields must be adjacent `u64`s in
    /// x0..x30 order. A named-field change would otherwise silently corrupt them.
    #[test]
    fn test_register_fields_are_adjacent() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(Mcontext, mc_x0), 0);
        assert_eq!(offset_of!(Mcontext, mc_x1), size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_x10), 10 * size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_x30), 30 * size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_sp), 31 * size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_pc), 32 * size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_pstate), 33 * size_of::<u64>());
    }

    /// These are the offsets `set_initial_regs`, `switch_to_user` and the
    /// post-syscall hook all use, pinned numerically so a change shows up here.
    #[test]
    fn test_frame_offsets() {
        assert_eq!(frame::NR_GPRS, 31);
        assert_eq!(frame::SP, 31 * size_of::<u64>());
        assert_eq!(frame::PC, 32 * size_of::<u64>());
        assert_eq!(frame::PSTATE, 33 * size_of::<u64>());
    }

    #[test]
    fn test_from_frame_maps_the_slots_it_names() {
        // Field by field rather than by round trip: a round trip is symmetric and
        // would pass with the stack pointer and program counter swapped.
        let mut f = [0u8; 288];
        f[0..8].copy_from_slice(&0xA0u64.to_ne_bytes()); // x0
        f[8..16].copy_from_slice(&0xA1u64.to_ne_bytes()); // x1
        f[240..248].copy_from_slice(&0xA30u64.to_ne_bytes()); // x30
        f[248..256].copy_from_slice(&0x5F00u64.to_ne_bytes()); // SP_EL0
        f[256..264].copy_from_slice(&0x5E9Cu64.to_ne_bytes()); // ELR_EL1
        f[264..272].copy_from_slice(&0x3C5u64.to_ne_bytes()); // SPSR_EL1

        let mc = Mcontext::from_frame(&f);
        assert_eq!(mc.mc_x0, 0xA0);
        assert_eq!(mc.mc_x1, 0xA1);
        assert_eq!(mc.mc_x30, 0xA30);
        assert_eq!(mc.mc_sp, 0x5F00);
        assert_eq!(mc.mc_pc, 0x5E9C);
        assert_eq!(mc.mc_pstate, 0x3C5);
        assert_eq!(mc.mc_fpstate, [0u8; 512]);
    }

    #[test]
    fn test_frame_roundtrip() {
        // Every slot the frame carries. Its 272..288 tail is not a register slot and
        // both directions leave it alone, so it stays zero on both sides.
        let mut f = [0u8; 288];
        for i in 0..=30u64 {
            let off = (i as usize) * size_of::<u64>();
            f[off..off + 8].copy_from_slice(&(0x2000 + i).to_ne_bytes());
        }
        f[248..256].copy_from_slice(&0x5F00u64.to_ne_bytes());
        f[256..264].copy_from_slice(&0x5E9Cu64.to_ne_bytes());
        f[264..272].copy_from_slice(&0x3C5u64.to_ne_bytes());

        let mc = Mcontext::from_frame(&f);
        let mut back = [0u8; 288];
        mc.write_into_frame(&mut back);
        assert_eq!(back, f);
    }
}
