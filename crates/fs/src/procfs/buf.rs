//! ProcFS output buffer — adapted from `minix/fs/procfs/buf.c`
//!
//! Provides a static 4096-byte buffer with skip/limit semantics.
//! All formatting goes through `core::fmt::Write` to remain `no_std`-compatible.

use core::cell::UnsafeCell;
use core::fmt;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub const BUF_SIZE: usize = 4096;

struct ProcfsBufCell(UnsafeCell<[u8; BUF_SIZE]>);
unsafe impl Sync for ProcfsBufCell {}
impl ProcfsBufCell {
    const fn new(val: [u8; BUF_SIZE]) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut [u8; BUF_SIZE] {
        self.0.get()
    }
}

static BUF: ProcfsBufCell = ProcfsBufCell::new([0; BUF_SIZE]);
static OFF: AtomicUsize = AtomicUsize::new(0); // offset into BUF where data starts (after skip)
static USED: AtomicUsize = AtomicUsize::new(0); // bytes of useful data written
static LEFT: AtomicUsize = AtomicUsize::new(0); // remaining writable capacity
static SKIP: AtomicU64 = AtomicU64::new(0); // bytes to skip before recording

/// Initialize the buffer for fresh use.
///
/// The first `start` bytes of produced output are skipped. After that,
/// at most `len` bytes are retained.
pub fn buf_init(start: u64, len: usize) {
    SKIP.store(start, Ordering::Relaxed);
    LEFT.store(len.min(BUF_SIZE), Ordering::Relaxed);
    OFF.store(0, Ordering::Relaxed);
    USED.store(0, Ordering::Relaxed);
}

/// Append a plain string to the buffer.
pub fn buf_write(s: &str) {
    buf_append(s.as_bytes());
}

/// Append formatted output using `core::fmt::Arguments`.
pub fn buf_write_fmt(args: fmt::Arguments<'_>) {
    use fmt::Write;
    let _ = BufWriter.write_fmt(args);
}

/// Append raw bytes to the buffer.
pub fn buf_append(data: &[u8]) {
    let left = LEFT.load(Ordering::Relaxed);
    if left == 0 {
        return;
    }

    let mut data = data;
    let mut len = data.len();

    let skip = SKIP.load(Ordering::Relaxed);
    if skip > 0 {
        let skip_usize = skip as usize;
        if skip_usize >= len {
            SKIP.store(skip - len as u64, Ordering::Relaxed);
            return;
        }
        data = &data[skip_usize..];
        len -= skip_usize;
        SKIP.store(0, Ordering::Relaxed);
    }

    if len > left {
        len = left;
    }

    let off = OFF.load(Ordering::Relaxed);
    let used = USED.load(Ordering::Relaxed);
    let dst =
        unsafe { core::slice::from_raw_parts_mut(BUF.get().cast::<u8>().add(off + used), len) };
    dst.copy_from_slice(&data[..len]);

    USED.store(used + len, Ordering::Relaxed);
    LEFT.store(left - len, Ordering::Relaxed);
}

/// Return a pointer to the used portion of the buffer and its length.
pub fn buf_get() -> (&'static [u8], usize) {
    let off = OFF.load(Ordering::Relaxed);
    let used = USED.load(Ordering::Relaxed);
    unsafe {
        (
            core::slice::from_raw_parts(BUF.get().cast::<u8>().add(off), used),
            used,
        )
    }
}

struct BufWriter;

impl fmt::Write for BufWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        buf_write(s);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buf_write_and_get() {
        buf_init(0, 100);
        buf_write("hello ");
        buf_write("world");
        let (data, len) = buf_get();
        assert_eq!(len, 11);
        assert_eq!(core::str::from_utf8(&data[..len]).unwrap(), "hello world");
    }

    #[test]
    fn buf_write_fmt_works() {
        buf_init(0, 64);
        buf_write_fmt(format_args!("{} + {} = {}", 1, 2, 3));
        let (data, len) = buf_get();
        assert_eq!(core::str::from_utf8(&data[..len]).unwrap(), "1 + 2 = 3");
    }

    #[test]
    fn buf_append_raw() {
        buf_init(0, 64);
        buf_append(b"\x00\x01\x02");
        let (data, len) = buf_get();
        assert_eq!(len, 3);
        assert_eq!(&data[..len], b"\x00\x01\x02");
    }

    #[test]
    fn buf_skip() {
        buf_init(5, 64);
        buf_write("0123456789");
        let (data, len) = buf_get();
        assert_eq!(&data[..len], b"56789");
    }

    #[test]
    fn buf_limit() {
        buf_init(0, 4);
        buf_write("12345678");
        let (_, len) = buf_get();
        assert_eq!(len, 4);
    }

    #[test]
    fn buf_no_panic_on_empty() {
        buf_init(0, 0);
        buf_write("anything");
        let (_, len) = buf_get();
        assert_eq!(len, 0);
    }
}
