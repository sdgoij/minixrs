//! Serial input ring buffer — interrupt-driven character buffering.
//!
//! The serial ISR writes characters here; `sys_read_handler` for fd=0
//! reads from here.  Single-consumer (syscall), single-producer (ISR).
//! Synchronised via atomics — the ISR runs with interrupts disabled so
//! there is no concurrency on the writer side.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Size of the ring buffer (power of two for fast masking).
const BUF_SIZE: usize = 256;
const BUF_MASK: usize = BUF_SIZE - 1;

struct SerInputBuf(UnsafeCell<[u8; BUF_SIZE]>);
unsafe impl Sync for SerInputBuf {}
impl SerInputBuf {
    const fn new(val: [u8; BUF_SIZE]) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut [u8; BUF_SIZE] {
        self.0.get()
    }
}

/// Ring buffer storage.
static BUF: SerInputBuf = SerInputBuf::new([0u8; BUF_SIZE]);

/// Write index (ISR writes here, then advances).
static WRITE_IDX: AtomicUsize = AtomicUsize::new(0);
/// Read index (syscall handler reads from here, then advances).
static READ_IDX: AtomicUsize = AtomicUsize::new(0);

/// Push a byte into the buffer.  Called from the serial ISR.
/// Drops the byte if the buffer is full.
///
/// # Safety
///
/// Must be called with interrupts disabled (from ISR context).
#[inline]
pub unsafe fn push_byte(byte: u8) {
    unsafe {
        let w = WRITE_IDX.load(Ordering::Relaxed);
        let r = READ_IDX.load(Ordering::Acquire);
        let next = (w + 1) & BUF_MASK;
        if next != r {
            let p = BUF.get().cast::<u8>();
            core::ptr::write(p.add(w), byte);
            WRITE_IDX.store(next, Ordering::Release);
        }
    }
}

/// Try to read a byte from the buffer.
/// Returns `Some(byte)` if data is available, `None` if empty.
#[inline]
pub fn try_read() -> Option<u8> {
    unsafe {
        let r = READ_IDX.load(Ordering::Relaxed);
        let w = WRITE_IDX.load(Ordering::Acquire);
        if r == w {
            return None;
        }
        let p = BUF.get().cast::<u8>();
        let byte = core::ptr::read(p.add(r));
        READ_IDX.store((r + 1) & BUF_MASK, Ordering::Release);
        Some(byte)
    }
}

/// Block until a byte is available, then return it.
/// First tries the interrupt-driven buffer; if empty, polls the COM1
/// serial port hardware directly via `hal::serial_read_byte()`.
#[inline]
pub fn read_blocking() -> u8 {
    loop {
        // Try the interrupt-driven buffer first.
        if let Some(byte) = try_read() {
            return byte;
        }
        // Fallback: poll COM1 directly via HAL.
        if crate::hal::serial_byte_available() {
            return crate::hal::serial_read_byte();
        }
        // Spin-hint to yield on hypervisors.
        crate::hal::pause();
    }
}

/// Return the number of bytes available for reading.
#[inline]
pub fn available() -> usize {
    let w = WRITE_IDX.load(Ordering::Acquire);
    let r = READ_IDX.load(Ordering::Relaxed);
    (w.wrapping_sub(r)) & BUF_MASK
}

/// Reset the buffer (for testing).
#[cfg(test)]
pub fn reset() {
    unsafe {
        let p = BUF.get().cast::<u8>();
        for i in 0..BUF_SIZE {
            core::ptr::write(p.add(i), 0);
        }
    }
    WRITE_IDX.store(0, Ordering::Relaxed);
    READ_IDX.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_try_read() {
        reset();
        assert_eq!(try_read(), None);
        unsafe { push_byte(b'a') };
        assert_eq!(try_read(), Some(b'a'));
        assert_eq!(try_read(), None);
    }

    #[test]
    fn test_multiple_bytes() {
        reset();
        for b in 0..10 {
            unsafe { push_byte(b) };
        }
        for b in 0..10 {
            assert_eq!(try_read(), Some(b));
        }
        assert_eq!(try_read(), None);
    }

    #[test]
    fn test_available() {
        reset();
        assert_eq!(available(), 0);
        unsafe { push_byte(1) };
        assert_eq!(available(), 1);
        unsafe { push_byte(2) };
        assert_eq!(available(), 2);
        let _ = try_read();
        assert_eq!(available(), 1);
    }

    #[test]
    fn test_buffer_wraparound() {
        reset();
        // Fill the buffer to capacity.
        for i in 0..(BUF_SIZE - 1) {
            unsafe { push_byte((i & 0xFF) as u8) };
        }
        // One more byte should be dropped.
        unsafe { push_byte(0xFF) };
        assert_eq!(available(), BUF_SIZE - 1);

        // Read all bytes back.
        for i in 0..(BUF_SIZE - 1) {
            assert_eq!(try_read(), Some((i & 0xFF) as u8));
        }
        assert_eq!(try_read(), None);
    }
}
