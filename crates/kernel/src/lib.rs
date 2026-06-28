//! Core kernel: processes, scheduling, IPC, VM.

#![no_std]

pub mod clock;
pub mod debug;
pub mod exec;
pub mod glo;
pub mod grants;
pub mod interrupt;
pub mod ipc;
pub mod pagetable;
pub mod r#priv;
pub mod proc;
pub mod profile;
pub mod sched;
pub mod smp;
pub mod syscall;
pub mod system;
pub mod table;
pub mod vm;

/// Kernel initialization.
pub fn init() {
    arch_x86_64::init();
    arch_common::init();
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
