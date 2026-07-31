//! AArch64 context switch — switch_to_user function.

/// Switch to user mode for the given process and never return.
///
/// # Safety
///
/// `proc_ptr` must point to a valid `Proc` with correctly set up
/// `p_reg`, `p_seg.p_cr3`, and per-process page table mappings.
#[cfg(target_arch = "aarch64")]
pub unsafe fn switch_to_user(proc_ptr: *const u8) -> ! {
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

            "eret",

            proc = in(reg) proc_ptr,
            options(noreturn),
        );
    }
}
