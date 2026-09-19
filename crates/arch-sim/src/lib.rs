//! Host-mode HAL for the MINIX/Rust kernel.
//!
//! This crate implements the kernel's HAL surface with host primitives, so the
//! arch-independent kernel — process table, scheduling, IPC, VM bookkeeping —
//! can run under `cargo test` instead of only inside QEMU.
//!
//! It also serves as the skeleton for `arch-wasm32`: everything here that is a
//! host *import* there is a plain function call in this port, so the difference
//! between the two is the boundary, not the logic.
//!
//! # Host-only
//!
//! Every device is simulated: there is no real port I/O, no paging hardware,
//! and no interrupts. The crate is `#![no_std]` so it does not force a
//! runtime onto the kernel, but it must not be selected for a bare-metal
//! target — see `hal::init`.
//!
//! # Determinism
//!
//! Time, the physical page arena, and console I/O are all fixed-function so a
//! failing test reproduces exactly. The kernel's notion of "now" advances a
//! fixed step on every read, which is what keeps a spin-wait from hanging a
//! test run.

#![no_std]

pub mod frame;
pub mod hal;
pub mod mcontext;

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Interior mutability the kernel needs but `static mut` cannot express
/// without the `static_mut_refs` lint.
///
/// Access is only ever from the test thread — the workspace pins
/// `RUST_TEST_THREADS=1` — and every cursor that could expose a torn update is
/// an atomic.
#[repr(transparent)]
pub struct Shared<T>(UnsafeCell<T>);

// SAFETY: the simulator is single-threaded by construction; see the type docs.
unsafe impl<T> Sync for Shared<T> {}

impl<T> Shared<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    /// # Safety
    ///
    /// The caller must not hold another live borrow of the same value.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get(&self) -> &mut T {
        unsafe { &mut *self.0.get() }
    }
}

pub const PAGE_SIZE: usize = 4096;

// ---------------------------------------------------------------- console

const CONSOLE_CAP: usize = 1 << 16;
static CONSOLE: Shared<[u8; CONSOLE_CAP]> = Shared::new([0; CONSOLE_CAP]);
static CONSOLE_LEN: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_DROPPED: AtomicUsize = AtomicUsize::new(0);

/// Kernel console output. Dropped (and counted) once the buffer is full, so a
/// runaway kernel cannot exhaust host memory.
pub fn console_write(byte: u8) {
    let len = CONSOLE_LEN.load(Ordering::Relaxed);
    if len >= CONSOLE_CAP {
        CONSOLE_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // SAFETY: single-threaded; `len` is behind a live atomic and each byte is
    // written at a distinct index.
    unsafe { CONSOLE.get()[len] = byte };
    CONSOLE_LEN.store(len + 1, Ordering::Relaxed);
}

/// Consume console output, returning how many bytes were copied. Bytes are
/// removed, so successive calls see each one exactly once.
pub fn console_drain(out: &mut [u8]) -> usize {
    let len = CONSOLE_LEN.load(Ordering::Relaxed);
    let take = len.min(out.len());
    // SAFETY: single-threaded; the region is disjoint from `out`.
    unsafe {
        let buf = CONSOLE.get();
        out[..take].copy_from_slice(&buf[..take]);
        let rest = len - take;
        if rest > 0 {
            core::ptr::copy(buf[take..len].as_ptr(), buf.as_mut_ptr(), rest);
        }
    }
    CONSOLE_LEN.store(len - take, Ordering::Relaxed);
    take
}

pub fn console_len() -> usize {
    CONSOLE_LEN.load(Ordering::Relaxed)
}

/// Console bytes the kernel produced after the buffer filled.
pub fn console_dropped() -> usize {
    CONSOLE_DROPPED.load(Ordering::Relaxed)
}

pub fn console_reset() {
    CONSOLE_LEN.store(0, Ordering::Relaxed);
    CONSOLE_DROPPED.store(0, Ordering::Relaxed);
}

// ------------------------------------------------------------------ input

const INPUT_CAP: usize = 4096;
static INPUT: Shared<[u8; INPUT_CAP]> = Shared::new([0; INPUT_CAP]);
static INPUT_LEN: AtomicUsize = AtomicUsize::new(0);

/// Queue a byte for the kernel to read back through `poll_console`.
pub fn console_push_input(byte: u8) {
    let len = INPUT_LEN.load(Ordering::Relaxed);
    if len >= INPUT_CAP {
        return;
    }
    // SAFETY: single-threaded; see `console_write`.
    unsafe { INPUT.get()[len] = byte };
    INPUT_LEN.store(len + 1, Ordering::Relaxed);
}

pub fn console_push_input_str(s: &str) {
    for byte in s.bytes() {
        console_push_input(byte);
    }
}

pub fn console_len_input() -> usize {
    INPUT_LEN.load(Ordering::Relaxed)
}

/// Take the next queued input byte, if any.
pub fn console_take_input() -> Option<u8> {
    let len = INPUT_LEN.load(Ordering::Relaxed);
    if len == 0 {
        return None;
    }
    // SAFETY: single-threaded; see `console_write`.
    let byte = unsafe {
        let buf = INPUT.get();
        let byte = buf[0];
        core::ptr::copy(buf[1..len].as_ptr(), buf.as_mut_ptr(), len - 1);
        byte
    };
    INPUT_LEN.store(len - 1, Ordering::Relaxed);
    Some(byte)
}

// ------------------------------------------------------------------ clock

/// Cycles advance by this much on every read. A fixed step keeps runs
/// reproducible while guaranteeing that a kernel spin-wait on the clock
/// terminates rather than hanging the test.
const CYCLE_STEP: u64 = 1000;

static CYCLES: AtomicU64 = AtomicU64::new(0);
static TSC_SWITCH: AtomicU64 = AtomicU64::new(0);
static PROFILE_CALLBACK: Shared<Option<unsafe extern "C" fn()>> = Shared::new(None);

/// Advance the clock without a read, for tests that need to age the system and
/// for HAL idle paths, which must make progress despite there being no timer
/// interrupt to wake the kernel.
pub fn advance_cycles(n: u64) {
    CYCLES.fetch_add(n, Ordering::Relaxed);
}

/// Read the clock, advancing it as a side effect.
pub fn clock_read() -> u64 {
    CYCLES.fetch_add(CYCLE_STEP, Ordering::Relaxed) + CYCLE_STEP
}

pub fn tsc_switch() -> u64 {
    TSC_SWITCH.load(Ordering::Relaxed)
}

pub fn set_tsc_switch(val: u64) {
    TSC_SWITCH.store(val, Ordering::Relaxed);
}

pub fn set_profile_callback(callback: Option<unsafe extern "C" fn()>) {
    // SAFETY: single-threaded; the kernel installs this from its own thread.
    unsafe { *PROFILE_CALLBACK.get() = callback };
}

/// Run the profiler callback, if one is installed. The simulator has no timer
/// interrupt, so sampling is driven explicitly by tests.
pub fn profile_tick() {
    // SAFETY: single-threaded; only a callback installed through
    // `set_profile_callback` is run.
    let cb = unsafe { *PROFILE_CALLBACK.get() };
    if let Some(f) = cb {
        // SAFETY: the kernel installed a function of this signature.
        unsafe { f() };
    }
}

// -------------------------------------------------------- physical memory

/// 8 MiB of simulated physical memory, tracked a 4 KiB page at a time so
/// `free_phys_pages` is a real count rather than an estimate.
const ARENA_PAGES: usize = 2048;
const BITMAP_WORDS: usize = ARENA_PAGES / 64;

/// Arbitrary non-zero base standing in for a real physical address range.
pub const ARENA_BASE: u64 = 0x0010_0000;

static PHYS_BITMAP: Shared<[u64; BITMAP_WORDS]> = Shared::new([0; BITMAP_WORDS]);
static PHYS_READY: AtomicBool = AtomicBool::new(false);
static PHYS_BASE: AtomicU64 = AtomicU64::new(ARENA_BASE);
static PHYS_PAGES: AtomicUsize = AtomicUsize::new(ARENA_PAGES);

fn phys_claim(from: usize, count: usize) -> Option<u64> {
    if count == 0 || from + count > ARENA_PAGES {
        return None;
    }
    // SAFETY: single-threaded.
    let bitmap = unsafe { PHYS_BITMAP.get() };
    let base = PHYS_BASE.load(Ordering::Relaxed);
    let mut start = from;
    loop {
        if start + count > ARENA_PAGES {
            return None;
        }
        let free =
            (start..start + count).all(|page| bitmap[page / 64] & (1u64 << (page % 64)) == 0);
        if free {
            for page in start..start + count {
                bitmap[page / 64] |= 1u64 << (page % 64);
            }
            return Some(base + (start as u64) * PAGE_SIZE as u64);
        }
        start += 1;
    }
}

fn phys_release(addr: u64, count: usize) -> bool {
    let base = PHYS_BASE.load(Ordering::Relaxed);
    if addr < base {
        return false;
    }
    let start = ((addr - base) / PAGE_SIZE as u64) as usize;
    if start + count > ARENA_PAGES {
        return false;
    }
    // SAFETY: single-threaded.
    let bitmap = unsafe { PHYS_BITMAP.get() };
    for p in start..start + count {
        bitmap[p / 64] &= !(1u64 << (p % 64));
    }
    true
}

fn phys_free_count() -> usize {
    // SAFETY: single-threaded.
    let bitmap = unsafe { PHYS_BITMAP.get() };
    let used: usize = bitmap.iter().map(|w| w.count_ones() as usize).sum();
    ARENA_PAGES - used
}

fn phys_reset() {
    // SAFETY: single-threaded.
    unsafe {
        let bitmap = PHYS_BITMAP.get();
        for word in bitmap.iter_mut() {
            *word = 0;
        }
    }
    PHYS_READY.store(true, Ordering::Relaxed);
}

/// Allocate `count` physically contiguous pages, or `None`.
pub fn phys_alloc_contig(count: usize) -> Option<u64> {
    if count == 0 {
        return None;
    }
    if !PHYS_READY.load(Ordering::Relaxed) {
        phys_reset();
    }
    phys_claim(0, count)
}

pub fn phys_alloc_page() -> Option<u64> {
    phys_alloc_contig(1)
}

pub fn phys_free_contig(addr: u64, count: usize) {
    phys_release(addr, count);
}

pub fn phys_free_pages() -> usize {
    if !PHYS_READY.load(Ordering::Relaxed) {
        phys_reset();
    }
    phys_free_count()
}

pub fn phys_base() -> u64 {
    PHYS_BASE.load(Ordering::Relaxed)
}

pub fn phys_usable_size() -> u64 {
    (PHYS_PAGES.load(Ordering::Relaxed) * PAGE_SIZE) as u64
}

/// Reconfigure the arena. Ignores requests larger than the static backing
/// store, since the pages are real memory in the host process.
pub fn phys_init(base: u64, size: u64) {
    let pages = (size as usize / PAGE_SIZE).min(ARENA_PAGES);
    PHYS_BASE.store(base, Ordering::Relaxed);
    PHYS_PAGES.store(pages, Ordering::Relaxed);
    phys_reset();
}

/// Reset all simulated state. Tests call this so one test's kernel cannot leak
/// console output, clock, or page allocations into the next.
pub fn reset() {
    console_reset();
    INPUT_LEN.store(0, Ordering::Relaxed);
    CYCLES.store(0, Ordering::Relaxed);
    TSC_SWITCH.store(0, Ordering::Relaxed);
    set_profile_callback(None);
    phys_reset();
}
