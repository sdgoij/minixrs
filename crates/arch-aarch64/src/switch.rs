//! AArch64 context switch — switch_to_user function.

use core::sync::atomic::{AtomicU64, Ordering};

/// Byte offset of `Proc::p_tls` within the `Proc` struct, registered by the
/// kernel at boot (0 = TLS support disabled). `switch_to_user` reads it to
/// load the new thread's tpidr_el0 (thread pointer) on every switch.
static TLS_TPIDR_OFF: AtomicU64 = AtomicU64::new(0);

/// Register the byte offset of `Proc::p_tls` (set once during boot, before
/// any thread can run).
///
/// # Safety
///
/// Must be called exactly once during boot.
pub unsafe fn set_tls_tpidr_offset(off: u64) {
    TLS_TPIDR_OFF.store(off, Ordering::Relaxed);
}

/// Switch to user mode for the given process and never return.
///
/// # Safety
///
/// `proc_ptr` must point to a valid `Proc` with correctly set up
/// `p_reg`, `p_seg.p_cr3`, and per-process page table mappings.
#[cfg(target_arch = "aarch64")]
pub unsafe fn switch_to_user(proc_ptr: *const u8) -> ! {
    let tls_off = TLS_TPIDR_OFF.load(Ordering::Relaxed);
    unsafe {
        core::arch::asm!(
            "mov     x20, {proc}",

            "ldr     x9, [x20, #288]",
            // TLBI before TTBR0 switch, with I-cache invalidation.
            "dsb     ish",
            "tlbi    vmalle1is",
            "dsb     ish",
            "ic      ialluis",
            "dsb     ish",
            "isb",
            "msr     ttbr0_el1, x9",
            "isb",

            // Our trap frame layout:
            //   ELR_EL1 at offset 256, SPSR_EL1 at 264, SP_EL0 at 248.
            "ldr     x16, [x20, #256]",   // ELR_EL1
            "ldr     x17, [x20, #264]",   // SPSR_EL1
            "ldr     x18, [x20, #248]",   // SP_EL0
            "msr     elr_el1, x16",
            "msr     spsr_el1, x17",
            "msr     sp_el0, x18",

            // Load GPRs from p_reg (offsets 0-240).
            "ldp     x0,  x1,  [x20, #0x00]",
            "ldp     x2,  x3,  [x20, #0x10]",
            "ldp     x4,  x5,  [x20, #0x20]",
            "ldp     x6,  x7,  [x20, #0x30]",
            "ldp     x8,  x9,  [x20, #0x40]",
            "ldp     x10, x11, [x20, #0x50]",
            "ldp     x12, x13, [x20, #0x60]",
            "ldp     x14, x15, [x20, #0x70]",
            "ldp     x16, x17, [x20, #0x80]",
            "ldp     x18, x19, [x20, #0x90]",
            "ldp     x22, x23, [x20, #0xB0]",
            "ldp     x24, x25, [x20, #0xC0]",
            "ldp     x26, x27, [x20, #0xD0]",
            "ldp     x28, x29, [x20, #0xE0]",
            "ldr     x30,      [x20, #0xF0]",
            "ldp     x20, x21, [x20, #0xA0]",

            // Load the new thread's tpidr_el0 (TLS thread pointer) from
            // Proc.p_tls at the registered offset. x21 is clobbered here and
            // reloaded from the frame above; the offset register is the
            // caller-provided operand.
            "ldr     x21, [x20, {tls_off}]",
            "cbz     x21, 1f",
            "msr     tpidr_el0, x21",
            "1:",

            "eret",

            proc = in(reg) proc_ptr,
            tls_off = in(reg) tls_off,
            options(noreturn),
        );
    }
}
