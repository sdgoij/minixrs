//! Architecture-independent kernel primitives.
//! This crate provides types and utilities shared across all architectures.
//! These types match the C definitions from Minix 3.3.0 for ABI compatibility.

#![no_std]

pub mod consts;
pub mod devio;
pub mod dmap;
pub mod endpoint;
pub mod ipc;
pub mod ipcconst;
pub mod safecopies;
pub mod sys_config;
pub mod types;
pub mod vm;

/// Initialize arch-common subsystem.
pub fn init() {}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
