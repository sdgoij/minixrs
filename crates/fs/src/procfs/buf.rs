//! ProcFS output buffer — adapted from `minix/fs/procfs/buf.c`
//!
//! Provides a static 4096-byte buffer with skip/limit semantics.
//! All formatting goes through `core::fmt::Write` to remain `no_std`-compatible.

use core::fmt;

pub const BUF_SIZE: usize = 4096;

static mut BUF: [u8; BUF_SIZE] = [0; BUF_SIZE];
static mut OFF: usize = 0; // offset into BUF where data starts (after skip)
static mut USED: usize = 0; // bytes of useful data written
static mut LEFT: usize = 0; // remaining writable capacity
static mut SKIP: u64 = 0; // bytes to skip before recording

/// Initialize the buffer for fresh use.
///
/// The first `start` bytes of produced output are skipped. After that,
/// at most `len` bytes are retained.
pub fn buf_init(start: u64, len: usize) {
    unsafe {
        SKIP = start;
        LEFT = len.min(BUF_SIZE);
        OFF = 0;
        USED = 0;
    }
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
    unsafe {
        if LEFT == 0 {
            return;
        }

        let mut data = data;
        let mut len = data.len();

        if SKIP > 0 {
            let skip = SKIP as usize;
            if skip >= len {
                SKIP -= len as u64;
                return;
            }
            data = &data[skip..];
            len -= skip;
            SKIP = 0;
        }

        if len > LEFT {
            len = LEFT;
        }

        let dst = &mut BUF[OFF + USED..][..len];
        dst.copy_from_slice(&data[..len]);

        USED += len;
        LEFT -= len;
    }
}

/// Return a pointer to the used portion of the buffer and its length.
pub fn buf_get() -> (&'static [u8], usize) {
    unsafe { (&BUF[OFF..][..USED], USED) }
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
