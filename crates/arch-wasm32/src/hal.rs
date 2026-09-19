//! The kernel's HAL surface for wasm32.
//!
//! The glob below brings in every item that has no host boundary — paging stubs,
//! frame accessors, the VA layout, the page arena, CPU and scheduler statics.
//! The explicit definitions that follow *shadow* it for the pieces that do cross
//! into the host, which is why there is no ambiguity to resolve: in Rust an
//! explicit item always wins over a glob import.

pub use arch_sim::hal::*;

use crate::{
    console_available, console_read, console_write, cycles, halt_host, run_profile_callback,
    set_profile_callback, set_tsc_switch, trap, tsc_switch,
};

pub fn init() {
    arch_sim::hal::init();
}

// ------------------------------------------------------------------ console

pub fn serial_write_byte(byte: u8) {
    console_write(byte);
}

pub fn serial_byte_available() -> bool {
    console_available() > 0
}

pub fn poll_console() -> Option<u8> {
    console_read()
}

/// Blocking read. Bounded rather than unbounded: with no timer interrupt there is
/// nothing to wake a wait, so a missing byte would hang the host rather than fail
/// it. Panicking names the problem instead.
pub fn serial_read_byte() -> u8 {
    for _ in 0..1_000_000 {
        if let Some(byte) = console_read() {
            return byte;
        }
    }
    panic!("arch-wasm32: serial_read_byte found no input (nothing will supply one)");
}

// --------------------------------------------------------------------- clock

/// The host is the clock source: there is no TSC, and no timer interrupt to
/// sample it from.
pub fn read_cycles() -> u64 {
    cycles()
}

pub fn read_tsc() -> u64 {
    cycles()
}

/// # Safety
///
/// No preconditions; `unsafe` only to match the other ports' signatures.
pub unsafe fn read_tsc_ctr_switch() -> u64 {
    tsc_switch()
}

/// # Safety
///
/// No preconditions; see `read_tsc_ctr_switch`.
pub unsafe fn write_tsc_ctr_switch(val: u64) {
    set_tsc_switch(val);
}

/// # Safety
///
/// `callback` must be callable with no arguments.
pub unsafe fn init_profile_clock(_rate_code: u32, callback: unsafe extern "C" fn()) -> i32 {
    set_profile_callback(Some(callback));
    0
}

pub fn stop_profile_clock() {
    set_profile_callback(None);
}

/// Run the profiler callback, if one is installed. Sampling has to be driven
/// explicitly because there is no timer interrupt to drive it.
pub fn profile_tick() {
    run_profile_callback();
}

// -------------------------------------------------------------- stop paths

/// Idling does not need to advance anything: the clock is the host's, and the
/// host is what resumes an instance.
pub fn cpu_idle() {}

pub fn pause() {}

pub fn hlt() {}

pub fn halt() -> ! {
    halt_host(0);
    trap()
}

pub fn qemu_exit(code: u32) -> ! {
    halt_host(code);
    trap()
}
