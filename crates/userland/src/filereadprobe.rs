//! `/bin/filereadprobe` — hash the bytes that reads of a chosen size return.
//!
//! A file read crosses four layers (VFS's grant, MFS's block loop, the kernel's grant copy, and the
//! cache), and every earlier tool that looked at the result had its own buffering on top: `md5sum`
//! reads a whole file in one call, while `od` reads sixteen bytes per line and seeks between dumps.
//! That difference is why `KNOWN_ISSUES.md` item 60's symptom looked like a large read and a small
//! read disagreeing. This program removes the tool from the measurement: it opens a path, seeks to
//! an offset, and reads a chosen length in chunks of a chosen size, hashing every byte it gets.
//!
//! Usage: `filereadprobe <path> <offset> <length> <chunk>` — `chunk` 0 means one read for the whole
//! length. It prints what each request did (`pos` is what `lseek` answered), the byte count it
//! received, an FNV-1a 64 of those bytes, and the first eight bytes, so a caller can tell a wrong
//! count from wrong content and recognise foreign data by its prefix.

use crate::{append, write_err, write_out};

/// The largest single read the probe will issue.
const MAX_READ: usize = 64 * 1024;

struct BufCell(core::cell::UnsafeCell<[u8; MAX_READ]>);
// SAFETY: the probe is single-threaded and holds the buffer for one read at a time.
unsafe impl Sync for BufCell {}

static BUF: BufCell = BufCell(core::cell::UnsafeCell::new([0u8; MAX_READ]));

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(sum: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *sum ^= b as u64;
        *sum = sum.wrapping_mul(FNV_PRIME);
    }
}

fn hex(bytes: &[u8], out: &mut [u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in bytes.iter().enumerate() {
        out[i * 2] = DIGITS[(b >> 4) as usize];
        out[i * 2 + 1] = DIGITS[(b & 0xf) as usize];
    }
}

/// A field `name=value` for a decimal number.
fn named(line: &mut [u8], at: &mut usize, name: &[u8], value: i64) {
    append(line, at, name);
    if value < 0 {
        append(line, at, b"-");
    }
    append(line, at, crate::Decimal::of(value.unsigned_abs() as u32).bytes());
}

/// Parse a non-negative decimal `u64`, or `None`.
fn number(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    let mut n: u64 = 0;
    for b in text.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(n)
}

fn fail(what: &[u8], errno: i32) -> i32 {
    write_err(what);
    write_err(crate::Decimal::of(errno.unsigned_abs()).bytes());
    write_err(b"\n");
    1
}

/// Read `length` bytes at `offset`, in reads of at most `chunk` bytes, and report what arrived.
pub fn filereadprobe(args: &[&str]) -> i32 {
    if args.len() != 5 {
        write_err(b"usage: filereadprobe <path> <offset> <length> <chunk>\n");
        return 1;
    }
    let (offset, length, chunk) = match (
        number(args[2]),
        number(args[3]),
        number(args[4]),
    ) {
        (Some(o), Some(l), Some(c)) => (o, l, c as usize),
        _ => {
            write_err(b"filereadprobe: arguments must be decimal numbers\n");
            return 1;
        }
    };

    let fd = match unsafe {
        minix_std::fs::open(args[1].as_bytes(), minix_std::fs::O_RDONLY, 0)
    } {
        Ok(fd) => fd,
        Err(e) => return fail(b"filereadprobe: open failed, errno ", e.0),
    };

    let pos = match minix_std::fs::lseek(fd, offset as i64, minix_std::fs::SEEK_SET) {
        Ok(pos) => pos,
        Err(e) => {
            let _ = minix_std::fs::close(fd);
            return fail(b"filereadprobe: lseek failed, errno ", e.0);
        }
    };

    let buf = unsafe { &mut *BUF.0.get() };
    let mut sum = FNV_OFFSET;
    let mut got: usize = 0;
    let mut first = [0u8; 8];
    let mut first_len: usize = 0;
    let mut errno: i32 = 0;

    while got < length as usize {
        let want = if chunk == 0 {
            length as usize - got
        } else {
            chunk.min(length as usize - got)
        }
        .min(MAX_READ);
        let n = minix_rt::read(fd, &mut buf[..want]);
        if n < 0 {
            errno = n as i32;
            break;
        }
        if n == 0 {
            break;
        }
        let n = n as usize;
        fnv1a(&mut sum, &buf[..n]);
        if first_len < first.len() {
            let take = (first.len() - first_len).min(n);
            first[first_len..first_len + take].copy_from_slice(&buf[..take]);
            first_len += take;
        }
        got += n;
    }
    let _ = minix_std::fs::close(fd);

    let mut line = [0u8; 192];
    let mut at = 0usize;
    append(&mut line, &mut at, b"filereadprobe:");
    named(&mut line, &mut at, b" pos=", pos);
    named(&mut line, &mut at, b" off=", offset as i64);
    named(&mut line, &mut at, b" len=", length as i64);
    named(&mut line, &mut at, b" chunk=", chunk as i64);
    named(&mut line, &mut at, b" got=", got as i64);
    append(&mut line, &mut at, b" sum=");
    let mut digits = [0u8; 16];
    hex(&sum.to_be_bytes(), &mut digits);
    append(&mut line, &mut at, &digits);
    append(&mut line, &mut at, b" first=");
    let mut first_hex = [0u8; 16];
    hex(&first, &mut first_hex);
    append(&mut line, &mut at, &first_hex);
    if errno != 0 {
        named(&mut line, &mut at, b" errno=", errno as i64);
    }
    append(&mut line, &mut at, b"\n");
    write_out(&line[..at]);
    if errno != 0 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_matches_the_reference_vectors() {
        let mut sum = FNV_OFFSET;
        fnv1a(&mut sum, b"");
        assert_eq!(sum, 0xcbf2_9ce4_8422_2325);
        let mut sum = FNV_OFFSET;
        fnv1a(&mut sum, b"a");
        assert_eq!(sum, 0xaf63dc4c8601ec8c);
        let mut sum = FNV_OFFSET;
        fnv1a(&mut sum, b"foobar");
        assert_eq!(sum, 0x85944171f73967e8);
    }

    #[test]
    fn a_number_is_decimal_and_bounded() {
        assert_eq!(number("4096"), Some(4096));
        assert_eq!(number("0"), Some(0));
        assert_eq!(number(""), None);
        assert_eq!(number("-1"), None);
        assert_eq!(number("1x"), None);
    }
}
