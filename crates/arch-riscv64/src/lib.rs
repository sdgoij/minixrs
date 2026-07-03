//! RISC-V64-specific kernel code (bonus architecture).

#![no_std]

#[cfg(target_arch = "riscv64")]
pub mod hal;
pub mod mcontext;
pub mod param;
pub mod psl;
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
