//! `/bin/linktest` — `link(2)` and `rename(2)`, measured rather than assumed.
//!
//! `KNOWN_ISSUES.md` item 37: VFS's `do_link`/`do_rename` pass a null name where the name goes,
//! and MFS has no parser for `REQ_LINK`/`REQ_RENAME`, so both calls read `cch[]`/`user_path` left
//! over from whatever request ran before them. Nothing exercised either, so this client is the
//! measurement — it makes its own files, links one, renames the link, and reports the two calls'
//! outcomes *and* the state that follows them, so a call that "succeeds" against the wrong file is
//! as visible as one that fails.
//!
//! The whole line below is the step this gate is written for: it is what `link` and `rename`
//! should answer, so it is red until item 37 is fixed. `keep` is the guard on that — a call that
//! went to the wrong name must not be able to leave the third file alone and still look right.

use crate::{Decimal, append, write_err, write_out};
use minix_std::fs::{O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY};

const SRC: &str = "/tmp/lr-src";
const HARD: &str = "/tmp/lr-hard";
const MOVED: &str = "/tmp/lr-moved";
/// A file neither call may touch, so "did this go to the wrong name?" has an answer.
const OTHER: &str = "/tmp/lr-other";
const SRC_DATA: &[u8] = b"lr-src";
const OTHER_DATA: &[u8] = b"lr-other";

/// Report a failure that leaves the rest of the line unwritable, with the errno.
fn fail(what: &[u8], path: &str, errno: i32) -> i32 {
    write_err(what);
    write_err(path.as_bytes());
    write_err(b", errno ");
    write_err(Decimal::of(errno as u32).bytes());
    write_err(b"\n");
    1
}

/// Write `data` to `path`, creating or truncating it.
fn make(path: &str, data: &[u8]) -> Result<(), i32> {
    let fd = match unsafe {
        minix_std::fs::open(path.as_bytes(), O_CREAT | O_WRONLY | O_TRUNC, 0o644)
    } {
        Ok(fd) => fd,
        Err(e) => return Err(e.0),
    };
    let written = unsafe { minix_std::fs::write(fd, data) };
    // The descriptor is closed on every path, so a failed write still releases it.
    let closed = minix_std::fs::close(fd);
    written.map_err(|e| e.0)?;
    closed.map_err(|e| e.0)
}

/// Whether `path` holds exactly `data`. Every error — the file not being there, a descriptor that
/// will not open, a short read — is a `false` rather than an errno of its own: this is a check on
/// what a call left behind, and how it failed is not what the field is claiming.
fn holds(path: &str, data: &[u8]) -> bool {
    let mut buf = [0u8; 16];
    let fd = match unsafe { minix_std::fs::open(path.as_bytes(), O_RDONLY, 0) } {
        Ok(fd) => fd,
        Err(_) => return false,
    };
    let got = unsafe { minix_std::fs::read(fd, &mut buf) };
    let closed = minix_std::fs::close(fd);
    match (got, closed) {
        (Ok(n), Ok(())) => n as usize == data.len() && buf[..data.len()] == *data,
        _ => false,
    }
}

/// Whether two names are the same file — the claim a hard link makes, and the one a *stale* name
/// would break while still reporting success.
fn same_file(a: &str, b: &str) -> bool {
    match (minix_std::fs::stat(a), minix_std::fs::stat(b)) {
        (Ok(x), Ok(y)) => x.st_dev == y.st_dev && x.st_ino == y.st_ino,
        _ => false,
    }
}

/// A call's outcome: the errno it gave, or `ok` when it reported success.
fn call(line: &mut [u8], at: &mut usize, label: &[u8], errno: Option<i32>) {
    append(line, at, label);
    match errno {
        Some(e) => {
            append(line, at, b"err ");
            append(line, at, Decimal::of(e as u32).bytes());
        }
        None => append(line, at, b"ok"),
    }
}

/// A verification: `ok`, `bad`, or `none` when the call it checks did not get far enough to
/// check. The three are distinct on purpose — "succeeded but left the wrong state" is not the
/// same failure as "did not run", and a line that ran them together would hide the second.
fn checked(line: &mut [u8], at: &mut usize, label: &[u8], outcome: Option<bool>) {
    append(line, at, label);
    match outcome {
        Some(true) => append(line, at, b"ok"),
        Some(false) => append(line, at, b"bad"),
        None => append(line, at, b"none"),
    }
}

/// Link a file, rename the link, and report what the calls said and what they left behind.
pub fn linktest(_args: &[&str]) -> i32 {
    for (path, data) in [(SRC, SRC_DATA), (OTHER, OTHER_DATA)] {
        if let Err(e) = make(path, data) {
            return fail(b"linktest: could not make ", path, e);
        }
    }

    let ln = minix_std::fs::link(SRC.as_bytes(), HARD.as_bytes())
        .err()
        .map(|e| e.0);
    let tgt = if ln.is_some() {
        None
    } else {
        Some(same_file(SRC, HARD))
    };

    let rn = minix_std::fs::rename(SRC.as_bytes(), MOVED.as_bytes())
        .err()
        .map(|e| e.0);
    let mv = if rn.is_some() {
        None
    } else {
        // The move is complete when the old name is *gone* and the new one holds the bytes: a
        // rename that copied and left the original is a different bug, and this sees it.
        Some(minix_std::fs::stat(SRC).is_err() && holds(MOVED, SRC_DATA))
    };

    let keep = Some(holds(OTHER, OTHER_DATA));

    let mut line = [0u8; 160];
    let mut at = 0usize;
    append(&mut line, &mut at, b"linktest:");
    call(&mut line, &mut at, b" ln=", ln);
    checked(&mut line, &mut at, b" tgt=", tgt);
    call(&mut line, &mut at, b" rn=", rn);
    checked(&mut line, &mut at, b" mv=", mv);
    checked(&mut line, &mut at, b" keep=", keep);
    append(&mut line, &mut at, b"\n");
    write_out(&line[..at]);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four paths are distinct: a step whose expectation another step's name could satisfy is
    /// the trap these scenarios anchor against.
    #[test]
    fn the_four_paths_are_distinct() {
        assert_ne!(SRC, HARD);
        assert_ne!(SRC, MOVED);
        assert_ne!(SRC, OTHER);
        assert_ne!(HARD, MOVED);
        assert_ne!(HARD, OTHER);
        assert_ne!(MOVED, OTHER);
    }

    /// The contents differ, so `keep` can tell one file's bytes from another's.
    #[test]
    fn the_two_files_hold_different_bytes() {
        assert_ne!(SRC_DATA, OTHER_DATA);
        assert!(
            SRC_DATA.len() <= 16 && OTHER_DATA.len() <= 16,
            "the buffer must hold either"
        );
    }

    /// `checked` writes three different words, so "succeeded with the wrong state" cannot read as
    /// "did not run".
    #[test]
    fn a_verified_field_renders_three_states() {
        let mut line = [0u8; 32];
        let mut at = 0usize;
        checked(&mut line, &mut at, b" a=", Some(true));
        checked(&mut line, &mut at, b" b=", Some(false));
        checked(&mut line, &mut at, b" c=", None);
        assert_eq!(&line[..at], b" a=ok b=bad c=none");
    }

    /// A call's outcome is `ok` only for a call that was *not* refused, with the errno otherwise.
    #[test]
    fn a_call_field_writes_ok_only_for_a_success() {
        let mut line = [0u8; 32];
        let mut at = 0usize;
        call(&mut line, &mut at, b" ln=", Some(22));
        call(&mut line, &mut at, b" rn=", None);
        assert_eq!(&line[..at], b" ln=err 22 rn=ok");
    }
}
