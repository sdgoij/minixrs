//! VFS (Virtual File System) — the central filesystem multiplexer.
//!
//! The VFS is the core of the Minix file I/O subsystem. It receives
//! POSIX file I/O requests from user processes (via the Process Manager)
//! and forwards them to the appropriate filesystem server (MFS, ext2,
//! PFS, ProcFS, etc.).
//!
//! ## Module layout
//!
//! | Module       | Description                                      |
//! |--------------|--------------------------------------------------|
//! | `consts`     | Constants: table sizes, call numbers, flags       |
//! | `types`      | Core data structures: Fproc, Filp, Vnode, Vmnt…   |
//! | `glo`        | Global singleton `VfsGlobal`                      |
//! | `table`      | Dispatch table mapping call numbers to handlers    |
//! | `main`       | Entry point, main loop, SEF callbacks              |
//! | `filedes`    | File descriptor and filp operations                |
//! | `worker`     | Worker thread pool management                      |

// System-level code ported from C — all functions are inherently unsafe.
#![allow(unsafe_op_in_unsafe_fn)]

pub mod consts;
pub mod filedes;
pub mod glo;
pub mod main;
pub mod table;
pub mod types;
pub mod worker;
