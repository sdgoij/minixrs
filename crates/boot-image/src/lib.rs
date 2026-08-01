//! Boot image assembly library.
//!
//! Pure builders for the initramfs CPIO archive and the MinixFS root
//! image, the shared boot-binary manifest, and the per-target userland
//! build helper. Used by the kernel `build.rs` and by the thin
//! `mkinitramfs` / `mkminixfs` CLI wrappers.

pub mod cpio;
pub mod manifest;
pub mod minixfs;
pub mod targets;
