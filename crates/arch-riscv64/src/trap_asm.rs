//! RISC-V64 trap vector — assembly trap handler via global_asm!.
//!
//! Installed at stvec. Saves/restores all 32 GPRs + CSRs,
//! calls the Rust trap_handler, then sret.

use core::arch::global_asm;

global_asm!(
    r#"
.section .text.trap_vector, "ax"
.globl trap_vector
.align 2

trap_vector:
    # Swap t0 with sscratch: sscratch = the interrupted t0 (preserved for the
    # frame), t0 = the old sscratch value. The old sscratch value itself is
    # irrelevant — this entry no longer swaps SP (see below) — so t0 is free
    # to be clobbered by the SPP check.
    csrrw   t0, sscratch, t0
    # Trap source from sstatus.SPP (the CPU sets it on entry): 0 = U-mode,
    # 1 = S-mode. A U-mode trap runs on the kernel stack (__boot_stack_top).
    # An S-mode fault taken mid-syscall keeps the interrupted kernel SP — the
    # frame goes BELOW it so the syscall's own stack and its U-mode trap frame
    # (at the kernel stack top) stay intact for the eventual syscall-exit
    # sret. The interrupted SP is spilled below the frame in both cases.
    csrr    t0, sstatus
    srli    t0, t0, 8
    andi    t0, t0, 1
    bnez    t0, 1f
    # U-mode: kernel stack = __boot_stack_top (fixed symbol).
    la      t0, __boot_stack_top
    addi    t0, t0, -296            # t0 = frame base (top - 296)
    sd      sp, -24(t0)             # interrupted user sp, below the frame
    mv      sp, t0
    j       2f
1:  # S-mode: keep the interrupted kernel SP; frame 296 bytes below it, with
    # the interrupted SP spilled in the 24 bytes below the frame.
    addi    t0, sp, -320            # t0 = frame base (sp - 296 - 24)
    sd      sp, -24(t0)             # interrupted kernel sp, below the frame
    mv      sp, t0
2:  # sp = frame base; [sp-24, sp) holds the interrupted SP.

    # Save all 32 GPRs (t0's slot is fixed up below; sp is saved from the
    # spill; every other register holds its interrupted value — nothing was
    # clobbered before this point except t0, whose value is in sscratch).
    sd      zero, 0(sp)
    sd      ra,   8(sp)
    sd      gp,   24(sp)
    sd      tp,   32(sp)
    sd      t0,   40(sp)
    sd      t1,   48(sp)
    sd      t2,   56(sp)
    sd      s0,   64(sp)
    sd      s1,   72(sp)
    sd      a0,   80(sp)
    sd      a1,   88(sp)
    sd      a2,   96(sp)
    sd      a3,   104(sp)
    sd      a4,   112(sp)
    sd      a5,   120(sp)
    sd      a6,   128(sp)
    sd      a7,   136(sp)
    sd      s2,   144(sp)
    sd      s3,   152(sp)
    sd      s4,   160(sp)
    sd      s5,   168(sp)
    sd      s6,   176(sp)
    sd      s7,   184(sp)
    sd      s8,   192(sp)
    sd      s9,   200(sp)
    sd      s10,  208(sp)
    sd      s11,  216(sp)
    sd      t3,   224(sp)
    sd      t4,   232(sp)
    sd      t5,   240(sp)
    sd      t6,   248(sp)

    # Save CSRs
    csrr    t0, sepc
    sd      t0, 256(sp)
    csrr    t0, sstatus
    sd      t0, 264(sp)
    # Set SUM=1 in sstatus so S-mode can access user pages (for IPC copy).
    # The original sstatus with SUM=0 is saved at offset 264 and restored on return.
    li      t0, 0x40000              # SUM bit = bit 18
    csrs    sstatus, t0
    csrr    t0, scause
    sd      t0, 272(sp)

    # Fix-ups: the interrupted SP (from the spill) into slot 16 (and 280 for
    # debug), and the interrupted t0 (from sscratch) into slot 40.
    ld      t4, -24(sp)
    sd      t4, 16(sp)
    sd      t4, 280(sp)
    csrr    t4, sscratch
    sd      t4, 40(sp)

    # Re-enable SIE for user-mode traps (SPP=0) so interrupt-driven UART RX
    # keeps draining while the kernel processes the trap; a bursty push
    # otherwise overruns the 16550 FIFO during long kernel windows. S-mode
    # traps (a fault taken mid-syscall) stay SIE=0: kernel processing must
    # not nest. The saved sstatus at 264 has SIE=0 and is restored on
    # return, so SIE is cleared again before sret.
    csrr    t0, sstatus
    srli    t0, t0, 8
    andi    t0, t0, 1
    bnez    t0, 3f
    csrs    sstatus, 2                # SIE bit = bit 1
3:
    # Call trap_handler(frame)
    mv      a0, sp
    call    trap_handler

    # Mask SIE before the exit path: a U-mode trap handler ran with SIE=1
    # (the whole point of Phase E), but the sepc/sstatus/GPR restore below
    # must not be interrupted — a nested trap here re-fetches the restore
    # code, and if a context switch just rewrote the frame the fault lands
    # on a wrong page-table context (a kernel-address fault the VM treats
    # as user-range and blocks the process on). The saved sstatus at 264
    # (SIE=0) is restored below regardless.
    csrci   sstatus, 2

    # Restore CSRs
    ld      t0, 256(sp)
    csrw    sepc, t0
    ld      t0, 264(sp)
    csrw    sstatus, t0

    # Re-arm sscratch for the next trap: the U-mode kernel stack. (S-mode
    # resumes don't swap SP — their frames go below the interrupted SP — so
    # sscratch's value is only used as the interrupted-t0 spill slot.)
    la      t0, __boot_stack_top
    csrw    sscratch, t0

    # Restore GPRs (except sp)
    ld      ra,   8(sp)
    ld      gp,   24(sp)
    ld      tp,   32(sp)
    ld      t0,   40(sp)
    ld      t1,   48(sp)
    ld      t2,   56(sp)
    ld      s0,   64(sp)
    ld      s1,   72(sp)
    ld      a0,   80(sp)
    ld      a1,   88(sp)
    ld      a2,   96(sp)
    ld      a3,   104(sp)
    ld      a4,   112(sp)
    ld      a5,   120(sp)
    ld      a6,   128(sp)
    ld      a7,   136(sp)
    ld      s2,   144(sp)
    ld      s3,   152(sp)
    ld      s4,   160(sp)
    ld      s5,   168(sp)
    ld      s6,   176(sp)
    ld      s7,   184(sp)
    ld      s8,   192(sp)
    ld      s9,   200(sp)
    ld      s10,  208(sp)
    ld      s11,  216(sp)
    ld      t3,   224(sp)
    ld      t4,   232(sp)
    ld      t5,   240(sp)
    ld      t6,   248(sp)

    # Restore sp from the saved frame (user sp for U-mode, kernel sp for S-mode).
    ld      sp,   16(sp)

    sret
"#
);

/// Get the address of the trap vector for stvec.
pub fn trap_vector_addr() -> u64 {
    unsafe extern "C" {
        static trap_vector: u8;
    }
    core::ptr::addr_of!(trap_vector) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trap_vector_addr_not_null() {
        // The trap vector must be at a non-zero, aligned address.
        let addr = trap_vector_addr();
        assert!(addr != 0);
        assert_eq!(addr & 0x3, 0); // Must be 4-byte aligned for DIRECT stvec
    }
}
