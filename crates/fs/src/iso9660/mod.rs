//! ISO 9660 File System — `no_std` Rust port
//!
//! Ported from `minix/fs/iso9660fs/` (Minix 3.3.0).
//!
//! `unsafe_op_in_unsafe_fn` is allowed because this is a C-to-Rust port
//! where every `unsafe fn` body is inherently unsafe by construction.
//!
//! # Architecture
//!
//! | Module | C Source | Purpose |
//! |--------|----------|---------|
//! | `consts` | const.h | Constants, errno values, mode bits |
//! | `types` | inode.h, super.h | Core data structures (DirRecord, ExtAttrRec, Iso9660VdPri) |
//! | `glo` | glo.h | Global state |
//! | `utility` | utility.c | Date parsing, no_sys, byte helpers |
//! | `super_block` | super.c | Volume descriptor reading & validation |
//! | `inode` | inode.c | Dir record / ext attr record cache |
//! | `mount` | mount.c | Mount/unmount/mountpoint |
//! | `path` | path.c | Path lookup, directory search |
//! | `read` | read.c | File read, getdents |
//! | `stadir` | stadir.c | Stat/statvfs |
//! | `misc` | misc.c | Sync/flush/new_driver |
//! | `table` | table.c | VFS dispatch table |
//! | `main` | main.c | Server init and main loop |

#![allow(unsafe_op_in_unsafe_fn)]

pub mod consts;
pub mod glo;
pub mod inode;
pub mod main;
pub mod misc;
pub mod mount;
pub mod path;
pub mod read;
pub mod stadir;

// `super` is a Rust keyword, so we use `super_block` as the module name
// and map it to the file `super.rs` via the path attribute.
#[path = "super.rs"]
pub mod super_block;

pub mod table;
pub mod types;
pub mod utility;
