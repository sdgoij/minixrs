//! Std smoke-test binary — built by `tools/build-std-hello.py`.
//!
//! Exercises the `std` PAL for the minix target end to end: the std
//! `_start` bootstrap, `println!` through the VFS/serial fd path, and a
//! real PM-assigned PID via `std::process::id()`.
//!
//! This is *not* a `userland` cargo bin (those link `minix-rt` instead of
//! `std`); it is compiled directly with the rust fork's stage1 compiler
//! against the std built by `x.py build library/std --target
//! x86_64-pc-minix`, and embedded into the boot images as `/bin/hello`
//! (see `crates/boot-image/src/manifest.rs`).

fn main() {
    println!("hello from minix std");
    println!("pid={}", std::process::id());
}
