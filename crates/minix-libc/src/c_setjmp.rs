//! setjmp/longjmp — minimal SysV x86_64 (rbx, rbp, r12-r15, rsp, rip).
//!
//! Written in `naked_asm!` so the callee-saved registers are captured
//! before the compiler can touch them. The minix target's inline asm is
//! Intel-syntax (no `%` prefixes), matching the toolchain's behavior for
//! `x86_64-unknown-none`.

#[cfg(target_os = "minix")]
use core::ffi::c_int;
use core::ffi::c_long;

/// C `jmp_buf` — 8 longs (rbx, rbp, r12, r13, r14, r15, rsp, rip).
pub type JmpBuf = [c_long; 8];

#[cfg(all(target_os = "minix", target_arch = "x86_64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn setjmp(env: *mut JmpBuf) -> c_int {
    core::arch::naked_asm!(
        "mov [rdi], rbx",
        "mov [rdi+8], rbp",
        "mov [rdi+16], r12",
        "mov [rdi+24], r13",
        "mov [rdi+32], r14",
        "mov [rdi+40], r15",
        "mov [rdi+48], rsp",
        "mov rax, [rsp]",
        "mov [rdi+56], rax",
        "xor eax, eax",
        "ret",
    )
}

#[cfg(all(target_os = "minix", target_arch = "x86_64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn longjmp(env: *mut JmpBuf, val: c_int) -> ! {
    core::arch::naked_asm!(
        "mov eax, esi",
        "test eax, eax",
        "jnz 1f",
        "inc eax",
        "1:",
        "mov rbx, [rdi]",
        "mov rbp, [rdi+8]",
        "mov r12, [rdi+16]",
        "mov r13, [rdi+24]",
        "mov r14, [rdi+32]",
        "mov r15, [rdi+40]",
        "mov rcx, [rdi+56]",
        "mov rsp, [rdi+48]",
        "push rcx",
        "ret",
    )
}
