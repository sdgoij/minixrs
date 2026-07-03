# RISC-V64 trap vector (assembly).
#
# Installed at stvec. Handles all exceptions and interrupts.
# Saves all 32 GPRs + sepc + sstatus + scause, calls the Rust
# trap_handler, then restores and sret.

.section .text.trap_vector, "ax"
.globl trap_vector
.align 2  # Must be 4-byte aligned for stvec (DIRECT mode)

trap_vector:
    # Allocate space on the kernel stack for the trap frame.
    # Layout: 32 GPRs (256 bytes) + sepc (8) + sstatus (8) + scause (8) = 280 bytes
    # We allocate 288 bytes for alignment.
    addi    sp, sp, -288

    # Save all 32 GPRs (x0 is zero, save it anyway for uniformity)
    sd      zero, 0(sp)     # x0 = zero
    sd      ra,   8(sp)     # x1 = ra
    sd      sp,   16(sp)    # x2 = sp (will be corrected before restore)
    sd      gp,   24(sp)    # x3 = gp
    sd      tp,   32(sp)    # x4 = tp
    sd      t0,   40(sp)    # x5 = t0
    sd      t1,   48(sp)    # x6 = t1
    sd      t2,   56(sp)    # x7 = t2
    sd      s0,   64(sp)    # x8 = s0/fp
    sd      s1,   72(sp)    # x9 = s1
    sd      a0,   80(sp)    # x10 = a0
    sd      a1,   88(sp)    # x11 = a1
    sd      a2,   96(sp)    # x12 = a2
    sd      a3,   104(sp)   # x13 = a3
    sd      a4,   112(sp)   # x14 = a4
    sd      a5,   120(sp)   # x15 = a5
    sd      a6,   128(sp)   # x16 = a6
    sd      a7,   136(sp)   # x17 = a7
    sd      s2,   144(sp)   # x18 = s2
    sd      s3,   152(sp)   # x19 = s3
    sd      s4,   160(sp)   # x20 = s4
    sd      s5,   168(sp)   # x21 = s5
    sd      s6,   176(sp)   # x22 = s6
    sd      s7,   184(sp)   # x23 = s7
    sd      s8,   192(sp)   # x24 = s8
    sd      s9,   200(sp)   # x25 = s9
    sd      s10,  208(sp)   # x26 = s10
    sd      s11,  216(sp)   # x27 = s11
    sd      t3,   224(sp)   # x28 = t3
    sd      t4,   232(sp)   # x29 = t4
    sd      t5,   240(sp)   # x30 = t5
    sd      t6,   248(sp)   # x31 = t6

    # Save CSRs
    csrr    t0, sepc
    sd      t0, 256(sp)     # sepc
    csrr    t0, sstatus
    sd      t0, 264(sp)     # sstatus
    csrr    t0, scause
    sd      t0, 272(sp)     # scause

    # Save the original SP (before trap)
    # The original SP = current sp + 288 (the allocation we just did)
    addi    t0, sp, 288
    sd      t0, 16(sp)      # save original sp at offset 16

    # Set up the C ABI frame
    mv      s0, zero        # clear frame pointer for stack trace

    # Call trap_handler(frame_ptr) — frame_ptr in a0
    mv      a0, sp
    call    trap_handler

    # Restore CSRs
    ld      t0, 256(sp)     # sepc
    csrw    sepc, t0
    ld      t0, 264(sp)     # sstatus
    csrw    sstatus, t0

    # Restore all 32 GPRs (except sp — we need it for the frame pointer)
    ld      ra,   8(sp)     # x1 = ra
    ld      gp,   24(sp)    # x3 = gp
    ld      tp,   32(sp)    # x4 = tp
    ld      t0,   40(sp)    # x5 = t0
    ld      t1,   48(sp)    # x6 = t1
    ld      t2,   56(sp)    # x7 = t2
    ld      s0,   64(sp)    # x8 = s0/fp
    ld      s1,   72(sp)    # x9 = s1
    ld      a0,   80(sp)    # x10 = a0
    ld      a1,   88(sp)    # x11 = a1
    ld      a2,   96(sp)    # x12 = a2
    ld      a3,   104(sp)   # x13 = a3
    ld      a4,   112(sp)   # x14 = a4
    ld      a5,   120(sp)   # x15 = a5
    ld      a6,   128(sp)   # x16 = a6
    ld      a7,   136(sp)   # x17 = a7
    ld      s2,   144(sp)   # x18 = s2
    ld      s3,   152(sp)   # x19 = s3
    ld      s4,   160(sp)   # x20 = s4
    ld      s5,   168(sp)   # x21 = s5
    ld      s6,   176(sp)   # x22 = s6
    ld      s7,   184(sp)   # x23 = s7
    ld      s8,   192(sp)   # x24 = s8
    ld      s9,   200(sp)   # x25 = s9
    ld      s10,  208(sp)   # x26 = s10
    ld      s11,  216(sp)   # x27 = s11
    ld      t3,   224(sp)   # x28 = t3
    ld      t4,   232(sp)   # x29 = t4
    ld      t5,   240(sp)   # x30 = t5
    ld      t6,   248(sp)   # x31 = t6

    # Restore sp last
    ld      sp,   16(sp)    # x2 = sp (original sp before trap)

    sret

# Public symbol for the Rust trap handler to reference.
.globl trap_vector_ptr
trap_vector_ptr:
    .dword trap_vector
