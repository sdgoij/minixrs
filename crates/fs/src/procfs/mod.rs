//! ProcFS (Process File System) — `no_std` Rust port
//!
//! Adapted from Minix 3.3.0 (`minix/fs/procfs/`).
//!
//! # Module layout
//!
//! | Module    | C source     | Purpose                              |
//! |-----------|--------------|--------------------------------------|
//! | `consts`  | const.h      | Constants, mode bits, limits         |
//! | `types`   | type.h       | Core types (`Load`, `File`, etc.)    |
//! | `buf`     | buf.c        | Output buffer with `core::fmt::Write`|
//! | `root`    | root.c       | Static root file definitions         |
//! | `pid`     | pid.c        | Per-process PID directory files      |
//! | `tree`    | tree.c       | VTreeFS hook implementations         |
//! | `cpuinfo` | cpuinfo.c/h  | CPU info printing for `/proc/cpuinfo`|
//! | `misc`    | util.c       | Miscellaneous utilities              |
//! | `main`    | main.c       | Entry point and tree construction    |

#![allow(unsafe_op_in_unsafe_fn)]

pub mod buf;
pub mod consts;
pub mod cpuinfo;
pub mod main;
pub mod misc;
pub mod pid;
pub mod root;
pub mod tree;
pub mod types;
