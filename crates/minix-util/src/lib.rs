//! `minix-util` — Utility crate with client wrappers for system services.
//!
//! Provides thin, safe wrappers over IPC `sendrec` to MINIX system servers:
//!
//! - **DS client** (`ds`): Data Store publish/retrieve/subscribe/delete
//! - **DEVMAN client** (`devman`): Device tree operations
//! - **BDEV client** (`bdev`): Block device I/O
//! - **CDEV client** (`cdev`): Character device I/O
//!
//! All functions return `Err(MinixErr(71))` on host (`cfg(not(target_os = "none"))`).
//! Real implementations use `minix_std::sendrec` when `target_os = "none"`.

#![no_std]

pub mod bdev;
pub mod cdev;
pub mod devman;
pub mod ds;
