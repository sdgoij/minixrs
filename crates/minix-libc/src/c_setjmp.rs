//! setjmp/longjmp — the callee-saved set of each arch, per its C ABI.
//!
//! Written in `naked_asm!` so the registers are captured before the compiler
//! can touch them. The x86_64 form is the SysV set (rbx, rbp, r12-r15, rsp,
//! rip) and its inline asm is Intel-syntax, matching the toolchain's behavior
//! for `x86_64-unknown-none`; the aarch64 and riscv64 forms are the standard
//! GAS syntax their targets use.

#[cfg(target_os = "minix")]
use core::ffi::c_int;
use core::ffi::c_long;

/// C `jmp_buf`. The size is per arch because what has to survive a `longjmp`
/// is the ABI's callee-saved set, and the header in `tools/c-include` declares
/// the same length:
///
///   * x86_64 — 8 longs: rbx, rbp, r12-r15, rsp, rip.
///   * aarch64 — 21 longs: x19-x28, x29 (fp), x30 (lr), sp, d8-d15.
///   * riscv64 — 26 longs: ra, sp, s0-s11, fs0-fs11.
#[cfg(target_arch = "x86_64")]
pub type JmpBuf = [c_long; 8];
#[cfg(target_arch = "aarch64")]
pub type JmpBuf = [c_long; 21];
#[cfg(target_arch = "riscv64")]
pub type JmpBuf = [c_long; 26];

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

#[cfg(all(target_os = "minix", target_arch = "aarch64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn setjmp(env: *mut JmpBuf) -> c_int {
    core::arch::naked_asm!(
        "stp x19, x20, [x0, #0]",
        "stp x21, x22, [x0, #16]",
        "stp x23, x24, [x0, #32]",
        "stp x25, x26, [x0, #48]",
        "stp x27, x28, [x0, #64]",
        "stp x29, x30, [x0, #80]",
        "mov x2, sp",
        "str x2, [x0, #96]",
        "stp d8, d9, [x0, #104]",
        "stp d10, d11, [x0, #120]",
        "stp d12, d13, [x0, #136]",
        "stp d14, d15, [x0, #152]",
        "mov w0, #0",
        "ret",
    )
}

#[cfg(all(target_os = "minix", target_arch = "aarch64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn longjmp(env: *mut JmpBuf, val: c_int) -> ! {
    // The return value is parked in w2 first: x0 holds the buffer pointer for
    // every load below, and it is x0 that has to carry the value back out.
    core::arch::naked_asm!(
        "cmp w1, #0",
        "csinc w2, w1, wzr, ne",
        "ldp x19, x20, [x0, #0]",
        "ldp x21, x22, [x0, #16]",
        "ldp x23, x24, [x0, #32]",
        "ldp x25, x26, [x0, #48]",
        "ldp x27, x28, [x0, #64]",
        "ldp x29, x30, [x0, #80]",
        "ldr x3, [x0, #96]",
        "mov sp, x3",
        "ldp d8, d9, [x0, #104]",
        "ldp d10, d11, [x0, #120]",
        "ldp d12, d13, [x0, #136]",
        "ldp d14, d15, [x0, #152]",
        "mov w0, w2",
        "ret",
    )
}

#[cfg(all(target_os = "minix", target_arch = "riscv64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn setjmp(env: *mut JmpBuf) -> c_int {
    core::arch::naked_asm!(
        "sd ra, 0(a0)",
        "sd sp, 8(a0)",
        "sd s0, 16(a0)",
        "sd s1, 24(a0)",
        "sd s2, 32(a0)",
        "sd s3, 40(a0)",
        "sd s4, 48(a0)",
        "sd s5, 56(a0)",
        "sd s6, 64(a0)",
        "sd s7, 72(a0)",
        "sd s8, 80(a0)",
        "sd s9, 88(a0)",
        "sd s10, 96(a0)",
        "sd s11, 104(a0)",
        "fsd fs0, 112(a0)",
        "fsd fs1, 120(a0)",
        "fsd fs2, 128(a0)",
        "fsd fs3, 136(a0)",
        "fsd fs4, 144(a0)",
        "fsd fs5, 152(a0)",
        "fsd fs6, 160(a0)",
        "fsd fs7, 168(a0)",
        "fsd fs8, 176(a0)",
        "fsd fs9, 184(a0)",
        "fsd fs10, 192(a0)",
        "fsd fs11, 200(a0)",
        "li a0, 0",
        "ret",
    )
}

#[cfg(all(target_os = "minix", target_arch = "riscv64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn longjmp(env: *mut JmpBuf, val: c_int) -> ! {
    // `a0` holds the buffer pointer for every load and then carries the value
    // back out, so the "val of 0 becomes 1" rule is applied to `a1` in place.
    core::arch::naked_asm!(
        "bnez a1, 1f",
        "li a1, 1",
        "1:",
        "ld ra, 0(a0)",
        "ld sp, 8(a0)",
        "ld s0, 16(a0)",
        "ld s1, 24(a0)",
        "ld s2, 32(a0)",
        "ld s3, 40(a0)",
        "ld s4, 48(a0)",
        "ld s5, 56(a0)",
        "ld s6, 64(a0)",
        "ld s7, 72(a0)",
        "ld s8, 80(a0)",
        "ld s9, 88(a0)",
        "ld s10, 96(a0)",
        "ld s11, 104(a0)",
        "fld fs0, 112(a0)",
        "fld fs1, 120(a0)",
        "fld fs2, 128(a0)",
        "fld fs3, 136(a0)",
        "fld fs4, 144(a0)",
        "fld fs5, 152(a0)",
        "fld fs6, 160(a0)",
        "fld fs7, 168(a0)",
        "fld fs8, 176(a0)",
        "fld fs9, 184(a0)",
        "fld fs10, 192(a0)",
        "fld fs11, 200(a0)",
        "mv a0, a1",
        "ret",
    )
}
