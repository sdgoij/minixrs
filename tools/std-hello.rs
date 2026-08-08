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

    alloc_stress();

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

/// Exercise the mmap-backed free-list allocator: small-block churn that
/// reuses the free list, a large buffer that grows via `realloc` across
/// chunk boundaries, a high-alignment allocation, and a full-chunk free
/// that returns memory to the kernel with `munmap`.
fn alloc_stress() {
    // Small-block churn: allocate/free in a loop so blocks cycle through
    // the free list (and the payload contents stay intact).
    let mut v: Vec<Box<[u8; 64]>> = Vec::new();
    for i in 0..200u8 {
        v.push(Box::new([i; 64]));
    }
    for (i, b) in v.iter().enumerate() {
        assert_eq!(b[0], i as u8);
        assert_eq!(b[63], i as u8);
    }
    drop(v);

    // Grow a buffer through many reallocs (Vec doubling) up to ~1 MiB;
    // verify the contents survive the moves, then free the whole chunk.
    let mut big: Vec<u8> = Vec::with_capacity(16);
    for i in 0..1_000_000u32 {
        big.push((i % 251) as u8);
    }
    assert_eq!(big.len(), 1_000_000);
    assert_eq!(big[0], 0);
    assert_eq!(big[999_999], (999_999u32 % 251) as u8);
    drop(big);

    // High alignment: the allocator must align the payload to 4096.
    #[repr(align(4096))]
    struct Aligned([u8; 8192]);
    let a = Box::new(Aligned([0u8; 8192]));
    let payload = core::ptr::addr_of!((*a).0);
    assert_eq!(payload.align_offset(4096), 0);
    drop(a);

    println!("alloc: churn + realloc + align ok");
}
