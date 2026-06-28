//! x86_64 architecture-specific process initialization.
//!
//! Sets up the TrapFrame for a new process so that the first return to
//! userspace via `sysretq` loads the correct RIP, RFLAGS, and RSP.
//!
//! On x86_64, `sysretq`:
//! - Loads RIP from RCX
//! - Loads RFLAGS from R11
//! - Uses the saved RSP for the stack pointer
//!
//! The kernel Proc struct has `p_reg: TrapFrame` as its first field,
//! so callers pass `&mut proc.p_reg` to this function.  This avoids
//! a circular dependency (kernel depends on arch-x86_64, so the arch
//! crate cannot import kernel types).

use crate::frame::TrapFrame;

/// Set up the architecture-specific parts of a new process.
///
/// Configures the `TrapFrame` so that the process starts executing at
/// `entry` with stack pointer `stack` and a default set of RFLAGS
/// (interrupts enabled, all other user-settable bits cleared).
///
/// # Arguments
///
/// * `tf` - Pointer to the `TrapFrame` inside the kernel `Proc`
///   (`p_reg` field). Must be valid and properly aligned.
/// * `entry` - Virtual address of the first instruction to execute
///   in userspace (loaded into RCX, used as RIP by `sysretq`).
/// * `stack`   - Initial user-space stack pointer.
/// * `_name`   - Process name bytes (reserved for future use).
/// * `_ps_str` - Process string identifier (reserved for future use).
///
/// # Safety
///
/// `tf` must point to a valid, properly aligned `TrapFrame` for a
/// process that is being initialized.  Must be called before the
/// process is first scheduled.
pub unsafe fn arch_proc_init(
    tf: *mut TrapFrame,
    entry: u64,
    stack: u64,
    _name: &[u8],
    _ps_str: u64,
) {
    // On sysretq-based return to userspace:
    //   RCX → RIP   (sysretq loads the return address from RCX)
    //   R11 → RFLAGS (sysretq restores RFLAGS from R11)
    //
    // RSP is loaded from the saved RSP field.
    //
    // PSL_USERSET = MBO (bit 1) | I (bit 9, interrupt enable)
    // This gives us a clean RFLAGS with only interrupts enabled.
    // SAFETY: Caller guarantees `tf` is valid and properly aligned.
    unsafe {
        (*tf).rcx = entry;
    }
    unsafe {
        (*tf).r11 = crate::psl::PSL_USERSET;
    }
    unsafe {
        (*tf).rsp = stack;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psl::rflags;

    #[test]
    fn test_arch_proc_init_sets_regs() {
        let mut tf = TrapFrame::default();
        let entry: u64 = 0x400000;
        let stack: u64 = 0x7FFFF000;

        unsafe {
            arch_proc_init(&mut tf, entry, stack, b"test_proc", 0);
        }

        // RCX should hold the entry point (loaded as RIP by sysretq).
        assert_eq!(tf.rcx, entry);

        // R11 should hold PSL_USERSET (loaded as RFLAGS by sysretq).
        // PSL_USERSET = MBO (bit 1) | I (bit 9, interrupt enable).
        assert_eq!(tf.r11, crate::psl::PSL_USERSET);
        assert_ne!(tf.r11 & rflags::I, 0, "IF must be set");
        assert_ne!(tf.r11 & rflags::MBO, 0, "MBO must be set");

        // RSP should hold the stack pointer.
        assert_eq!(tf.rsp, stack);
    }

    #[test]
    fn test_arch_proc_init_clears_trap() {
        // Verify that the trap flag (single-step) is not set in RFLAGS.
        let mut tf = TrapFrame::default();
        unsafe {
            arch_proc_init(&mut tf, 0x400000, 0x7FFFF000, b"test", 0);
        }
        assert_eq!(tf.r11 & rflags::T, 0, "TF must not be set");
        assert_eq!(tf.r11 & rflags::AC, 0, "AC must not be set");
    }

    #[test]
    fn test_psl_userset_value() {
        // PSL_USERSET = MBO (0x2) | I (0x200) = 0x202.
        assert_eq!(crate::psl::PSL_USERSET, 0x202);
    }

    #[test]
    fn test_arch_proc_init_zero_entry() {
        // Entry point 0 is valid (process starts at address 0).
        let mut tf = TrapFrame::default();
        unsafe {
            arch_proc_init(&mut tf, 0, 0x7FFFF000, b"null_entry", 0);
        }
        assert_eq!(tf.rcx, 0);
        assert_eq!(tf.rsp, 0x7FFFF000);
    }
}
