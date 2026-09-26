//! The loader binary, installed as `/libexec/ld.so`.
//!
//! Its whole job is to hand the kernel-provided main-program entry information
//! to [`ldso::rtld`] and then jump to the main program's entry point.
#![no_std]
#![no_main]

use core::panic::PanicInfo;

/// A panic in the loader is a load failure: report it and stop, rather than
/// start a half-relocated program.
#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    minix_rt::exit(1)
}

/// Entered by the kernel at the loader's image base.
///
/// The register carrying the main program's ELF header page is the one this
/// machine's `exec_init_regs` writes the loader's second argument into: `r9` on
/// x86_64, `a2` on riscv64, `x3` on aarch64. `argc`/`argv`/`envp` are read from
/// the exec'd stack — the layout `tools/crt0-<machine>.S` consumes and the fork's
/// own `_start` does — and the initialisers are called with them, per the ABI.
///
/// The initial `sp` is saved across the linking call and restored before the
/// jump: the main program's entry reads `argc`/`argv` from the stack it was
/// exec'd with, and a call frame of ours sitting on top of it would be read as
/// `argc`.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov    rdi, [rsp]",             // argc
        "lea    rsi, [rsp + 8]",         // argv
        "mov    rax, rdi",               // argc again, to reach past argv
        "lea    rdx, [rsi + rax*8 + 8]", // envp = argv + (argc + 1) * 8
        "mov    rcx, r9",                // main program's ELF header page
        "mov    r12, rsp",               // the exec'd stack, to restore before the jump
        "and    rsp, -16",               // SysV alignment for the call
        "call   {link}",
        "mov    rsp, r12",
        "jmp    rax",                    // into the main program's entry
        link = sym ldso_link,
    )
}

/// The riscv64 entry. `a2` is the header page
/// (`crates/arch-riscv64/src/hal.rs::exec_init_regs`), and the stack holds
/// `argc`/`argv`/`envp` exactly as `tools/crt0-riscv64.S` reads them.
#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mv     s1, a2",       // main program's ELF header page (before a2 is reused)
        "mv     s2, sp",       // the exec'd stack, to restore before the jump
        "ld     a0, 0(sp)",    // argc
        "addi   a1, sp, 8",    // argv
        "slli   t0, a0, 3",
        "add    a2, a1, t0",
        "addi   a2, a2, 8",    // envp = argv + (argc + 1) * 8
        "andi   sp, sp, -16",  // 16-byte alignment for the call
        "mv     a3, s1",       // main_hdr is `ldso_link`'s fourth argument
        "call   {link}",
        "mv     sp, s2",       // restore the exec'd stack
        "jr     a0",           // into the main program's entry
        link = sym ldso_link,
    )
}

/// The aarch64 entry. `x3` is the header page
/// (`crates/arch-aarch64/src/hal.rs::exec_init_regs`), and the stack holds
/// `argc`/`argv`/`envp` exactly as `tools/crt0-aarch64.S` reads them.
#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov    x19, sp",           // the exec'd stack, to restore before the jump
        "mov    x20, x3",           // main program's ELF header page (before x3 is reused)
        "ldr    x0, [sp]",          // argc
        "add    x1, sp, #8",        // argv
        "add    x2, x1, x0, lsl #3",
        "add    x2, x2, #8",        // envp = argv + (argc + 1) * 8
        "mov    x16, sp",           // 16-byte alignment for the call: the logical
        "and    x16, x16, #-16",    // immediates have no SP form, so SP is only
        "mov    sp, x16",           // ever written by a `mov`
        "mov    x3, x20",           // main_hdr is `ldso_link`'s fourth argument
        "bl     {link}",
        "mov    sp, x19",           // restore the exec'd stack
        "br     x0",                // into the main program's entry
        link = sym ldso_link,
    )
}

/// The Rust half of `_start`: link the program and return its entry point. `r12`
/// (x86_64), `s1`/`s2` (riscv64) and `x19`/`x20` (aarch64) hold the initial stack
/// and the header page across this call; all of them are callee-saved, so the
/// call preserves them.
unsafe extern "C" fn ldso_link(argc: u64, argv: u64, envp: u64, main_hdr: u64) -> u64 {
    unsafe { ldso::rtld::run(argc, argv, envp, main_hdr) }
}
