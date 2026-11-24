//! A rustc linked into a single static binary.
//!
//! `rustc-main` links `rustc_driver`, which is a `dylib` so that the compiler
//! front end can be built in parallel with its consumers and shared with
//! `rustdoc`. A host that cannot produce dylibs cannot link that wrapper, and
//! the minix executable target is one: it is a fully static ELF. This binary
//! depends on `rustc_driver_impl` directly instead, so the compiler is an rlib
//! graph and the LLVM backend is the statically selected
//! `rustc_codegen_llvm::LlvmCodegenBackend` rather than a dylib loaded by name.
//!
//! The `[patch.crates-io]` table in this package's manifest is what makes the
//! compiler's dependency graph buildable for minix, so nothing under `rust/` has
//! to be modified.

// `rustc_private` links the standard library from the compiler's own sysroot.
#![feature(rustc_private)]
#![expect(unused_crate_dependencies)]

use std::process::ExitCode;

// The port's C library: linked for its exported C ABI symbols (`malloc`, `write`,
// `dlopen`, `__cxa_atexit`), which the C++ runtime and the LLVM wrapper call but
// no Rust code here names.
use minix_libc as _;

// minixrs has no kernel entropy source yet, and `rand` reaches the compiler
// through `rustc_abi`, `rustc_incremental`, `rustc_interface` and
// `rustc_thread_pool`. Register a weak backend so `getrandom` compiles and the
// compiler's non-cryptographic seeds have a source, exactly as the coreutils
// multicall does (`coreutils/src/bin/coreutils.rs`); both mix the stack address,
// a counter and the cycle counter through SplitMix64. Replace this when the OS
// grows a kernel entropy source.
mod minix_rng {
    use core::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn splitmix64(mut x: u64) -> impl FnMut() -> u64 {
        move || {
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
    }

    pub fn fill(dest: &mut [u8]) -> Result<(), getrandom::Error> {
        let stack = core::ptr::addr_of!(COUNTER) as u64;
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        #[cfg(target_arch = "x86_64")]
        let cycles = unsafe { core::arch::x86_64::_rdtsc() };
        #[cfg(not(target_arch = "x86_64"))]
        let cycles = 0u64;
        let mut next = splitmix64((stack << 32) ^ stack ^ counter ^ cycles);
        for chunk in dest.chunks_mut(8) {
            let x = next();
            for (i, byte) in chunk.iter_mut().enumerate() {
                *byte = (x >> (i * 8)) as u8;
            }
        }
        Ok(())
    }

    // getrandom 0.3 selects this symbol under `--cfg getrandom_backend="custom"`.
    #[unsafe(no_mangle)]
    pub unsafe extern "Rust" fn __getrandom_v03_custom(
        dest: *mut u8,
        len: usize,
    ) -> Result<(), getrandom::Error> {
        let dest = unsafe { core::slice::from_raw_parts_mut(dest, len) };
        fill(dest)
    }
}

rustc_driver_impl::override_c_allocator_in_binary!();

unsafe extern "C" {
    static __init_array_start: u8;
    static __init_array_end: u8;
}

/// Run the image's `.init_array` constructors, which nothing else does here.
///
/// A C++ translation unit registers its global initialisers in `.init_array`, and
/// `crt0` walks that array for a C program. A Rust binary has no `crt0`: `std`'s
/// `_start` is the entry point (there is no `main` in C either, only the
/// `lang_start` wrapper), and its minix PAL never walks the array. LLVM registers
/// every `cl::opt` in such a constructor, so without this walk LLVM's
/// `cl::ParseCommandLineOptions` knows none of its own options and `configure_llvm`
/// fails on each argument it passes.
///
/// This is the `rustc`-only form of the gap: a Rust program that links C++ with
/// static constructors needs the same walk, and the proper home for it is
/// `std`'s minix PAL (`rust/library/std/src/sys/pal/minix/mod.rs`), beside its
/// `init_tls`. Keeping it here avoids rebuilding `std` for this one binary. The
/// gap is recorded in `KNOWN_ISSUES.md`.
fn run_static_constructors() {
    type Ctor = unsafe extern "C" fn();
    // SAFETY: `tools/minix-user.ld` places both bounds around `.init_array`, an
    // 8-aligned array of function pointers. LLVM is built with clang, whose
    // initialisers are `void _GLOBAL__sub_I_*()`, so they take no arguments.
    unsafe {
        let start = core::ptr::addr_of!(__init_array_start).cast::<Ctor>();
        let end = core::ptr::addr_of!(__init_array_end).cast::<Ctor>();
        let mut slot = start;
        while slot < end {
            (*slot)();
            slot = slot.add(1);
        }
    }
}

fn main() -> ExitCode {
    run_static_constructors();
    rustc_driver_impl::main()
}
