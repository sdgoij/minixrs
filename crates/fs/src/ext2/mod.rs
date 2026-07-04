//! Ext2 Filesystem — `no_std` Rust port
//!
//! `unsafe_op_in_unsafe_fn` is allowed because this is a C-to-Rust port
//! where every `unsafe fn` body is inherently unsafe by construction.
//! Getting to full conformance is tracked as a future cleanup.

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
#![allow(non_snake_case)]
#![allow(clippy::all)]
#![allow(static_mut_refs)]
//!
//! This module implements the ext2 filesystem, ported from
//! Minix 3.3.0 (`minix/fs/ext2/`). Each submodule corresponds to
//! a `.c` source file and implements the same functions with
//! the same semantics.
//!
//! # Architecture
//!
//! | Module | C Source | Purpose |
//! |--------|----------|---------|
//! | `consts` | const.h | Constants, errno values, feature flags |
//! | `types` | type.h, inode.h, super.h | Core data structures |
//! | `glo` | glo.h | Global state and accessors |
//! | `utility` | utility.c | Byte-swap, bitmap ops, min, no_sys |
//! | `super_` | super.c | Super block read/write, group descriptors |
//! | `inode` | inode.c | Inode cache and I/O |
//! | `balloc` | balloc.c | Block bitmap allocation/free |
//! | `ialloc` | ialloc.c | Inode allocation/free |
//! | `path` | path.c | Path lookup, directory search |
//! | `read` | read.c | File read, block mapping |
//! | `write` | write.c | File write, block allocation |
//! | `link` | link.c | Link/unlink/rename/rdlink |
//! | `open` | open.c | File/dir/symlink creation |
//! | `mount` | mount.c | Mount/unmount |
//! | `protect` | protect.c | Permission checks, chmod/chown, getdents |
//! | `misc` | misc.c | Sync/flush/new_driver/bpeek |
//! | `stadir` | stadir.c | stat/statvfs |
//! | `time` | time.c | utimensat |
//! | `table` | table.c | VFS dispatch table |
//! | `main` | main.c | Server init and main loop |

pub mod balloc;
pub mod consts;
pub mod glo;
pub mod ialloc;
pub mod inode;
pub mod link;
pub mod main;
pub mod misc;
pub mod mount;
pub mod open;
pub mod path;
pub mod protect;
pub mod read;
pub mod stadir;
pub mod super_;
pub mod table;
pub mod time;
pub mod types;
pub mod utility;
pub mod write;


pub use balloc::*;
pub use inode::*;
pub use link::*;
pub use main::*;
pub use misc::*;
pub use mount::*;
pub use open::*;
pub use path::*;
pub use protect::*;
pub use read::*;
pub use stadir::*;
pub use super_::*;
pub use table::*;
pub use time::*;
pub use utility::*;
pub use write::*;
