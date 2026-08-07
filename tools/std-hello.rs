//! Std smoke-test binary — built by `tools/build-std-hello.py`.
//!
//! Exercises the `std` PAL for the minix target end to end: the std
//! `_start` bootstrap, `println!` through the VFS/serial fd path, a real
//! PM-assigned PID via `std::process::id()`, and the thread + TLS support
//! (1:1 kernel threads + native ELF TLS): spawns threads that each keep a
//! per-thread `thread_local!` counter, bump a shared atomic, and are joined
//! back. `println!` from the workers also exercises the futex-backed stdout
//! lock.
//!
//! This is *not* a `userland` cargo bin (those link `minix-rt` instead of
//! `std`); it is compiled directly with the rust fork's stage1 compiler
//! against the std built by `x.py build library/std --target
//! x86_64-pc-minix`, and embedded into the boot images as `/bin/hello`
//! (see `crates/boot-image/src/manifest.rs`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

thread_local! {
    static TLS_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn main() {
    println!("hello from minix std");
    println!("pid={}", std::process::id());

    let shared = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for i in 0..4 {
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || {
            // Per-thread TLS: each thread must see its own counter.
            TLS_HITS.with(|c| c.set(c.get() + 1));
            let tls = TLS_HITS.with(|c| c.get());
            let n = shared.fetch_add(1, Ordering::SeqCst) + 1;
            println!("  thread {i}: shared={n} tls={tls}");
        }));
    }
    for h in handles {
        h.join().expect("join failed");
    }
    let total = shared.load(Ordering::SeqCst);
    println!("threadstd: all joined, shared={total} (expected 4)");
    if total != 4 {
        std::process::exit(1);
    }
}
