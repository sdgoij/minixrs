//! RISC-V64 CLINT timer — using SBI timer extension.
//!
//! The CLINT (Core-Local Interrupt Controller) provides:
//! - `mtime` at offset 0xBFF8: 64-bit free-running counter
//! - `mtimecmp` per hart: S-mode can't write this (M-mode only)
//! - Timer interrupts via SBI: `sbi_set_timer(stime_value)`
//!
//! QEMU virt CLINT base: 0x02000000, timebase ~10 MHz (100ns/tick)

#![cfg(target_arch = "riscv64")]

use core::sync::atomic::{AtomicU64, Ordering};

/// CLINT memory-mapped registers (M-mode only, listed for reference).
/// In S-mode, we use the SBI timer extension instead.
pub const CLINT_BASE: u64 = 0x02000000;
/// mtime register offset from CLINT base.
pub const MTIME_OFFSET: u64 = 0xBFF8;
/// mtimecmp per hart offset (hart 0).
pub const MTIMECMP_HART0: u64 = 0x4000;

/// Default timer interval in ticks (100 Hz @ 10 MHz = 100,000 ticks).
pub const DEFAULT_TIMER_INTERVAL: u64 = 100_000;

/// Read the current time from the `time` CSR (RISC-V `rdtime` instruction).
///
/// The `time` CSR is a read-only view of the platform's `mtime` register.
/// On QEMU virt, the timebase frequency is typically 10 MHz.
pub fn read_time() -> u64 {
    let time: u64;
    unsafe {
        core::arch::asm!("rdtime {time}", time = out(reg) time, options(nomem, nostack));
    }
    time
}

/// Initialize the timer by scheduling the first tick.
///
/// Sets up a periodic timer at the given `interval_hz` frequency.
/// Returns the actual interval in ticks that was programmed.
///
/// # Safety
///
/// Must be called once during boot, with interrupts disabled.
pub unsafe fn init_timer(interval_hz: u64) -> u64 {
    let timebase_hz = estimate_timebase();
    let interval_ticks = timebase_hz / interval_hz;
    let now = read_time();
    let next = now + interval_ticks;
    crate::sbi::set_timer(next);
    NEXT_INTERVAL.store(interval_ticks, Ordering::Relaxed);
    interval_ticks
}

/// Handle a timer interrupt — schedule the next tick.
///
/// Must be called from the trap handler on `SUP_TIMER_INTR`.
///
/// # Safety
///
/// Must be called with interrupts disabled, from the timer ISR context.
pub unsafe fn handle_timer_interrupt() {
    let now = read_time();
    let interval = NEXT_INTERVAL.load(Ordering::Relaxed);
    if interval > 0 {
        crate::sbi::set_timer(now + interval);
    }
}

/// Stored timer interval for periodic scheduling.
static NEXT_INTERVAL: AtomicU64 = AtomicU64::new(0);

/// Estimate the timebase frequency by reading mtime twice.
/// On QEMU virt, this is typically 10 MHz.
fn estimate_timebase() -> u64 {
    // For now, hardcode 10 MHz (QEMU virt default).
    // TODO: Read from FDT `/cpus/timebase-frequency` property.
    10_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_time_returns_value() {
        // rdtime should return a non-zero value on real hardware.
        // In tests (host build), this function is cfg-gated so won't compile.
    }

    #[test]
    fn test_clint_constants() {
        assert_eq!(CLINT_BASE, 0x02000000);
        assert_eq!(MTIME_OFFSET, 0xBFF8);
        assert_eq!(MTIMECMP_HART0, 0x4000);
        assert_eq!(DEFAULT_TIMER_INTERVAL, 100_000);
    }
}
