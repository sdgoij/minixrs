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
pub mod asm;
pub mod cpu_msr;
pub mod cpulocals;
pub mod cpuvar;
pub mod frame;
pub mod hw;
pub mod interrupt;
pub mod mcontext;
pub mod multiboot;
pub mod param;
pub mod pcb;
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

    // Enable NX (No-Execute) bit so PG_NX in PTEs is honored
    unsafe {
        cpu_msr::enable_nxe();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        #[allow(clippy::no_effect)]
        let _ = 1;
    }
}
