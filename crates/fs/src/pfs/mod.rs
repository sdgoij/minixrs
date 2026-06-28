//! PFS (Pipe File System) — `no_std` Rust port
//!
//! Adapted from Minix 3.3.0 (`minix/fs/pfs/`).  The Pipe File System
//! is a purely in-memory filesystem that backs named pipes (FIFOs).
//! There is no on-disk format — all data lives in a static buffer pool.
//!
//! # Module layout
//!
//! | Module     | C Source    | Purpose                                        |
//! |------------|-------------|------------------------------------------------|
//! | `consts`   | const.h     | Constants, errno values, mode bits, sizes      |
//! | `types`    | inode.h, buf.h | Core data structures (Inode, Buf)           |
//! | `glo`      | glo.h, buf.h | Global state and accessors via raw pointers    |
//! | `bitmap`   | super.c     | Inode bitmap allocation and free               |
//! | `buffer`   | buffer.c    | Pipe data buffer pool management               |
//! | `inode`    | inode.c     | Inode cache: hash table, free list, alloc/free |
//! | `path`     | —           | Stub — PFS has no directory-based path lookup  |
//! | `read`     | read.c      | Pipe read/write operations                     |
//! | `link`     | link.c      | Truncate and stub link/unlink/rename           |
//! | `open`     | open.c      | Pipe and special node creation                 |
//! | `mount`    | mount.c, super.c | Mount/unmount, mountpoint check            |
//! | `misc`     | misc.c      | Sync, flush, new_driver, chmod                 |
//! | `stadir`   | stadir.c    | Stat and statvfs                               |
//! | `time`     | time.c      | Utime / timestamp helpers                      |
//! | `utility`  | utility.c   | no_sys, clock_time                             |
//! | `table`    | table.c     | VFS dispatch table (33 entries)                |
//! | `main`     | main.c      | Server init and main loop                      |

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::explicit_auto_deref)]

pub mod bitmap;
pub mod buffer;
pub mod consts;
pub mod glo;
pub mod inode;
pub mod link;
pub mod main;
pub mod misc;
pub mod mount;
pub mod open;
pub mod path;
pub mod read;
pub mod stadir;
pub mod table;
pub mod time;
pub mod types;
pub mod utility;

// ── Re-exports of all public functions from submodules ──

pub use bitmap::*;
pub use buffer::*;
pub use inode::*;
pub use link::*;
pub use main::*;
pub use misc::*;
pub use mount::*;
pub use open::*;
pub use path::*;
pub use read::*;
pub use stadir::*;
pub use table::*;
pub use time::*;
pub use utility::*;
