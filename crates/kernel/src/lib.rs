//! Core kernel: processes, scheduling, IPC, VM.

#![no_std]

pub mod clock;
pub mod debug;
pub mod elf;
pub mod exec;
pub mod glo;
pub mod grants;
pub mod initramfs;
pub mod interrupt;
pub mod ipc;
pub mod pagetable;
pub mod r#priv;
pub mod proc;
pub mod profile;
pub mod sched;
pub mod ser_input;
pub mod smp;
pub mod syscall;
pub mod system;
pub mod table;
pub mod vm;

// Include the generated initramfs data when embed_initramfs is active.
// CARGO_MANIFEST_DIR = crates/kernel/, ../../target/ = target/
#[cfg(feature = "embed_initramfs")]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/initramfs_data.rs"
));

#[cfg(feature = "qemu-tests")]
pub mod tests;

/// Kernel initialization.
pub fn init() {
    #[cfg(target_arch = "x86_64")]
    arch_x86_64::init();
    #[cfg(target_arch = "riscv64")]
    arch_riscv64::init();
    arch_common::init();
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let _ = 0;
    }
}
