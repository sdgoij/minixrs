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
/// `r9` carries the VA of the main program's ELF header page, which VFS mapped
/// read-only for a `PT_INTERP` exec. `argc`/`argv` are read for the ABI but
/// unused in Phase 0 (a shared object's initialisers get none).
///
/// The initial `rsp` is saved across the linking call and restored before the
/// jump: the main program's entry reads `argc`/`argv` from the stack it was
/// exec'd with, and a call frame of ours sitting on top of it would be read as
/// `argc`.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov    rdi, [rsp]",     // argc
        "lea    rsi, [rsp + 8]", // argv
        "mov    rdx, r9",        // main program's ELF header page
        "mov    r12, rsp",       // the exec'd stack, to restore before the jump
        "and    rsp, -16",       // SysV alignment for the call
        "call   {link}",
        "mov    rsp, r12",
        "jmp    rax",            // into the main program's entry
        link = sym ldso_link,
    )
}

/// The Rust half of `_start`: relocate the program and return its entry point.
/// `r12`, holding the initial stack, is callee-saved, so it survives this call.
unsafe extern "C" fn ldso_link(_argc: i64, _argv: *const *const u8, main_hdr: u64) -> u64 {
    unsafe { ldso::rtld::run(main_hdr) }
}
