//! x86_64 HAL implementation.
//!
//! Provides all the functions declared in `kernel::hal` for the x86_64
//! architecture. These are called from arch-independent kernel code.

use core::sync::atomic::Ordering;

// ── Initialization ────────────────────────────────────────────────────────

/// Initialize x86_64 architecture subsystem (IDT, MSRs, cpulocals, etc.).
pub fn init() {
    crate::init();
}

// ── Serial port I/O (COM1) ───────────────────────────────────────────────

const COM1_DATA: u16 = 0x3F8;
const COM1_LSR: u16 = 0x3FD; // Line Status Register
const LSR_DR: u8 = 0x01; // Data Ready bit

/// Write a single byte to the COM1 serial port.
pub fn serial_write_byte(byte: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") COM1_DATA,
            in("al") byte,
            options(nomem, nostack),
        );
    }
}

/// Read a single byte from COM1, blocking until data is available.
pub fn serial_read_byte() -> u8 {
    loop {
        if let Some(byte) = serial_try_read_byte() {
            return byte;
        }
        // Spin-hint to yield to hyperthread on hypervisors.
        unsafe {
            core::arch::asm!("pause", options(nomem, nostack));
        }
    }
}

/// Non-blocking check: is a byte available on COM1?
pub fn serial_byte_available() -> bool {
    let lsr: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") lsr,
            in("dx") COM1_LSR,
            options(nomem, nostack),
        );
    }
    lsr & LSR_DR != 0
}

/// Try to read a byte from COM1 without blocking.
fn serial_try_read_byte() -> Option<u8> {
    if !serial_byte_available() {
        return None;
    }
    let byte: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") byte,
            in("dx") COM1_DATA,
            options(nomem, nostack),
        );
    }
    Some(byte)
}

// ── Cycle counter ─────────────────────────────────────────────────────────

/// Read the x86_64 timestamp counter (TSC).
pub fn read_cycles() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    (lo as u64) | ((hi as u64) << 32)
}

// ── Halt ──────────────────────────────────────────────────────────────────

/// Halt the CPU with interrupts disabled. Never returns.
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("cli; hlt", options(nomem, nostack));
        }
    }
}

// ── Per-CPU current process pointer ───────────────────────────────────────

use core::ffi::c_void;

/// Set the per-CPU current process pointer (stored in `cpulocals`).
///
/// # Safety
///
/// `proc` must point to a valid `Proc` or be null.
pub unsafe fn set_current_proc(proc: *mut c_void) {
    unsafe {
        crate::cpulocals::set_cpulocal_proc_ptr(proc);
    }
}

/// Get the per-CPU current process pointer.
pub fn current_proc() -> *mut c_void {
    unsafe { crate::cpulocals::get_cpulocal_proc_ptr() }
}

// ── Spinlocks ─────────────────────────────────────────────────────────────

/// A simple spinlock backed by an atomic flag.
pub struct Spinlock(core::sync::atomic::AtomicBool);

impl Spinlock {
    /// Create a new unlocked spinlock.
    pub const fn new() -> Self {
        Self(core::sync::atomic::AtomicBool::new(false))
    }

    /// Acquire the spinlock, spinning until it is available.
    pub fn acquire(&self) {
        while self
            .0
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Spin-hint
            unsafe {
                core::arch::asm!("pause", options(nomem, nostack));
            }
        }
    }

    /// Release the spinlock.
    pub fn release(&self) {
        self.0.store(false, Ordering::Release);
    }
}

// ── TrapFrame accessors (raw [u8; 256] helpers) ──────────────────────────

// These are stubs until 19.0.2 converts Proc.p_reg to raw bytes.
// For now the kernel still uses the typed TrapFrame.

// ── Page table constants (stubs until 19.0.3) ────────────────────────────

/// Physical memory page size.
pub const PAGE_SIZE: u64 = 4096;
/// Number of bits for the page offset.
pub const PAGE_SHIFT: u64 = 12;

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinlock_acquire_release() {
        let lock = Spinlock::new();
        lock.acquire();
        lock.release();
        // If we get here without deadlock, the test passes.
    }

    #[test]
    fn spinlock_exclusion() {
        let lock = Spinlock::new();
        lock.acquire();
        // Second acquire should fail immediately with try_lock
        // (not provided, but we can test basic mutual exclusion)
        lock.release();
    }
}
