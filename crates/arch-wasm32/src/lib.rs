//! WebAssembly (wasm32) HAL.
//!
//! The host boundary is the point of this port. Everything the kernel treats as
//! a device — console, clock, and the halt/exit path — is a host import, so the
//! kernel instance holds policy while the host holds privilege.
//!
//! The parts of the HAL with no host boundary at all are re-exported from
//! `arch-sim` rather than duplicated a fourth time: paging stubs, the frame
//! layout, the VA layout, and the page arena. `ARCH_WASM32.md` §2 frames the two
//! as the same HAL, and §3 explains why the boundary is where the authority sits.
//!
//! # Imports
//!
//! Declared without a `wasm_import_module` attribute, so they land in `env` —
//! wasm-ld's default for undefined symbols — and so the crate still compiles when
//! it is built for the host as a workspace member. The harness supplies them.

#![no_std]

pub mod hal;

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

unsafe extern "C" {
    /// Append one byte to the console.
    fn host_console_write(byte: u32);
    /// Pop one console byte, or `-1` if none is pending.
    fn host_console_read() -> i32;
    /// Pending console byte count, without consuming any.
    fn host_console_available() -> i32;
    /// Monotonic cycle count.
    fn host_cycles() -> u64;
    /// Report a terminal condition and stop the instance.
    fn host_halt(code: u32);
    /// Copy `bytes` between two address spaces. `proc` is a process number, or
    /// negative for the kernel's own address space. Returns 0, or a negative
    /// errno.
    ///
    /// This is the port's page table. Two processes' address spaces are two
    /// linear memories here, and the host owns both, so a cross-process copy is
    /// not something the kernel can perform any more than a CR3 switch is
    /// something the host could (§5.1 of `ARCH_WASM32.md`).
    fn host_copy_between(
        src_proc: i32,
        src_addr: u32,
        dst_proc: i32,
        dst_addr: u32,
        bytes: u32,
    ) -> i32;
}

static TSC_SWITCH: AtomicU64 = AtomicU64::new(0);
static PROFILE_CB: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn console_write(byte: u8) {
    // SAFETY: the host supplies every import in this module at instantiation.
    unsafe { host_console_write(byte as u32) };
}

pub(crate) fn console_read() -> Option<u8> {
    // SAFETY: see `console_write`.
    let value = unsafe { host_console_read() };
    if value < 0 { None } else { Some(value as u8) }
}

pub(crate) fn console_available() -> i32 {
    // SAFETY: see `console_write`.
    unsafe { host_console_available() }
}

pub(crate) fn cycles() -> u64 {
    // SAFETY: see `console_write`.
    unsafe { host_cycles() }
}

pub(crate) fn halt_host(code: u32) {
    // SAFETY: see `console_write`.
    unsafe { host_halt(code) };
}

/// Perform a cross-process copy through the host.
///
/// Called only by [`hal::CROSS_ADDRESS_SPACE_COPY`], which `kernel::vm`
/// reaches for instead of switching page tables.
pub(crate) fn copy_between_procs(
    src_proc: i32,
    src_addr: u32,
    dst_proc: i32,
    dst_addr: u32,
    bytes: u32,
) -> i32 {
    // SAFETY: see `console_write`.
    unsafe { host_copy_between(src_proc, src_addr, dst_proc, dst_addr, bytes) }
}

pub(crate) fn tsc_switch() -> u64 {
    TSC_SWITCH.load(Ordering::Relaxed)
}

pub(crate) fn set_tsc_switch(val: u64) {
    TSC_SWITCH.store(val, Ordering::Relaxed);
}

pub(crate) fn set_profile_callback(callback: Option<unsafe extern "C" fn()>) {
    // The kernel installs this from its own thread, and nothing reads it
    // concurrently in a single-instance wasm build.
    PROFILE_CB.store(callback.map_or(0, |f| f as usize), Ordering::Relaxed);
}

pub(crate) fn run_profile_callback() {
    let raw = PROFILE_CB.load(Ordering::Relaxed);
    if raw != 0 {
        // SAFETY: only an address stored by `set_profile_callback` is converted
        // back, and its type is fixed at that call site.
        let callback: unsafe extern "C" fn() = unsafe { core::mem::transmute(raw) };
        // SAFETY: the kernel installed a function of exactly this signature.
        unsafe { callback() };
    }
}

/// Trap the instance. Reaching this means the kernel could not continue, so
/// there is nothing to fall back to.
pub(crate) fn trap() -> ! {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::unreachable()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Only reachable if this HAL were linked into a host binary, which the
        // kernel never does. Failing loudly beats returning a bogus value.
        panic!("arch-wasm32: trap on a non-wasm target")
    }
}
