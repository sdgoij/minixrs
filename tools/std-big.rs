//! Large-binary verification — compiled by `tools/build-std-big.py` and
//! injected into the boot images as `/bin/big` via `MINIXFS_EXTRA`.
//!
//! Proves the old 16 MiB executable cap and the contiguous whole-ELF
//! allocation are gone: the image is demand-paged from the file by VM, so a
//! binary far past the old cap execs and its pages fault in lazily on first
//! access. The 33 MiB static lives in `.rodata` (its own PT_LOAD); the
//! checksum touches one byte per page, so every page must fault in and read
//! back exactly what the file holds.

use std::sync::atomic::{AtomicUsize, Ordering};

/// 33 MiB of static data — past the old 16 MiB executable cap. Referenced
/// and `#[used]` so the linker cannot drop it.
#[used]
static BIG: [u8; 33 * 1024 * 1024] = [0xAB; 33 * 1024 * 1024];

/// Force the static to be kept even under LTO: read one byte per page.
static TOUCHED: AtomicUsize = AtomicUsize::new(0);

fn main() {
    let mut sum: u64 = 0;
    let mut i = 0usize;
    while i < BIG.len() {
        sum = sum.wrapping_add(BIG[i] as u64);
        i += 4096;
    }
    TOUCHED.store(i, Ordering::Relaxed);
    println!(
        "big: ok, pid={} size={} checksum={}",
        std::process::id(),
        BIG.len(),
        sum
    );
    std::process::exit(0);
}
