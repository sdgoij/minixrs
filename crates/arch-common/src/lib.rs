//! Architecture-independent kernel primitives.
//! This crate provides types and utilities shared across all architectures.
//! These types match the C definitions from Minix 3.3.0 for ABI compatibility.

#![no_std]

pub mod com;
pub mod consts;
pub mod devio;
pub mod dmap;
pub mod endpoint;
pub mod fdt;
pub mod ipc;
pub mod ipcconst;
pub mod safecopies;
pub mod sys_config;
pub mod types;
pub mod vm;

/// Initialize arch-common subsystem.
pub fn init() {}

/// How a page-table builder reaches the physical addresses it writes.
///
/// The kernel normally reaches a table or a frame through the physmap. A builder that derefs the
/// physical address instead is leaning on the identity map — a VA a user window can occupy
/// (`KNOWN_ISSUES.md` item 38, `PHYSMAP.md` D4). The one unavoidable case is installing the window
/// into the boot tables: before that call returns there is no window to reach anything through, so
/// the root itself is written at its physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysAccess {
    /// Write at `VA == PA`. Only the boot tables, before the window exists.
    Identity,
    /// Write through the physmap. Every address space that already has the window.
    Physmap,
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let _ = 0;
    }
}
