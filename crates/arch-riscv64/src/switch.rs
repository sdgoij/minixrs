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
/// Trap frame layout within `p_reg` (`[u8; 256]`):
///   0: sepc   (x0 slot, 8 bytes, never loaded as GPR)
///   8: ra     (x1), 16: sp (x2), 24: gp (x3), 32: tp (x4)
///  40: t0,    48: t1,  56: t2,  64: s0,  72: s1
///  80: a0,    88: a1,  96: a2, 104: a3, 112: a4
/// 120: a5,   128: a6, 136: a7, 144: s2, 152: s3
/// 160: s4,   168: s5, 176: s6, 184: s7, 192: s8
/// 200: s9,   208: s10, 216: s11, 224: t3, 232: t4
/// 240: t5,   248: sstatus (t6 slot, skipped in GPR loads)
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
            // Read sepc from offset 0 (x0 slot).
            "ld      t0, 0(a0)",
            "csrw    sepc, t0",

            // Read sstatus from offset 248 (t6 slot).
            "ld      t0, 248(a0)",
            "csrw    sstatus, t0",

            // Set SATP from p_seg.p_cr3 at offset 256.
            // SATP = (8 << 60) | (cr3 >> 12)  [SV39 mode]
            "ld      t0, 256(a0)",
            "srli    t0, t0, 12",        // PPN = cr3 >> 12
            "li      t1, 8",
            "slli    t1, t1, 60",        // MODE = 8 (SV39)
            "or      t0, t0, t1",
            "csrw    satp, t0",
            "sfence.vma",

            // Load all GPRs except x0 (offset 0, holds sepc) and t6 (offset 248, holds sstatus).
            "ld      ra,   8(a0)",
            "ld      gp,   24(a0)",
            "ld      tp,   32(a0)",
            "ld      t0,   40(a0)",
            "ld      t1,   48(a0)",
            "ld      t2,   56(a0)",
            "ld      s0,   64(a0)",
            "ld      s1,   72(a0)",
            "ld      a1,   88(a0)",
            "ld      a2,   96(a0)",
            "ld      a3,   104(a0)",
            "ld      a4,   112(a0)",
            "ld      a5,   120(a0)",
            "ld      a6,   128(a0)",
            "ld      a7,   136(a0)",
            "ld      s2,   144(a0)",
            "ld      s3,   152(a0)",
            "ld      s4,   160(a0)",
            "ld      s5,   168(a0)",
            "ld      s6,   176(a0)",
            "ld      s7,   184(a0)",
            "ld      s8,   192(a0)",
            "ld      s9,   200(a0)",
            "ld      s10,  208(a0)",
            "ld      s11,  216(a0)",
            "ld      t3,   224(a0)",
            "ld      t4,   232(a0)",
            "ld      t5,   240(a0)",
            // Skip t6 (offset 248) — holds sstatus.

            // Save current kernel stack pointer in sscratch for the trap handler.
            // The trap handler swaps sp ↔ sscratch on U-mode entry.
            "mv      t0, sp",
            "csrw    sscratch, t0",

            // Load sp (user stack) and a0 last.
            "ld      sp,   16(a0)",
            "ld      a0,   80(a0)",

            "sret",

            in("a0") proc_ptr,
            options(noreturn),
        );
    }
}
