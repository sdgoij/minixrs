//! RISC-V64 context switch — switch_to_user function.
//!
//! Restores all registers from a process's trap frame and sret to userspace.
//! Equivalent to x86_64's `restore()` in arch-x86_64/src/asm.rs.

/// Switch to a user process by loading its trap frame from `p_reg`.
///
/// Equivalent to x86_64's `restore()` in arch-x86_64/src/asm.rs.
/// Takes a pointer to the `Proc` struct; reads sepc from offset 0
/// (x0 slot, set by `hal::set_initial_regs`) and sstatus from offset 248
/// (t6 slot). Also loads the per-process page table from p_seg.p_cr3
/// (offset 256) and writes SATP with SV39 mode before sret.
///
/// # Safety
///
/// `proc_ptr` must point to a valid `Proc` whose `p_reg` and `p_seg`
/// contain valid user-space register values. Must be called in S-mode
/// with interrupts disabled. Never returns.
#[cfg(target_arch = "riscv64")]
pub unsafe fn switch_to_user(proc_ptr: *const u8) -> ! {
    unsafe {
        core::arch::asm!(
        // Save proc_ptr in t6 (t6 is skipped in GPR loads below, so it persists).
        "mv      t6, a0",

        // Read sepc from offset 0 (x0 slot) and set it.
        "ld      t0, 0(t6)",
        "csrw    sepc, t0",

        // Set SATP from p_seg.p_cr3 at offset 256.
        // SATP = (8 << 60) | (cr3 >> 12)  [SV39 mode]
        "ld      t0, 256(t6)",
        "srli    t0, t0, 12",        // PPN = cr3 >> 12
        "li      t1, 8",
        "slli    t1, t1, 60",        // MODE = 8 (SV39)
        "or      t0, t0, t1",
        "csrw    satp, t0",
        "sfence.vma",

        // Save kernel sp to sscratch (before any U-mode trap can happen).
        "mv      t0, sp",
        "csrw    sscratch, t0",

        // Load all GPRs except x0 (offset 0, holds sepc), t6 (offset 248, holds sstatus),
        // and sp/a0 (loaded at the end).
        "ld      ra,   8(t6)",
        "ld      gp,   24(t6)",
            "ld      tp,   32(t6)",
            "ld      t0,   40(t6)",
            "ld      t1,   48(t6)",
            "ld      t2,   56(t6)",
            "ld      s0,   64(t6)",
            "ld      s1,   72(t6)",
            "ld      a1,   88(t6)",
            "ld      a2,   96(t6)",
            "ld      a3,   104(t6)",
            "ld      a4,   112(t6)",
            "ld      a5,   120(t6)",
            "ld      a6,   128(t6)",
            "ld      a7,   136(t6)",
            "ld      s2,   144(t6)",
            "ld      s3,   152(t6)",
            "ld      s4,   160(t6)",
            "ld      s5,   168(t6)",
            "ld      s6,   176(t6)",
            "ld      s7,   184(t6)",
            "ld      s8,   192(t6)",
            "ld      s9,   200(t6)",
            "ld      s10,  208(t6)",
            "ld      s11,  216(t6)",
            "ld      t3,   224(t6)",
            "ld      t4,   232(t6)",
            "ld      t5,   240(t6)",
            // t6 (offset 248) NOT loaded — holds proc_ptr.

            // Load sp (user stack) and a0 last.
            "ld      sp,   16(t6)",
            "ld      a0,   80(t6)",

            // Set sstatus (SPP=0 → U-mode) RIGHT BEFORE sret.
            // sret is atomic WRT interrupts, so no timer can fire between csrw and sret.
            "ld      t0,   248(t6)",
            "csrw    sstatus, t0",
            "sret",

            in("a0") proc_ptr,
            options(noreturn),
        );
    }
}
