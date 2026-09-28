//! `/bin/symlinktest` — `symlink(2)` and `readlink(2)` on what it made.
//!
//! `symlink` is the writing half of the `readlink` gate: MFS puts the target in the
//! inode's first block and `fs_rdlink` copies it back out through a grant, so a link
//! created here and read back exercises both directions at once. The image's `/link` is
//! written by the image builder; this one is made at run time by the filesystem itself.
//!
//! The target is never resolved — it is a string — so the only things that can refuse a
//! creation are the length checks and the link's own path. The fields are the ones that can
//! be wrong: `n`/`tgt` are what comes back, and `again`, `nodir` and `long` are the three
//! refusals — a link that is already there, a directory that is not, and a target past
//! `_POSIX_SYMLINK_MAX` (which is refused *before* the path is read, so its errno is not
//! the missing directory's).

use crate::{Decimal, append, write_err, write_out};

/// Where the link is made. `/tmp` is a directory in the image a session may write, which
/// is why the bash gate's own redirect step uses it too.
const LINK: &[u8] = b"/tmp/sl";
const TARGET: &[u8] = b"/sys/devices/virtio-gpu";
/// A directory that is not there, so the link's path cannot be resolved.
const NODIR: &[u8] = b"/no-such-dir/sl";
/// Past `_POSIX_SYMLINK_MAX` (255), which nothing else here reaches.
const LONG: [u8; 300] = [b'a'; 300];

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

/// Create the link, read it back, and report what the three refusals said.
pub fn symlinktest(_args: &[&str]) -> i32 {
    if let Err(e) = minix_std::fs::symlink(TARGET, LINK) {
        return fail(b"symlinktest: symlink failed, errno ", e.0);
    }

    let mut buf = [0u8; 64];
    let n = match minix_std::fs::readlink(LINK, &mut buf) {
        Ok(n) => n,
        Err(e) => return fail(b"symlinktest: readlink failed, errno ", e.0),
    };
    let tgt = n == TARGET.len() && buf[..n] == *TARGET;

    // The link is there now, so a second attempt is the filesystem's own `EEXIST`; the
    // second has no directory to be resolved in; the third's *target* is past the limit, so
    // it must come back as a length error even though its directory is missing as well.
    let again = minix_std::fs::symlink(TARGET, LINK).err().map(|e| e.0);
    let nodir = minix_std::fs::symlink(TARGET, NODIR).err().map(|e| e.0);
    let long = minix_std::fs::symlink(&LONG, NODIR).err().map(|e| e.0);

    let mut line = [0u8; 160];
    let mut at = 0usize;
    append(&mut line, &mut at, b"symlinktest:");
    named(&mut line, &mut at, b" n=", n);
    append(
        &mut line,
        &mut at,
        if tgt { b" tgt=ok" } else { b" tgt=bad" },
    );
    refused(&mut line, &mut at, b" again=", again);
    refused(&mut line, &mut at, b" nodir=", nodir);
    refused(&mut line, &mut at, b" long=", long);
    append(&mut line, &mut at, b"\n");
    write_out(&line[..at]);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three refusal paths are distinct, and none of them is the link that is supposed
    /// to be created first — a step whose expectation another step's output could satisfy is
    /// the trap the scenarios' anchoring exists for.
    #[test]
    fn the_four_paths_are_distinct() {
        assert_ne!(LINK, NODIR);
        assert_ne!(LINK, TARGET);
        assert_ne!(NODIR, TARGET);
    }

    /// The long target has to be past the limit and the real one under it, or `long` would
    /// assert the same thing as a successful creation.
    #[test]
    fn the_long_target_is_past_the_limit() {
        assert!(
            LONG.len() > 255,
            "the long target must be refused for its length"
        );
        assert!(TARGET.len() <= 255, "the real target must be accepted");
    }

    /// A field's rendering: `refused` writes `ok` only for a call that was *not* refused, so
    /// an unexpected success cannot read as a matching line.
    #[test]
    fn a_refusal_field_writes_ok_only_for_a_success() {
        let mut line = [0u8; 32];
        let mut at = 0usize;
        refused(&mut line, &mut at, b" again=", Some(17));
        refused(&mut line, &mut at, b" nodir=", None);
        assert_eq!(&line[..at], b" again=err 17 nodir=ok");
    }
}
