/* C runtime entry for Minix riscv64.
 *
 * The kernel's exec loader jumps here with the initial stack:
 *   [sp]          = argc
 *   [sp+8 ..]     = argv[] pointers, NULL terminator
 *   ... then envp[] pointers, NULL terminator
 * (same layout the rust fork's `_start` consumes). `main` gets argc/argv per
 * the RISC-V calling convention (a0/a1) and envp as its third argument (a2),
 * and `environ` is published from that argument so `getenv` works from the
 * first constructor onward. `exit` never returns.
 */

    .section .text._start,"ax"
    .globl _start
    .type _start,@function
_start:
    ld      a0, 0(sp)              // argc
    addi    a1, sp, 8              // argv
    slli    t0, a0, 3
    add     a2, a1, t0
    addi    a2, a2, 8              // envp = argv + (argc + 1) * 8
    andi    sp, sp, -16            // 16-byte align for the calls (main may use FP)
    // `minix_libc_tls_init` clobbers the caller-saved arg registers, so park
    // argc/argv/envp in callee-saved registers across the call (the Rust
    // runtime preserves s0-s11) and restore them for `main`.
    mv      s1, a0                 // argc
    mv      s2, a1                 // argv
    mv      s3, a2                 // envp
    call    minix_libc_tls_init    // set up the main thread's TLS block + thread pointer
    mv      a0, s3                 // envp
    call    __minix_set_environ    // publish it as C's `environ`
    // Run this image's .init_array constructors, walking the bounds its own
    // linker script defines. The walk is inline rather than a call into the C
    // library because that call would take its bounds from whichever image
    // defines it: in a dynamically linked program `__minix_init_array` resolves
    // into the shared object, whose array is the object's own and empty, and the
    // program's constructors would be skipped without a word.
    la      s4, __init_array_start
    la      s5, __init_array_end
.Lctors:
    bgeu    s4, s5, .Lctors_done
    mv      a0, s1                 // argc
    mv      a1, s2                 // argv
    mv      a2, s3                 // envp
    ld      t0, 0(s4)
    jalr    t0
    addi    s4, s4, 8
    j       .Lctors
.Lctors_done:
    mv      a0, s1
    mv      a1, s2
    mv      a2, s3
    call    main
    call    exit                   // exit(main(...))
1:  j       1b                     // exit never returns
    .size _start, .-_start
