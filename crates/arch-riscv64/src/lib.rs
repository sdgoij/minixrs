//! RISC-V64-specific kernel code (bonus architecture).

#![no_std]

use core::sync::atomic::AtomicU64;

/// Boot page table root physical address.
/// Set once during boot, before any per-process page tables are active.
pub static BOOT_CR3: AtomicU64 = AtomicU64::new(0);

#[cfg(target_arch = "riscv64")]
pub mod alloc;
#[cfg(target_arch = "riscv64")]
pub mod boot;
#[cfg(target_arch = "riscv64")]
pub mod clint;
#[cfg(target_arch = "riscv64")]
pub mod cpulocals;
#[cfg(target_arch = "riscv64")]
pub mod hal;
pub mod mcontext;
pub mod param;
#[cfg(target_arch = "riscv64")]
pub mod plic;
pub mod psl;
pub mod pte;
pub mod sbi;
#[cfg(target_arch = "riscv64")]
pub mod switch;
#[cfg(target_arch = "riscv64")]
pub mod trap;
#[cfg(target_arch = "riscv64")]
pub mod trap_asm;
#[cfg(target_arch = "riscv64")]
pub mod uart;
pub mod vmparam;

/// Initialize RISC-V64 architecture subsystem.
pub fn init() {
    // No-op for now
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let _ = 0;
    }
}
