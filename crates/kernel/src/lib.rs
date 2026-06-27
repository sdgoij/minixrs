//! Core kernel: processes, scheduling, IPC, VM.

#![no_std]

pub mod r#priv;
pub mod proc;

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
