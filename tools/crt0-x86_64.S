/* C runtime entry for Minix x86_64.
 *
 * The kernel's exec loader jumps here with the SysV initial stack:
 *   [rsp]         = argc
 *   [rsp+8 ..]    = argv[] pointers, NULL terminator
 *   ... then envp[] pointers, NULL terminator
 * (same layout the rust fork's `_start` consumes). `main` gets argc/argv
 * per the SysV calling convention; `exit` never returns.
 */

    .section .text._start,"ax"
    .globl _start
    .type _start,@function
_start:
    movq    (%rsp), %rdi           # argc
    leaq    8(%rsp), %rsi          # argv
    leaq    8(%rsi,%rdi,8), %rdx   # envp = argv + (argc + 1) * 8
    andq    $-16, %rsp             # 16-byte align for the call (main may use SSE)
    call    main
    movq    %rax, %rdi             # exit(main(...))
    call    exit
1:  jmp     1b                     # exit never returns
    .size _start, .-_start
