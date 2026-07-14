//! x86_64 syscall entry/exit configuration using `syscall`/`sysret`.
//!
//! On x86_64, the `syscall` instruction uses three MSRs:
//! - **STAR** (`0xC0000081`): Selects the CS/SS for kernel entry (SYSCALL)
//!   and the CS/SS for return to user (SYSRET).
//! - **LSTAR** (`0xC0000082`): The target RIP for `syscall`.
//! - **SF_MASK** (`0xC0000084`): Masks RFLAGS bits on `syscall` entry.
//!
//! Unlike i386's `int 0x80` path, `syscall` does not save RSP or RFLAGS
//! to the stack — the kernel must save them explicitly. RCX holds the
//! return RIP on entry (clobbered by `syscall`), and R11 holds the
//! original RFLAGS.

use crate::asm;
use crate::cpu_msr;

/// Kernel code segment selector (GDT index 1, RPL=0).
pub const SYSCALL_CS: u16 = 0x0008;

/// User code segment selector base for SYSRETQ.
/// SYSRETQ computes CS = (star[47:32] + 16) | 3, SS = (star[47:32] + 8) | 3.
///
/// QEMU's SYSRETQ does NOT implement the `| 3` for SS (observed: leaves SS
/// at STAR[47:32] + 8 = 0x0010 with RPL=0).  This causes the timer ISR's
/// iretq to #GP because SS.RPL (0) != CPL (3).
///
/// To work around this, set star[47:32] = 0x000B so the computed values
/// already have RPL=3 built-in even without the `| 3`:
///   CS = 0x000B + 16 = 0x001B (index 3, user code, RPL 3)
///   SS = 0x000B +  8 = 0x0013 (index 2, user data, RPL 3)
pub const SYSRET_CS: u16 = 0x000B;

/// Set up the syscall MSRs for `syscall`/`sysret`.
///
/// Configures STAR, LSTAR, and SF_MASK so that the `syscall` instruction
/// transitions to kernel mode at `entry_rip`.
///
/// # Arguments
///
/// * `entry_rip` - The kernel virtual address that `syscall` will jump to.
///
/// # Safety
///
/// Must be called once during boot, on the BSP, in ring 0. Affects all CPUs.
pub unsafe fn setup_syscall_msrs(entry_rip: u64) {
    // STAR layout (x86_64):
    //   Bits 63:48 = SYSCALL CS/SS selector (e.g. 0x0008 for kernel)
    //   Bits 47:32 = SYSRET CS/SS selector  (e.g. 0x0008 for user CS=0x001B, SS=0x0013)
    //   Bits 31:0  = SYSCALL EIP (unused on x86_64)
    //
    // The SS selector for both SYSCALL and SYSRET is always the data
    // segment selector corresponding to the code segment (CS + 8).
    let star_val = cpu_msr::make_star(SYSCALL_CS, SYSRET_CS);
    // SAFETY: Caller guarantees ring 0, single invocation on BSP.
    unsafe {
        asm::wrmsr(cpu_msr::msr::STAR, star_val);
    }

    // LSTAR = kernel entry point (where SYSCALL jumps to).
    // SAFETY: Same as above.
    unsafe {
        asm::wrmsr(cpu_msr::msr::LSTAR, entry_rip);
    }

    // SF_MASK = RFLAGS mask applied on syscall entry.
    // SAFETY: Same as above.
    unsafe {
        asm::wrmsr(cpu_msr::msr::SF_MASK, 0xFF_FFFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_cs_value() {
        // Kernel CS: GDT index 1, RPL 0 => 0x0008.
        assert_eq!(SYSCALL_CS, 0x0008);
    }

    #[test]
    fn test_sysret_cs_value() {
        // SYSRET_CS is the base for the SYSRETQ CS/SS computation.
        // SYSRETQ computes: CS = STAR[47:32] + 16, SS = STAR[47:32] + 8.
        // The standard formula adds | 3 for RPL, but QEMU's SYSRETQ skips
        // the OR for SS (leaving SS.RPL=0).  We set SYSRET_CS = 0x000B so
        // CS = 0x001B (idx 3, user code, RPL 3) and SS = 0x0013 (idx 2,
        // user data, RPL 3) — the RPL bits are baked into the selector.
        assert_eq!(SYSRET_CS, 0x000B);
    }

    #[test]
    fn test_star_selector_roundtrip() {
        // Verify that make_star produces values extractable by the
        // cpu_msr helper functions.
        let star = cpu_msr::make_star(SYSCALL_CS, SYSRET_CS);
        assert_eq!(cpu_msr::star_syscall_sel(star), SYSCALL_CS);
        assert_eq!(cpu_msr::star_sysret_sel(star), SYSRET_CS);
    }

    #[test]
    fn test_sf_mask_bits() {
        // SF_MASK should clear IF (0x200) and AC (0x40000).
        // 0xFF_FFFF covers at minimum these user-settable flags.
        const SF_MASK: u64 = 0xFF_FFFF;
        assert_ne!(SF_MASK & 0x200, 0, "IF must be masked");
        assert_ne!(SF_MASK & 0x40000, 0, "AC must be masked");
        assert_ne!(SF_MASK & 0x400, 0, "DF must be masked");
    }

    #[test]
    fn test_msr_constants() {
        assert_eq!(cpu_msr::msr::STAR, 0xC000_0081);
        assert_eq!(cpu_msr::msr::LSTAR, 0xC000_0082);
        assert_eq!(cpu_msr::msr::SF_MASK, 0xC000_0084);
    }
}
