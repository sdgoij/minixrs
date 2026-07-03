//! RISC-V64 context switch — switch_to_user function.
//!
//! Restores all registers from a process's trap frame and sret to userspace.
//! Equivalent to x86_64's `restore()` in arch-x86_64/src/asm.rs.

/// Switch to a user process by loading its trap frame.
///
/// Loads sepc, sstatus, and all 32 GPRs from `frame`, then executes
/// `sret` to jump to userspace.
///
/// Trap frame layout (byte offsets):
///   0: zero,   8: ra,   16: sp,  24: gp,  32: tp
///  40: t0,    48: t1,   56: t2,  64: s0,  72: s1
///  80: a0,    88: a1,   96: a2, 104: a3, 112: a4
/// 120: a5,   128: a6,  136: a7, 144: s2, 152: s3
/// 160: s4,   168: s5,  176: s6, 184: s7, 192: s8
/// 200: s9,   208: s10, 216: s11, 224: t3, 232: t4
/// 240: t5,   248: t6
/// 256: sepc, 264: sstatus
///
/// # Safety
///
/// `frame` must point to a valid 288-byte trap frame with proper
/// register state. This function never returns.
#[cfg(target_arch = "riscv64")]
pub unsafe fn switch_to_user(frame: &[u8; 288]) -> ! {
    let sepc_val = u64::from_ne_bytes(frame[256..264].try_into().unwrap());
    let sstatus_val = u64::from_ne_bytes(frame[264..272].try_into().unwrap());

    unsafe {
        core::arch::asm!(
            "csrw sstatus, {sstatus}",
            "csrw sepc, {sepc}",

            // Load all GPRs using a0 as frame pointer (set by in("a0"))
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
            "ld      t6,   248(a0)",

            // Load sp and a0 last (a0 is the frame pointer)
            "ld      sp,   16(a0)",
            "ld      a0,   80(a0)",

            "sret",

            sepc = in(reg) sepc_val,
            sstatus = in(reg) sstatus_val,
            in("a0") frame.as_ptr(),
            options(noreturn),
        );
    }
}
