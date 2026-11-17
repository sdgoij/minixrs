//! `/bin/procfsprobe` — reads the two files procfs serves from the kernel clock.
//!
//! `KNOWN_ISSUES.md` item 7 wired the procfs server: a boot process of its own, an
//! initramfs entry, and a mount at `/proc` in VFS. Nothing else in the tree opens a
//! path under a *user-mounted* VTreeFS, so without this client a mount that never
//! crossed the boundary would look exactly like one that did: the server's own unit
//! tests drive `handle_fs_message` directly and never involve VFS or MFS.
//!
//! The fields are the three things a mount has to get right and one it must refuse:
//! `hz` is `/proc/hz`, a positive decimal from `SYS_TIMES`; `uptime` is
//! `/proc/uptime`, `<seconds>.<hundredths>`; `dir` is `/proc` itself, which has to
//! resolve to a directory; and `gone` is a path under `/proc` that is not there, so
//! the mount answers from its own tree instead of silently falling back to MFS.

use crate::{Decimal, append, write_err, write_out};
use minix_std::fs::O_RDONLY;

/// The largest output either root file produces today is a handful of digits.
const BUF: usize = 32;

/// Decimal digits up to a trailing newline. `None` for empty input or a stray byte,
/// so a file that printed nothing does not parse as zero.
fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    let digits = match bytes {
        [rest @ .., b'\n'] => rest,
        rest => rest,
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(v)
}

/// `<seconds>.<hundredths>` up to a trailing newline, the shape `root_uptime` prints.
fn is_uptime(bytes: &[u8]) -> bool {
    let body = match bytes {
        [rest @ .., b'\n'] => rest,
        rest => rest,
    };
    let mut parts = body.split(|&b| b == b'.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(secs), Some(hundredths), None) => {
            !secs.is_empty()
                && !hundredths.is_empty()
                && secs.iter().all(u8::is_ascii_digit)
                && hundredths.iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

/// Open `path`, read it whole, and hand the bytes to `check`.
fn read_and(path: &[u8], check: impl Fn(&[u8]) -> bool) -> bool {
    let fd = match unsafe { minix_std::fs::open(path, O_RDONLY, 0) } {
        Ok(fd) => fd,
        Err(_) => return false,
    };
    let mut buf = [0u8; BUF];
    let read = unsafe { minix_std::fs::read(fd, &mut buf) };
    let closed = minix_std::fs::close(fd);
    match (read, closed) {
        (Ok(n), Ok(())) => check(&buf[..n.max(0) as usize]),
        _ => false,
    }
}

/// Report a failure that leaves the rest of the line unwritable, with the errno.
fn fail(what: &[u8], errno: i32) -> i32 {
    write_err(what);
    write_err(Decimal::of(errno as u32).bytes());
    write_err(b"\n");
    1
}

/// A `name=ok`/`name=bad` field.
fn verdict(line: &mut [u8], at: &mut usize, name: &[u8], good: bool) {
    append(line, at, name);
    append(line, at, if good { b"=ok" } else { b"=bad" });
}

pub fn procfsprobe(_args: &[&str]) -> i32 {
    let hz = read_and(b"/proc/hz", |b| parse_decimal(b).is_some_and(|v| v > 0));
    let uptime = read_and(b"/proc/uptime", is_uptime);

    // `/proc` itself resolves through the mount, so its mode is the mount point's.
    let dir = match minix_std::fs::stat_bytes(b"/proc") {
        Ok(st) => st.st_mode & 0o170000 == 0o040000,
        Err(_) => false,
    };

    // A path that is not in procfs must be refused with `ENOENT` from the mounted
    // tree, not by falling through to the root filesystem: this is the line that
    // says VFS split the path at `/proc` and asked procfs for the rest.
    let gone = match unsafe { minix_std::fs::open(b"/proc/no-such-file", O_RDONLY, 0) } {
        Ok(fd) => {
            let _ = minix_std::fs::close(fd);
            None
        }
        Err(e) => Some(e.0),
    };
    if gone != Some(2) {
        return fail(
            b"procfsprobe: /proc/no-such-file answered errno ",
            gone.unwrap_or(0),
        );
    }

    let mut line = [0u8; 96];
    let mut at = 0;
    append(&mut line, &mut at, b"procfs: ");
    verdict(&mut line, &mut at, b"hz", hz);
    append(&mut line, &mut at, b" ");
    verdict(&mut line, &mut at, b"uptime", uptime);
    append(&mut line, &mut at, b" ");
    verdict(&mut line, &mut at, b"dir", dir);
    append(&mut line, &mut at, b" gone=err 2\n");
    write_out(&line[..at]);

    if hz && uptime && dir { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_needs_digits() {
        assert_eq!(parse_decimal(b"60\n"), Some(60));
        assert_eq!(parse_decimal(b"100"), Some(100));
        assert_eq!(parse_decimal(b"\n"), None);
        assert_eq!(parse_decimal(b"6 0\n"), None);
        assert_eq!(parse_decimal(b"-1\n"), None);
    }

    #[test]
    fn uptime_is_two_digit_groups() {
        assert!(is_uptime(b"3.45\n"));
        assert!(is_uptime(b"0.00\n"));
        assert!(is_uptime(b"12.5"), "the fraction is not padded to two");
        assert!(!is_uptime(b"3\n"));
        assert!(!is_uptime(b".45\n"));
        assert!(!is_uptime(b"3.\n"));
        assert!(!is_uptime(b"3.45.1\n"));
        assert!(!is_uptime(b"3.x5\n"));
    }
}
