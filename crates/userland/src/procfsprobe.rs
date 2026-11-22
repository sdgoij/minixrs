//! `/bin/procfsprobe` — reads the files procfs serves from the kernel clock and the
//! kernel's load history.
//!
//! `KNOWN_ISSUES.md` item 7 wired the procfs server: a boot process of its own, an
//! initramfs entry, and a mount at `/proc` in VFS. Nothing else in the tree opens a
//! path under a *user-mounted* VTreeFS, so without this client a mount that never
//! crossed the boundary would look exactly like one that did: the server's own unit
//! tests drive `handle_fs_message` directly and never involve VFS or MFS.
//!
//! The fields are the things a mount has to get right and the ones it must refuse:
//! `hz` is `/proc/hz`, a positive decimal from `SYS_TIMES`; `uptime` is
//! `/proc/uptime`, `<seconds>.<hundredths>`; `load` is `/proc/loadavg`, three
//! `%ld.%02ld` averages from `GETINFO`'s `GET_LOADINFO`; `dir` is `/proc` itself,
//! which has to resolve to a directory; `list` is the root listing, which must name
//! every entry exactly once; `self` and `exe` are `/proc/self` and `/proc/self/exe`,
//! whose targets depend on the calling process, so they read back only if VFS passes
//! the caller's endpoint through the mount and restarts an absolute link target from
//! the root; `name` is `/proc/self/status`, whose `Name:` has to be the program the
//! probe was exec'd from rather than the name inherited at fork; and `gone` is a path
//! under `/proc` that is not there, so the mount answers from its own tree instead of
//! silently falling back to MFS.

use crate::{DIRENT_NAME_OFF, Decimal, append, write_err, write_out};
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

/// One `%ld.%02ld` load figure: digits, a dot, exactly two digits.
fn is_load_figure(field: &[u8]) -> bool {
    let mut parts = field.split(|&b| b == b'.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(whole), Some(hundredths), None) => {
            !whole.is_empty()
                && hundredths.len() == 2
                && whole.iter().all(u8::is_ascii_digit)
                && hundredths.iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

/// Three load figures separated by single spaces, as C's `root_loadavg` prints them.
fn is_loadavg(bytes: &[u8]) -> bool {
    let body = match bytes {
        [rest @ .., b'\n'] => rest,
        rest => rest,
    };
    let mut fields = body.split(|&b| b == b' ');
    let first = fields.next().is_some_and(is_load_figure);
    let second = fields.next().is_some_and(is_load_figure);
    let third = fields.next().is_some_and(is_load_figure);
    first && second && third && fields.next().is_none()
}

/// A PID directory's name as a number, or `None` when the name is not all digits.
fn parse_pid(name: &[u8]) -> Option<i32> {
    if name.is_empty() || name.len() > 9 || !name.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut v: i32 = 0;
    for &b in name {
        v = v.checked_mul(10)?.checked_add((b - b'0') as i32)?;
    }
    Some(v)
}

/// What one listing of `/proc`'s root has to look like: `hz`, `uptime`, `loadavg`,
/// `self`, and one directory per live process, each exactly once.
///
/// The failure this catches is a listing that repeats an ordinary entry once per
/// indexed slot (the `indexed` range read as ordinary children) or never yields the
/// PID directories at all (they were not enumerated). A duplicate PID is counted as
/// unexpected entry rather than a second directory.
struct Listing {
    hz: u32,
    uptime: u32,
    loadavg: u32,
    self_link: u32,
    pids: usize,
    pid_values: [i32; 64],
    unexpected: u32,
}

impl Listing {
    fn new() -> Self {
        Self {
            hz: 0,
            uptime: 0,
            loadavg: 0,
            self_link: 0,
            pids: 0,
            pid_values: [0; 64],
            unexpected: 0,
        }
    }

    fn add(&mut self, name: &[u8]) {
        match name {
            b"." | b".." => {}
            b"hz" => self.hz += 1,
            b"uptime" => self.uptime += 1,
            b"loadavg" => self.loadavg += 1,
            b"self" => self.self_link += 1,
            _ => match parse_pid(name) {
                Some(pid) if self.pids < self.pid_values.len() => {
                    if self.pid_values[..self.pids].contains(&pid) {
                        self.unexpected += 1;
                    } else {
                        self.pid_values[self.pids] = pid;
                        self.pids += 1;
                    }
                }
                // A non-numeric name, a duplicate, or more directories than the buffer holds.
                _ => self.unexpected += 1,
            },
        }
    }

    fn good(&self) -> bool {
        // At least one PID directory: the probe's own process is alive while it runs.
        self.hz == 1
            && self.uptime == 1
            && self.loadavg == 1
            && self.self_link == 1
            && self.pids >= 1
            && self.unexpected == 0
    }
}

/// Enumerate `/proc` the way `ls` does and check the root listing.
fn list_root() -> bool {
    let fd = match unsafe { minix_std::fs::open(b"/proc", O_RDONLY, 0) } {
        Ok(fd) => fd,
        Err(_) => return false,
    };
    let mut listing = Listing::new();
    let mut buf = [0u8; 4096];
    let mut ok = true;
    loop {
        let n = match minix_std::fs::getdents(fd, &mut buf) {
            Ok(n) if n > 0 => n as usize,
            Ok(_) => break,
            Err(_) => {
                ok = false;
                break;
            }
        };
        let mut off = 0usize;
        while off < n {
            if off + DIRENT_NAME_OFF > n {
                ok = false;
                break;
            }
            let reclen = u16::from_ne_bytes([buf[off + 8], buf[off + 9]]) as usize;
            if reclen == 0 || off + reclen > n {
                ok = false;
                break;
            }
            let namlen = u16::from_ne_bytes([buf[off + 10], buf[off + 11]]) as usize;
            if off + DIRENT_NAME_OFF + namlen > n {
                ok = false;
                break;
            }
            listing.add(&buf[off + DIRENT_NAME_OFF..off + DIRENT_NAME_OFF + namlen]);
            off += reclen;
        }
        if !ok {
            break;
        }
    }
    let _ = minix_std::fs::close(fd);
    ok && listing.good()
}

/// Read up to `buf.len()` bytes of `path` and hand them to `check`.
fn read_into(path: &[u8], buf: &mut [u8], check: impl Fn(&[u8]) -> bool) -> bool {
    let fd = match unsafe { minix_std::fs::open(path, O_RDONLY, 0) } {
        Ok(fd) => fd,
        Err(_) => return false,
    };
    let read = unsafe { minix_std::fs::read(fd, buf) };
    let closed = minix_std::fs::close(fd);
    match (read, closed) {
        (Ok(n), Ok(())) => check(&buf[..n.max(0) as usize]),
        _ => false,
    }
}

/// Open `path`, read it whole (up to `BUF`), and hand the bytes to `check`.
fn read_and(path: &[u8], check: impl Fn(&[u8]) -> bool) -> bool {
    let mut buf = [0u8; BUF];
    read_into(path, &mut buf, check)
}

/// Read a symbolic link's target into `buf`, returning its length.
fn read_link(path: &[u8], buf: &mut [u8]) -> Option<usize> {
    match minix_std::fs::readlink(path, buf) {
        Ok(n) if n > 0 && n <= buf.len() => Some(n),
        _ => None,
    }
}

/// Whether `target` is exactly `/proc/<pid>`.
fn names_pid(target: &[u8], pid: i32) -> bool {
    let digits = Decimal::of(pid.max(0) as u32);
    let digits = digits.bytes();
    target.len() == 6 + digits.len() && target.starts_with(b"/proc/") && &target[6..] == digits
}

/// Whether a `status` text's `Name:` line names `want`.
///
/// The field is `Name:` padded with spaces, so the value is what follows the
/// padding; comparing the whole line would pin the padding too.
fn names_program(status: &[u8], want: &[u8]) -> bool {
    status.split(|&b| b == b'\n').any(|line| {
        let Some(rest) = line.strip_prefix(b"Name:") else {
            return false;
        };
        let start = rest
            .iter()
            .position(|&b| b != b' ' && b != b'\t')
            .unwrap_or(rest.len());
        &rest[start..] == want
    })
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
    let load = read_and(b"/proc/loadavg", is_loadavg);

    // `/proc` itself resolves through the mount, so its mode is the mount point's.
    let dir = match minix_std::fs::stat_bytes(b"/proc") {
        Ok(st) => st.st_mode & 0o170000 == 0o040000,
        Err(_) => false,
    };

    let list = list_root();

    // `/proc/self` names the asking process's own PID directory, and
    // `/proc/self/exe` the program it was started from. Both targets come from
    // the caller's endpoint, which VFS passes with the lookup, and `self` also
    // exercises the restart VFS performs when a link's target is absolute: the
    // walk has to hand the expanded path back and continue from the root.
    let pid = minix_std::process::getpid()
        .map(|(pid, _)| pid)
        .unwrap_or(-1);
    let mut link_buf = [0u8; 64];
    let self_ok = read_link(b"/proc/self", &mut link_buf)
        .is_some_and(|n| names_pid(&link_buf[..n], pid));
    let mut link_buf = [0u8; 64];
    let exe_ok = read_link(b"/proc/self/exe", &mut link_buf)
        .is_some_and(|n| &link_buf[..n] == b"/bin/procfsprobe");

    // `status`'s `Name:` is the program the process is running, which PM takes
    // from the exec path; before that it kept the name inherited at fork.
    let mut status_buf = [0u8; 128];
    let name_ok = read_into(b"/proc/self/status", &mut status_buf, |b| {
        names_program(b, b"procfsprobe")
    });

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
    verdict(&mut line, &mut at, b"load", load);
    append(&mut line, &mut at, b" ");
    verdict(&mut line, &mut at, b"dir", dir);
    append(&mut line, &mut at, b" ");
    verdict(&mut line, &mut at, b"list", list);
    append(&mut line, &mut at, b" ");
    verdict(&mut line, &mut at, b"self", self_ok);
    append(&mut line, &mut at, b" ");
    verdict(&mut line, &mut at, b"exe", exe_ok);
    append(&mut line, &mut at, b" ");
    verdict(&mut line, &mut at, b"name", name_ok);
    append(&mut line, &mut at, b" gone=err 2\n");
    write_out(&line[..at]);

    if hz && uptime && load && dir && list && self_ok && exe_ok && name_ok {
        0
    } else {
        1
    }
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

    #[test]
    fn pid_names_are_all_digits() {
        assert_eq!(parse_pid(b"1"), Some(1));
        assert_eq!(parse_pid(b"1234"), Some(1234));
        assert_eq!(parse_pid(b""), None);
        assert_eq!(parse_pid(b"hz"), None);
        assert_eq!(parse_pid(b"1a"), None);
        assert_eq!(parse_pid(b"-1"), None);
        assert_eq!(parse_pid(b"1234567890"), None, "too long for a pid");
    }

    #[test]
    fn a_listing_with_one_of_everything_is_good() {
        let mut l = Listing::new();
        for name in [&b"."[..], b"..", b"hz", b"uptime", b"loadavg", b"self", b"1", b"42"] {
            l.add(name);
        }
        assert!(l.good());
        assert_eq!(l.pids, 2);
    }

    #[test]
    fn a_repeated_entry_is_not_good() {
        // The symptom of the indexed-range bug: `uptime` listed many times.
        let mut l = Listing::new();
        for name in [&b"hz"[..], b"uptime", b"uptime", b"1"] {
            l.add(name);
        }
        assert!(!l.good());

        // A PID directory listed twice is a duplicate as well.
        let mut l = Listing::new();
        for name in [&b"hz"[..], b"uptime", b"7", b"7"] {
            l.add(name);
        }
        assert!(!l.good());

        // A listing with no PID directory at all never enumerated the indexed children.
        let mut l = Listing::new();
        for name in [&b"hz"[..], b"uptime", b"loadavg"] {
            l.add(name);
        }
        assert!(!l.good());
    }

    #[test]
    fn a_self_target_names_its_pid() {
        assert!(names_pid(b"/proc/27", 27));
        assert!(names_pid(b"/proc/1", 1));
        assert!(!names_pid(b"/proc/", 1), "a target with no pid is not it");
        assert!(!names_pid(b"/proc/21", 12), "the digits are compared whole");
        assert!(!names_pid(b"/proc/2x", 2), "a non-digit is not it");
        assert!(!names_pid(b"/proc2/7", 7), "the prefix is exactly /proc/");
    }

    #[test]
    fn a_status_name_line_is_read_after_its_padding() {
        assert!(names_program(b"Name:   procfsprobe\nState:  R\n", b"procfsprobe"));
        assert!(names_program(b"Name:\tcoreutils\n", b"coreutils"));
        assert!(!names_program(b"Name:   init\n", b"procfsprobe"));
        assert!(!names_program(b"State:  R\n", b"procfsprobe"));
        assert!(!names_program(b"Name:   procfsprobe extra\n", b"procfsprobe"));
    }

    #[test]
    fn loadavg_is_three_padded_figures() {
        assert!(is_loadavg(b"0.00 0.00 0.00\n"));
        assert!(is_loadavg(b"1.04 12.50 100.00"));
        assert!(!is_loadavg(b"0.0 0.00 0.00\n"), "hundredths are two digits");
        assert!(!is_loadavg(b"0.00 0.00\n"), "three figures");
        assert!(!is_loadavg(b"0.00  0.00 0.00\n"), "single spaces");
        assert!(!is_loadavg(b"\n"));
    }
}
