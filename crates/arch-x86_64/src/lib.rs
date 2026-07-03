//! x86_64-specific kernel code.
//!
//! These modules port the Minix 3.3.0 i386 headers to x86_64, adapting
//! for:
//! - 4-level page tables (PML4→PDPT→PD→PT) instead of 2-level
//! - 64-bit PTEs with NX bit (bit 63), 8 bytes per entry
//! - 16-byte IDT gate descriptors (64-bit offset split across two qwords)
//! - 64-bit TSS with RSP0/1/2 and IST1-7 fields
//! - `syscall`/`sysret` MSR-based syscall (not `int 0x80`)
//! - Full 16 GPR register set (rax–r15)
//! - System V AMD64 calling convention (not cdecl)
//! - Region descriptor with 64-bit base for LGDT/LIDT
//! - `swapgs`-based per-CPU data access via GS segment

#![no_std]

use core::sync::atomic::{AtomicU64, Ordering};

pub mod alloc;
pub mod apic;
pub mod arch_proc;
pub mod arch_syscall;
pub mod asm;
pub mod cpu_msr;
pub mod cpulocals;
pub mod cpuvar;
pub mod frame;
pub mod hal;
pub mod hw;
pub mod idt;
pub mod interrupt;
pub mod mcontext;
pub mod multiboot;
pub mod param;
pub mod pcb;
pub mod pci;
pub mod psl;
pub mod pte;
pub mod segments;
pub mod spinlock;
pub mod tss;
pub mod vmparam;

/// The CR3 value used during boot (identity-mapped page table).
/// Set during `init()` to the current CR3 value.
/// Used by syscall dispatch to switch between per-process and kernel
/// page tables.
pub static BOOT_CR3: AtomicU64 = AtomicU64::new(0);

/// Initialize x86_64 architecture subsystem.
pub fn init() {
    // Save the boot CR3 value for per-process page table management
    let cr3 = unsafe { asm::read_cr3() };
    BOOT_CR3.store(cr3, Ordering::Relaxed);

    // Enable NX and SCE (required for sysretq to work)
    unsafe {
        cpu_msr::enable_nxe_and_sce();
    }

    // Initialize the IDT with default interrupt gates and load via lidt.
    unsafe {
        idt::init_idt();
    }

    // Set up syscall MSRs (STAR, LSTAR, SF_MASK) for syscall/sysret.
    // The LSTAR entry point will be filled in when the syscall handler
    // is registered; for now we write 0 as a placeholder.
    unsafe {
        arch_syscall::setup_syscall_msrs(0);
    }
}

/// Boot-time kernel stack for ring-3→ring-0 transitions (RSP0).
#[cfg(target_os = "none")]
static mut BOOT_KSTACK: [u8; 4096] = [0u8; 4096];
/// IST1 stack for page fault handler.
#[cfg(target_os = "none")]
static mut BOOT_IST1_STACK: [u8; 4096] = [0u8; 4096];
/// IST2 stack for double fault handler.
#[cfg(target_os = "none")]
static mut BOOT_IST2_STACK: [u8; 4096] = [0u8; 4096];
/// Boot TSS.
#[cfg(target_os = "none")]
static mut BOOT_TSS: crate::tss::Tss64 = crate::tss::Tss64::new_zeroed();
/// Boot GDT (16 entries).
#[cfg(target_os = "none")]
static mut BOOT_GDT: [u64; segments::NGDT as usize] = [
    0x0000000000000000,
    0x00AF9A0000000000,
    0x00CF920000000000,
    0x00CFF2000000FFFF,
    0x00AFFA000000FFFF,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
];

/// Set up the TSS and GDT required for ring-3 interrupts.
///
/// Must be called once during boot, on the BSP, before entering user mode.
///
/// # Safety
///
/// Must be called in ring 0.
#[cfg(target_os = "none")]
pub unsafe fn init_tss_for_boot() {
    use core::ptr;
    use segments::GTSS_SEL;

    // Wrap all unsafe operations in a single block for Rust 2024 compatibility.
    unsafe {
        let stack_top = ptr::addr_of_mut!(BOOT_KSTACK) as *mut u8 as u64 + 4096;
        let ist1_top = ptr::addr_of_mut!(BOOT_IST1_STACK) as *mut u8 as u64 + 4096;
        let ist2_top = ptr::addr_of_mut!(BOOT_IST2_STACK) as *mut u8 as u64 + 4096;

        let tss_bytes = ptr::addr_of_mut!(BOOT_TSS) as *mut u8;
        ptr::write_unaligned(tss_bytes.add(4) as *mut u32, stack_top as u32);
        ptr::write_unaligned(tss_bytes.add(8) as *mut u32, (stack_top >> 32) as u32);
        ptr::write_unaligned(tss_bytes.add(36) as *mut u32, ist1_top as u32);
        ptr::write_unaligned(tss_bytes.add(40) as *mut u32, (ist1_top >> 32) as u32);
        ptr::write_unaligned(tss_bytes.add(44) as *mut u32, ist2_top as u32);
        ptr::write_unaligned(tss_bytes.add(48) as *mut u32, (ist2_top >> 32) as u32);

        let gdt = ptr::addr_of_mut!(BOOT_GDT) as *mut u8;
        let tss_addr = tss_bytes as u64;
        let tss_limit = 103u32;
        let tss_desc = gdt.add(GTSS_SEL as usize * 8);
        let low: u64 = (tss_limit as u64 & 0xFFFF)
            | ((tss_addr & 0xFFFF) << 16)
            | (((tss_addr >> 16) & 0xFF) << 32)
            | (0x89u64 << 40)
            | ((((tss_limit >> 16) as u64) & 0x0F) << 48)
            | (((tss_addr >> 24) & 0xFF) << 56);
        let high: u64 = (tss_addr >> 32) & 0xFFFFFFFF;
        ptr::write_unaligned(tss_desc as *mut u64, low);
        ptr::write_unaligned(tss_desc.add(8) as *mut u64, high);

        // Load new GDT
        let gdt_base = gdt as u64;
        let gdt_limit = ((segments::NGDT as usize * 8) - 1) as u16;
        let mut gdtr = [0u8; 10];
        ptr::write_unaligned(gdtr.as_mut_ptr() as *mut u16, gdt_limit);
        ptr::write_unaligned(gdtr.as_mut_ptr().add(2) as *mut u64, gdt_base);
        crate::asm::lgdt(&gdtr);

        // Load task register
        crate::asm::ltr(GTSS_SEL << 3);
    }
}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn boot_cr3_initialized() {
        // init() is not called in tests; verify default.
        assert_eq!(BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn modules_compile() {
        // Verify all new modules export their expected public items.
        let _ = arch_syscall::SYSCALL_CS;
        let _ = arch_syscall::SYSRET_CS;
        // IDT is a mutable static; just check address exists.
        #[allow(static_mut_refs)]
        let _ = core::ptr::addr_of!(idt::IDT);
    }
}
