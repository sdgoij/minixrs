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
    # Check if we came from U-mode (sstatus.SPP = 0) or S-mode (SPP = 1).
    # If from U-mode, switch to the kernel stack via sscratch.
    csrr    t0, sstatus
    andi    t0, t0, 0x100          # SPP bit = bit 8, 0=U-mode, 1=S-mode
    bnez    t0, 1f                  # skip stack switch if from S-mode
    csrrw   sp, sscratch, sp        # sp = sscratch (kernel stack), sscratch = user sp
1:
    # Allocate trap frame (32 GPRs + sepc + sstatus + scause + kstack = 296 bytes)
    addi    sp, sp, -296

    # Save all 32 GPRs
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
    csrr    t0, scause
    sd      t0, 272(sp)

    # Save the original SP (before trap allocation).
    # If from U-mode: user sp is in sscratch.
    # If from S-mode: original sp = current sp + 296.
    csrr    t0, sscratch
    sd      t0, 16(sp)

    # Save the kernel stack pointer at offset 280 for restoring sscratch later.
    # For U-mode traps: sp is the kernel stack (after swap).
    # For S-mode traps: sp is the kernel stack (already).
    addi    t0, sp, 296             # t0 = kernel sp BEFORE trap allocation
    sd      t0, 280(sp)

    # Call trap_handler(frame)
    mv      a0, sp
    call    trap_handler

    # Restore CSRs
    ld      t0, 256(sp)
    csrw    sepc, t0
    ld      t0, 264(sp)
    csrw    sstatus, t0

    # Restore kernel stack pointer into sscratch (for next U-mode trap)
    ld      t0, 280(sp)
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
