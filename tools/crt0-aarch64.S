/* C runtime entry for Minix aarch64.
 *
 * The kernel's exec loader jumps here with the initial stack:
 *   [sp]          = argc
 *   [sp+8 ..]     = argv[] pointers, NULL terminator
 *   ... then envp[] pointers, NULL terminator
 * (same layout the rust fork's `_start` consumes). `main` gets argc/argv per
 * the AArch64 calling convention (x0/x1) and envp as its third argument (x2),
 * and `environ` is published from that argument so `getenv` works from the
 * first constructor onward. `exit` never returns.
 */

    .section .text._start,"ax"
    .globl _start
    .type _start,@function
_start:
    ldr     x0, [sp]               // argc
    add     x1, sp, #8             // argv
    add     x2, x1, x0, lsl #3     // envp = argv + (argc + 1) * 8
    add     x2, x2, #8
    // 16-byte align for the calls (main may use NEON). Through x16: the logical
    // immediates have no SP form, so SP is only ever written by a `mov`.
    mov     x16, sp
    and     x16, x16, #-16
    mov     sp, x16
    // `minix_libc_tls_init` clobbers the caller-saved arg registers, so park
    // argc/argv/envp in callee-saved registers across the call (the Rust
    // runtime preserves x19-x28) and restore them for `main`.
    mov     x19, x0                // argc
    mov     x20, x1                // argv
    mov     x21, x2                // envp
    bl      minix_libc_tls_init    // set up the main thread's TLS block + thread pointer
    mov     x0, x21                // envp
    bl      __minix_set_environ    // publish it as C's `environ`
    mov     x0, x19
    mov     x1, x20
    mov     x2, x21
    bl      __minix_init_array     // run ELF .init_array constructors
    mov     x0, x19
    mov     x1, x20
    mov     x2, x21
    bl      main
    bl      exit                   // exit(main(...))
1:  b       1b                     // exit never returns
    .size _start, .-_start
