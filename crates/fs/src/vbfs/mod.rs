//! Virtual Block File System (VBFS) — VirtualBox shared folder bridge.
//!
//! Thin server wrapping libsffs + libvboxfs. Ported from
//! `.refs/minix-3.3.0/minix/fs/vbfs/vbfs.c` (~140 lines).

pub mod config;
pub mod server;
