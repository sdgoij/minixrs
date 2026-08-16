//! AArch64 kernel boot binary entry point (boot-test variant).
//!
//! Build with: `cargo build -p kernel-boot --bin kernel-boot-aarch64-boot --target aarch64-unknown-minix --features ...,boot-test`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![allow(static_mut_refs)]
#![cfg(target_arch = "aarch64")]

include!("aarch64.rs");
