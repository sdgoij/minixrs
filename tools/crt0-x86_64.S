/* C runtime entry for Minix x86_64.
 *
 * The kernel's exec loader jumps here with the SysV initial stack:
 *   [rsp]         = argc
 *   [rsp+8 ..]    = argv[] pointers, NULL terminator
 *   ... then envp[] pointers, NULL terminator
 * (same layout the rust fork's `_start` consumes). `main` gets argc/argv per
 * the SysV calling convention and envp as its third argument, and `environ` is
 * published from that argument so `getenv` works from the first constructor
 * onward. `exit` never returns.
 */

    .section .text._start,"ax"
    .globl _start
    .type _start,@function
_start:
    movq    (%rsp), %rdi           # argc
    leaq    8(%rsp), %rsi          # argv
    leaq    8(%rsi,%rdi,8), %rdx   # envp = argv + (argc + 1) * 8
    andq    $-16, %rsp             # 16-byte align for the call (main may use SSE)
    # `minix_libc_tls_init` clobbers the caller-saved arg registers, so park
    # argc/argv/envp in callee-saved registers across the call (the Rust
    # runtime preserves r12-r15) and restore them for `main`.
    movq    %rdi, %r12             # argc
    movq    %rsi, %r13             # argv
    movq    %rdx, %r14             # envp
    call    minix_libc_tls_init    # set up the main thread's TLS block + thread pointer
    movq    %r14, %rdi             # envp
    call    __minix_set_environ    # publish it as C's `environ`
    # Run this image's .init_array constructors, walking the bounds its own
    # linker script defines. The walk is inline rather than a call into the C
    # library because that call would take its bounds from whichever image
    # defines it: in a dynamically linked program `__minix_init_array` resolves
    # into the shared object, whose array is the object's own and empty, and the
    # program's constructors would be skipped without a word.
    leaq    __init_array_start(%rip), %rbx
    leaq    __init_array_end(%rip), %r15
.Lctors:
    cmpq    %r15, %rbx
    jae     .Lctors_done
    movq    %r12, %rdi             # argc
    movq    %r13, %rsi             # argv
    movq    %r14, %rdx             # envp
    call    *(%rbx)
    addq    $8, %rbx
    jmp     .Lctors
.Lctors_done:
    movq    %r12, %rdi
    movq    %r13, %rsi
    movq    %r14, %rdx
    call    main
    movq    %rax, %rdi             # exit(main(...))
    call    exit
1:  jmp     1b                     # exit never returns
    .size _start, .-_start
