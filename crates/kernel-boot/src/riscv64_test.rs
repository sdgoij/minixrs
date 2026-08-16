//! RISC-V64 kernel boot binary entry point (integration-test variant).
//!
//! Build with: `cargo build -p kernel-boot --bin kernel-boot-riscv64-test --target riscv64gc-unknown-minix --features ...,integration-tests`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![allow(static_mut_refs)]
#![cfg(target_arch = "riscv64")]

include!("riscv64.rs");
