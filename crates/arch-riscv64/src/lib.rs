//! RISC-V64-specific kernel code (bonus architecture).

#![no_std]

#[cfg(target_arch = "riscv64")]
pub mod alloc;
#[cfg(target_arch = "riscv64")]
pub mod boot;
#[cfg(target_arch = "riscv64")]
pub mod hal;
pub mod mcontext;
pub mod param;
pub mod psl;
pub mod pte;
pub mod sbi;
#[cfg(target_arch = "riscv64")]
pub mod switch;
#[cfg(target_arch = "riscv64")]
pub mod trap;
#[cfg(target_arch = "riscv64")]
pub mod trap_asm;
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
