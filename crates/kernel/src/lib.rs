//! Core kernel: processes, scheduling, IPC, VM.

#![no_std]

pub mod debug;
pub mod glo;
pub mod ipc;
pub mod r#priv;
pub mod proc;
pub mod profile;
pub mod sched;
pub mod system;
pub mod table;

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
