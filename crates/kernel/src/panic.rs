//! Kernel panic handler with diagnostic output.
//!
//! Provides `handle()` — a function called by the `#[panic_handler]` in
//! the `kernel-boot` binary. Writes diagnostic information to the serial
//! console, then halts the CPU.
//!
//! Mirrors the C `panic()` in `minix/kernel/utility.c`.

use core::sync::atomic::{AtomicBool, Ordering};

static PANICKING: AtomicBool = AtomicBool::new(false);
static CPU_LOCALS_READY: AtomicBool = AtomicBool::new(false);

/// Called by kernel-boot's kmain after `init_cpulocals()` to mark per-CPU
/// data as safe to access. Without this, the panic handler must skip
/// `hal::current_proc()` because GS-relative reads return garbage before
/// the per-CPU data area is initialized.
pub fn mark_cpulocals_ready() {
    CPU_LOCALS_READY.store(true, Ordering::Relaxed);
}

/// Called by the `#[panic_handler]` in `kernel-boot`.
///
/// Outputs the panic message, source location, current process info,
/// stack trace, and KMESSAGES buffer to the serial console, then
/// halts the CPU.
pub fn handle(info: &core::panic::PanicInfo) -> ! {
    // Re-entrancy guard: if serial output itself triggers a panic
    // (e.g. corrupted UART MMIO), halt immediately.
    if PANICKING.swap(true, Ordering::SeqCst) {
        crate::hal::halt()
    }

    // Write all output byte-by-byte via hal::serial_write_byte().
    // No buffering, no allocation — safe even in corrupted kernel state.

    // Phase 1: banner.
    ser_write(b"KERNEL PANIC: ");

    // Phase 2: panic message.
    if let Some(s) = info.message().as_str() {
        ser_write(s.as_bytes());
    } else {
        ser_write(b"<non-string panic message>");
    }
    ser_byte(b'\n');

    // Phase 3: source location.
    if let Some(loc) = info.location() {
        ser_write(b"  at ");
        ser_write(loc.file().as_bytes());
        ser_byte(b':');
        ser_u64(loc.line() as u64);
        ser_byte(b'\n');
    }

    // Phase 4: current process info + stack trace.
    if CPU_LOCALS_READY.load(Ordering::Relaxed) {
        let proc = crate::hal::current_proc() as *const crate::proc::Proc;
        if !proc.is_null() {
            ser_write(b"  current process: ");
            unsafe {
                let name_ptr = &raw const (*proc).p_name as *const u8;
                for i in 0..15 {
                    let c = *name_ptr.add(i);
                    if c == 0 {
                        break;
                    }
                    ser_byte(c);
                }
            }
            ser_byte(b'\n');
            // Stack trace — writes to KMESSAGES buffer (best-effort).
            unsafe {
                crate::debug::proc_stacktrace(proc);
            }
        } else {
            ser_write(b"  (no current process)\n");
        }
    } else {
        ser_write(b"  (early boot - no process info)\n");
    }

    // Phase 5: drain KMESSAGES buffer to serial.
    unsafe {
        let km = crate::glo::KMESSAGES.get();
        let size = (*km).km_size.max(0) as usize;
        let size = size.min(arch_common::sys_config::KMESS_BUF_SIZE);
        let km_buf = &raw const (*km).km_buf as *const u8;
        for i in 0..size {
            ser_byte(core::ptr::read(km_buf.add(i)));
        }
    }

    ser_write(b"\n--- halt ---\n");
    crate::hal::halt()
}

// ── Serial output helpers ──

/// Write a byte slice directly to serial (no allocation, no formatting).
fn ser_write(bytes: &[u8]) {
    for &b in bytes {
        crate::hal::serial_write_byte(b);
    }
}

/// Write a single byte to serial.
fn ser_byte(byte: u8) {
    crate::hal::serial_write_byte(byte);
}

/// Format a u64 as decimal digits and write to serial.
fn ser_u64(n: u64) {
    if n == 0 {
        ser_byte(b'0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut i = 20;
    let mut v = n;
    while v > 0 {
        i -= 1;
        digits[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    for &d in &digits[i..] {
        ser_byte(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initially_not_ready() {
        assert!(!CPU_LOCALS_READY.load(Ordering::Relaxed));
    }

    #[test]
    fn test_mark_ready() {
        mark_cpulocals_ready();
        assert!(CPU_LOCALS_READY.load(Ordering::Relaxed));
        CPU_LOCALS_READY.store(false, Ordering::Relaxed);
    }

    #[test]
    fn test_panicking_initially_false() {
        assert!(!PANICKING.load(Ordering::Relaxed));
    }
}
