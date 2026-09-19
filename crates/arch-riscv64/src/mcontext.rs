//! RISC-V64 machine context (for future signal handling).
//!
//! Matches the `arch-x86_64/src/mcontext.rs` pattern.
//! The register layout follows the RISC-V supervisor ABI:
//! 32 GPRs (x0–x31), then sepc, sstatus, and FPU state.

use core::fmt;

/// RISC-V64 machine context (signal context).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Mcontext {
    /// General purpose registers x1–x31 (x0 = zero, always 0).
    pub mc_ra: u64, // x1
    pub mc_sp: u64,  // x2
    pub mc_gp: u64,  // x3
    pub mc_tp: u64,  // x4
    pub mc_t0: u64,  // x5
    pub mc_t1: u64,  // x6
    pub mc_t2: u64,  // x7
    pub mc_s0: u64,  // x8 (frame pointer)
    pub mc_s1: u64,  // x9
    pub mc_a0: u64,  // x10
    pub mc_a1: u64,  // x11
    pub mc_a2: u64,  // x12
    pub mc_a3: u64,  // x13
    pub mc_a4: u64,  // x14
    pub mc_a5: u64,  // x15
    pub mc_a6: u64,  // x16
    pub mc_a7: u64,  // x17
    pub mc_s2: u64,  // x18
    pub mc_s3: u64,  // x19
    pub mc_s4: u64,  // x20
    pub mc_s5: u64,  // x21
    pub mc_s6: u64,  // x22
    pub mc_s7: u64,  // x23
    pub mc_s8: u64,  // x24
    pub mc_s9: u64,  // x25
    pub mc_s10: u64, // x26
    pub mc_s11: u64, // x27
    pub mc_t3: u64,  // x28
    pub mc_t4: u64,  // x29
    pub mc_t5: u64,  // x30
    pub mc_t6: u64,  // x31
    /// Supervisor exception program counter.
    pub mc_sepc: u64,
    /// Supervisor status register.
    pub mc_sstatus: u64,
    /// FPU state (for F/D extension, 256 bytes).
    pub mc_fpstate: [u8; 256],
}

impl fmt::Debug for Mcontext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mcontext")
            .field("mc_sepc", &self.mc_sepc)
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

/// The `p_reg` byte layout this type converts to and from.
///
/// It is deliberately *not* the trap frame's layout: `p_reg` carries `sepc` in x0's
/// slot, the thirty registers x1..x30, and `sstatus` in x31's slot; the trap frame
/// instead holds all 32 registers at 0..248 with `sepc`/`sstatus` after them, and
/// the conversion between the two happens in the post-syscall hook
/// (`kernel-boot/src/riscv64.rs`, which documents both). x31 — `t6` — has no `p_reg`
/// slot because it need not survive a trap: `switch_to_user` keeps the register for
/// its own use, and the psABI lets a syscall clobber a caller-saved temp. The kernel
/// keeps the real value in `Proc::p_t6`, which this type cannot reach.
pub mod preg {
    /// Offset of `sepc`.
    pub const SEPC: usize = 0;
    /// Offset of the first register, `ra` (x1).
    pub const GPRS: usize = 8;
    /// Number of register slots in `p_reg` (x1..x30); x31 is absent.
    pub const NR_GPRS: usize = 30;
    /// Offset of `sstatus` — x31's slot in the trap frame, reused.
    pub const SSTATUS: usize = 248;
}

fn read_u64(frame: &[u8; 256], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&frame[offset..offset + 8]);
    u64::from_ne_bytes(bytes)
}

fn write_u64(frame: &mut [u8; 256], offset: usize, val: u64) {
    frame[offset..offset + 8].copy_from_slice(&val.to_ne_bytes());
}

impl Mcontext {
    /// Read a machine context out of a saved `p_reg`.
    ///
    /// `mc_t6` comes back zeroed and the FPU half stays zeroed: neither has a source
    /// here (see [`preg`] and the type docs).
    #[must_use]
    pub fn from_frame(frame: &[u8; 256]) -> Self {
        let mut mc = Self::default();
        // SAFETY: `Mcontext` is `repr(C)` and opens with its register fields
        // (x1..x31) as adjacent `u64`s — pinned by
        // `test_register_fields_are_adjacent` — so 30 words are writable at its
        // start, and the frame holds 30 register words at `preg::GPRS`. The two
        // ranges cannot overlap: they are distinct objects.
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame.as_ptr().add(preg::GPRS),
                (&mut mc as *mut Mcontext).cast::<u8>(),
                preg::NR_GPRS * core::mem::size_of::<u64>(),
            );
        }
        mc.mc_sepc = read_u64(frame, preg::SEPC);
        mc.mc_sstatus = read_u64(frame, preg::SSTATUS);
        mc
    }

    /// Write this context back into a saved `p_reg`.
    ///
    /// `mc_t6` is not written anywhere: `p_reg` has no slot for it (see [`preg`]).
    pub fn write_into_frame(&self, frame: &mut [u8; 256]) {
        // SAFETY: as in `from_frame`, the same 30 adjacent words, copied the other
        // way into a distinct object.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (self as *const Mcontext).cast::<u8>(),
                frame.as_mut_ptr().add(preg::GPRS),
                preg::NR_GPRS * core::mem::size_of::<u64>(),
            );
        }
        write_u64(frame, preg::SEPC, self.mc_sepc);
        write_u64(frame, preg::SSTATUS, self.mc_sstatus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_mcontext_size() {
        // 31 GPRs (248) + sepc(8) + sstatus(8) + fpstate(256) = 520
        assert_eq!(size_of::<Mcontext>(), 520);
    }

    /// The pointer-copy in `from_frame`/`write_into_frame` reads and writes the
    /// struct's opening words, so the register fields must be adjacent `u64`s in
    /// x1..x31 order. A named-field change would otherwise silently corrupt them.
    #[test]
    fn test_register_fields_are_adjacent() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(Mcontext, mc_ra), 0);
        assert_eq!(offset_of!(Mcontext, mc_sp), size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_a0), 9 * size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_t5), 29 * size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_t6), 30 * size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_sepc), 31 * size_of::<u64>());
        assert_eq!(offset_of!(Mcontext, mc_sstatus), 32 * size_of::<u64>());
    }

    /// The `p_reg` offsets the conversion uses are the ones the rest of the port
    /// uses: `switch_to_user` reads `sepc` at 0, and the post-syscall hook puts
    /// `sstatus` at 248. Pinned numerically so a change to either shows up here.
    #[test]
    fn test_preg_offsets() {
        assert_eq!(preg::SEPC, 0);
        assert_eq!(preg::GPRS, size_of::<u64>());
        assert_eq!(preg::NR_GPRS, 30);
        assert_eq!(preg::SSTATUS, 31 * size_of::<u64>());
    }

    #[test]
    fn test_from_frame_maps_the_slots_it_has() {
        // Field by field rather than by round trip: a round trip is symmetric and
        // would pass with `sepc` and `sstatus` swapped.
        let mut f = [0u8; 256];
        f[8..16].copy_from_slice(&0x11u64.to_ne_bytes()); // ra (x1)
        f[80..88].copy_from_slice(&0xA0u64.to_ne_bytes()); // a0 (x10)
        f[240..248].copy_from_slice(&0x30u64.to_ne_bytes()); // x30 (t5)
        f[0..8].copy_from_slice(&0x5E9Cu64.to_ne_bytes()); // sepc
        f[248..256].copy_from_slice(&0x5220u64.to_ne_bytes()); // sstatus, not x31

        let mc = Mcontext::from_frame(&f);
        assert_eq!(mc.mc_ra, 0x11);
        assert_eq!(mc.mc_a0, 0xA0);
        assert_eq!(mc.mc_t5, 0x30);
        assert_eq!(mc.mc_t6, 0, "p_reg has no x31 slot: t6 lives in Proc::p_t6");
        assert_eq!(mc.mc_sepc, 0x5E9C);
        assert_eq!(mc.mc_sstatus, 0x5220);
        assert_eq!(mc.mc_fpstate, [0u8; 256]);
    }

    #[test]
    fn test_frame_roundtrip() {
        let mut f = [0u8; 256];
        for i in 1..=30u64 {
            let off = (i as usize) * size_of::<u64>();
            f[off..off + 8].copy_from_slice(&(0x1000 + i).to_ne_bytes());
        }
        f[0..8].copy_from_slice(&0xDEADu64.to_ne_bytes());
        f[248..256].copy_from_slice(&0x5220u64.to_ne_bytes());

        let mc = Mcontext::from_frame(&f);
        let mut back = [0u8; 256];
        mc.write_into_frame(&mut back);
        assert_eq!(back, f);
    }

    #[test]
    fn test_write_into_frame_ignores_t6() {
        // `mc_t6` has no slot in `p_reg`, and the slot it *would* map to (248) holds
        // `sstatus`, so a `t6` that leaked into the copy would land on sstatus.
        let mc = Mcontext {
            mc_t6: 0xBAD,
            mc_sstatus: 0x5220,
            ..Mcontext::default()
        };
        let mut f = [0u8; 256];
        mc.write_into_frame(&mut f);
        assert_eq!(
            u64::from_ne_bytes(f[248..256].try_into().unwrap()),
            0x5220,
            "t6 must not overwrite sstatus — p_reg has no slot for it"
        );
        assert_eq!(u64::from_ne_bytes(f[8..16].try_into().unwrap()), mc.mc_ra);
    }
}
