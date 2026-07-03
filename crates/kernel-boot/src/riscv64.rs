//! RISC-V64 kernel boot binary entry point.
//!
//! Build with: `cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-none-elf`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![allow(static_mut_refs)]
#![cfg(target_arch = "riscv64")]

#[cfg(not(test))]
use core::panic::PanicInfo;

use core::arch::global_asm;

/// _start entry point — called by QEMU/OpenSBI.
/// a0 = hart ID, a1 = DTB pointer.
global_asm!(
    r#"
.section .text.boot, "ax"
.globl _start

_start:
    # Set up a temporary stack
    la      sp, _start
    li      t0, 0x10000
    add     sp, sp, t0

    # Clear BSS
    la      t0, __bss_start
    la      t1, __bss_end
    bge     t0, t1, 2f
1:
    sd      zero, 0(t0)
    addi    t0, t0, 8
    blt     t0, t1, 1b
2:

    # Call kmain(hart_id, dtb_ptr)
    mv      a0, a0
    mv      a1, a1
    call    kmain

    # Should never reach here
    wfi
    j       _start
"#
);

/// BSS symbols defined by linker script.
#[unsafe(no_mangle)]
pub static __bss_start: u8 = 0;
#[unsafe(no_mangle)]
pub static __bss_end: u8 = 0;

/// RISC-V64 kernel main entry.
///
/// # Safety
///
/// Must be called once on the boot hart in S-mode, with a0=hart_id and a1=dtb_ptr.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmain(hart_id: u64, dtb_ptr: u64) -> ! {
    // Only hart 0 proceeds
    if hart_id != 0 {
        loop {
            unsafe { core::arch::asm!("wfi", options(nomem, nostack)); }
        }
    }

    // Parse FDT for memory information
    if let Some((mem_base, mem_size)) = arch_riscv64::boot::parse_fdt_memory(dtb_ptr as *const u8)
    {
        let mut mmap = arch_riscv64::alloc::PhysicalMemoryMap::new();
        let kernel_end = 0x80200000u64 + 0x200000u64; // 2MB kernel
        if kernel_end < mem_base + mem_size {
            mmap.add(kernel_end, mem_base + mem_size);
        }
        arch_riscv64::alloc::init_allocator(&mmap);
    }

    // Set up STVEC to point to the trap vector
    let trap_vec = arch_riscv64::trap_asm::trap_vector_addr();
    unsafe {
        core::arch::asm!("csrw stvec, {addr}", addr = in(reg) trap_vec, options(nomem, nostack));
    }

    // Initialize per-CPU data (tp register)
    arch_riscv64::cpulocals::init_cpulocals();

    // Print banner via SBI
    for &b in b"\r\nHello MINIX/RISC-V!\r\n" {
        arch_riscv64::sbi::console_putchar(b);
    }

    // Initialize kernel subsystems
    kernel::init();
    arch_common::init();

    // Register basic syscall handlers
    unsafe {
        kernel::syscall::init_basic_syscalls();
    }
    unsafe {
        arch_riscv64::trap::register_syscall_handler(kernel::syscall::dispatch_basic_syscall);
    }

    // Initialize timer (100 Hz)
    unsafe {
        arch_riscv64::clint::init_timer(100);
    }

    // Enable S-mode interrupts (timer + external)
    unsafe {
        let mut sie_val: u64;
        core::arch::asm!("csrr {val}, sie", val = out(reg) sie_val, options(nomem, nostack));
        sie_val |= (1u64 << 5) | (1u64 << 9); // STIE | SEIE
        core::arch::asm!("csrw sie, {val}", val = in(reg) sie_val, options(nomem, nostack));
    }

    // Initialize PLIC
    unsafe {
        arch_riscv64::plic::init_plic();
    }

    // TODO: Load boot processes from initramfs
    // TODO: Create per-process SV39 page tables
    // TODO: Set up scheduler
    // TODO: switch_to_user(first_proc)

    for &b in b"RISC-V kernel initialized. Halting.\r\n" {
        arch_riscv64::sbi::console_putchar(b);
    }

    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)); }
    }
}

/// Panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)); }
    }
}
