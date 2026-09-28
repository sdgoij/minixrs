//! `/bin/readlinktest` — `readlink(2)` on a link the image ships.
//!
//! A symlink is the shape `/sys` is made of, and reading one is the only thing in this image
//! that moves a *filesystem server's* bytes into a user process: the target is copied out of
//! the inode's first block through a grant VFS made for the caller's own buffer. Nothing else
//! exercised that path — `readlink` was an `ENOSYS` stub in libc, VFS asked the filesystem to
//! copy into nothing, and MFS never copied at all — so this client is the assertion that the
//! three pieces now meet.
//!
//! The fields are the three ways a read can be wrong and the two ways it must be refused:
//! `n` is the count, `tgt` is whether the bytes are the link's target, `cut`/`cutok` are the
//! truncation POSIX requires when the buffer is shorter than the target (the count becomes the
//! buffer's length, not the target's), `chr` is a character device — which is not a link, and
//! the errno says the check happened before any filesystem was asked — and `gone` is a path
//! that is not there.

use crate::{Decimal, append, write_err, write_out};

/// The link the image ships and the target it was made with, both from
/// `boot-image/src/minixfs.rs`: a disagreement between these two statements is the thing this
/// client exists to notice.
const LINK: &[u8] = b"/link";
const TARGET: &[u8] = b"/sys/devices/virtio-gpu";
/// A node that exists and is not a symlink.
const NOT_A_LINK: &[u8] = b"/dev/dri/renderD128";
const MISSING: &[u8] = b"/no-such-link";
/// Shorter than the target, so the truncated read is a real truncation.
const SHORT: usize = 8;

/// Report a failure that leaves the rest of the line unwritable, with the errno.
fn fail(what: &[u8], errno: i32) -> i32 {
    write_err(what);
    write_err(Decimal::of(errno as u32).bytes());
    write_err(b"\n");
    1
}

/// A `name=value` field.
fn named(line: &mut [u8], at: &mut usize, name: &[u8], value: usize) {
    append(line, at, name);
    append(line, at, Decimal::of(value as u32).bytes());
}

/// A field that is `ok` when the request was *refused* and the errno otherwise — so a call
/// that unexpectedly succeeded reads as `ok` rather than as another refusal.
fn refused(line: &mut [u8], at: &mut usize, label: &[u8], errno: Option<i32>) {
    append(line, at, label);
    match errno {
        Some(e) => {
            append(line, at, b"err ");
            append(line, at, Decimal::of(e as u32).bytes());
        }
        None => append(line, at, b"ok"),
    }
}

/// Read the shipped link four ways and report what each said.
pub fn readlinktest(_args: &[&str]) -> i32 {
    let mut buf = [0u8; 64];

    let n = match minix_std::fs::readlink(LINK, &mut buf) {
        Ok(n) => n,
        Err(e) => return fail(b"readlinktest: readlink failed, errno ", e.0),
    };
    let tgt = n == TARGET.len() && buf[..n] == *TARGET;

    // POSIX truncates rather than failing, and reports the *buffer's* length.
    let mut short = [0u8; SHORT];
    let cut = match minix_std::fs::readlink(LINK, &mut short) {
        Ok(n) => n,
        Err(e) => return fail(b"readlinktest: short readlink failed, errno ", e.0),
    };
    let cutok = cut == SHORT && short[..] == TARGET[..SHORT];

    // The two refusals. A device node is not a link, so VFS answers before any filesystem is
    // asked; a path that is not there is the filesystem's own `ENOENT`.
    let chr = minix_std::fs::readlink(NOT_A_LINK, &mut buf)
        .err()
        .map(|e| e.0);
    let gone = minix_std::fs::readlink(MISSING, &mut buf)
        .err()
        .map(|e| e.0);

    let mut line = [0u8; 160];
    let mut at = 0usize;
    append(&mut line, &mut at, b"readlinktest:");
    named(&mut line, &mut at, b" n=", n);
    append(
        &mut line,
        &mut at,
        if tgt { b" tgt=ok" } else { b" tgt=bad" },
    );
    named(&mut line, &mut at, b" cut=", cut);
    append(
        &mut line,
        &mut at,
        if cutok { b" cutok=1" } else { b" cutok=0" },
    );
    refused(&mut line, &mut at, b" chr=", chr);
    refused(&mut line, &mut at, b" gone=", gone);
    append(&mut line, &mut at, b"\n");
    write_out(&line[..at]);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The target is longer than the short buffer, so the truncated read is a truncation and
    /// the two counts differ: a test where they agreed would prove neither.
    #[test]
    fn the_short_read_is_shorter_than_the_target() {
        assert!(SHORT < TARGET.len(), "the cut has to cut something");
        assert!(
            TARGET.len() <= 64,
            "the long buffer has to hold the whole target"
        );
    }

    /// The three paths this client names are distinct, and the link's own path is not one of the
    /// refusals — a step whose expectation could be met by another step's output is the trap the
    /// scenarios' anchoring exists for.
    #[test]
    fn the_four_paths_are_distinct() {
        assert_ne!(LINK, NOT_A_LINK);
        assert_ne!(LINK, MISSING);
        assert_ne!(NOT_A_LINK, MISSING);
    }

    /// A field's rendering: `refused` writes `ok` only for a call that was *not* refused, so an
    /// unexpected success cannot read as a matching line.
    #[test]
    fn a_refusal_field_writes_ok_only_for_a_success() {
        let mut line = [0u8; 32];
        let mut at = 0usize;
        refused(&mut line, &mut at, b" chr=", Some(22));
        refused(&mut line, &mut at, b" gone=", None);
        assert_eq!(&line[..at], b" chr=err 22 gone=ok");
    }
}
