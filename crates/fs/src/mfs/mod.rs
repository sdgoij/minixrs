//! MFS (Minix Filesystem) — `no_std` Rust port
//!
//! `unsafe_op_in_unsafe_fn` is allowed because this is a C-to-Rust port
//! where every `unsafe fn` body is inherently unsafe by construction.
//! Getting to full conformance is tracked as a future cleanup.

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::explicit_auto_deref)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::manual_is_multiple_of)]
//!
//! This module implements the MFS V3 filesystem, ported from
//! Minix 3.3.0 (`minix/fs/mfs/`). Each submodule corresponds to
//! a `.c` source file and implements the same functions with
//! the same semantics.
//!
//! # Architecture
//!
//! | Module | C Source | Purpose |
//! |--------|----------|---------|
//! | `consts` | const.h | Constants, errno values, mode bits |
//! | `types` | type.h, super.h, inode.h, mfsdir.h | Core data structures |
//! | `glo` | glo.h | Global state and accessors |
//! | `utility` | utility.c | Byte-swap, time, min, no_sys |
//! | `super_block` | super.c | Super block read/write, bitmap alloc/free |
//! | `inode` | inode.c | Inode cache and I/O |
//! | `cache` | cache.c | Zone alloc/free |
//! | `path` | path.c | Path lookup, directory search |
//! | `read` | read.c | File read, block mapping |
//! | `write` | write.c | File write, block allocation, truncate |
//! | `link` | link.c | Link/unlink/rename |
//! | `open` | open.c | File/dir/symlink creation |
//! | `mount` | mount.c | Mount/unmount |
//! | `protect` | protect.c | Permission checks, chmod/chown |
//! | `misc` | misc.c | Sync/flush/new_driver/bpeek |
//! | `stats` | stats.c | Free bit counting |
//! | `time` | time.c | utimensat |
//! | `table` | table.c | VFS dispatch table |
//! | `main` | main.c | Server init and main loop |

pub mod cache;
pub mod consts;
pub mod glo;
pub mod inode;
pub mod link;
pub mod main;
pub mod misc;
pub mod mount;
pub mod open;
pub mod path;
pub mod protect;
pub mod read;
pub mod stats;
pub mod super_block;
pub mod table;
pub mod time;
pub mod types;
pub mod utility;
pub mod write;

// ── Re-exports of all public functions from submodules ──

pub use cache::*;
pub use inode::*;
pub use link::*;
pub use main::*;
pub use misc::*;
pub use mount::*;
pub use open::*;
pub use path::*;
pub use protect::*;
pub use read::*;
pub use stats::*;
pub use super_block::*;
pub use table::*;
pub use time::*;
pub use utility::*;
pub use write::*;
