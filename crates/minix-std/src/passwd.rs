//! `/etc/passwd` parsing and the `$5$` password-verification scheme.
//!
//! The hash scheme is a documented stand-in for crypt(3): a password
//! field of `$5$<salt>$<hex>` stores the lowercase hex of
//! `sha256(salt || password)`. An empty field means "no password".

use crate::sha256::{sha256, to_hex};

/// One parsed `/etc/passwd` entry (fields borrow the line).
pub struct PasswdEntry<'a> {
    pub name: &'a [u8],
    pub passwd: &'a [u8],
    pub uid: u32,
    pub gid: u32,
    pub gecos: &'a [u8],
    pub dir: &'a [u8],
    pub shell: &'a [u8],
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(v)
}

/// Parse one passwd line (`name:passwd:uid:gid:gecos:dir:shell`); `None`
/// on a malformed line. Trailing fields may be empty or missing.
pub fn parse_passwd_line(line: &[u8]) -> Option<PasswdEntry<'_>> {
    let mut fields = line.split(|&b| b == b':');
    let name = fields.next()?;
    if name.is_empty() {
        return None;
    }
    let passwd = fields.next()?;
    let uid = parse_u32(fields.next()?)?;
    let gid = parse_u32(fields.next()?)?;
    let gecos = fields.next().unwrap_or(b"");
    let dir = fields.next().unwrap_or(b"");
    let shell = fields.next().unwrap_or(b"");
    Some(PasswdEntry {
        name,
        passwd,
        uid,
        gid,
        gecos,
        dir,
        shell,
    })
}

/// Find the first entry whose name matches `name`.
pub fn find_passwd<'a>(data: &'a [u8], name: &[u8]) -> Option<PasswdEntry<'a>> {
    for line in data.split(|&b| b == b'\n') {
        if let Some(e) = parse_passwd_line(line)
            && e.name == name
        {
            return Some(e);
        }
    }
    None
}

/// Find the first entry with uid `uid`.
pub fn find_passwd_uid<'a>(data: &'a [u8], uid: u32) -> Option<PasswdEntry<'a>> {
    for line in data.split(|&b| b == b'\n') {
        if let Some(e) = parse_passwd_line(line)
            && e.uid == uid
        {
            return Some(e);
        }
    }
    None
}

/// Verify `password` against a passwd password field.
///
/// - empty field → matches (no password set)
/// - `$5$<salt>$<64-hex>` → `sha256(salt || password)` hex compare
/// - anything else (e.g. `*`, `!`) → never matches (locked account)
pub fn passwd_matches(pw_field: &[u8], password: &[u8]) -> bool {
    if pw_field.is_empty() {
        return true;
    }
    let rest = match pw_field.strip_prefix(b"$5$") {
        Some(r) => r,
        None => return false,
    };
    let dollar = match rest.iter().position(|&b| b == b'$') {
        Some(d) => d,
        None => return false,
    };
    let salt = &rest[..dollar];
    let stored = &rest[dollar + 1..];
    if stored.len() != 64 {
        return false;
    }
    // Salt + password fit the demo buffer (salt <= 16, password <= 64).
    if salt.len() > 16 || password.len() > 64 {
        return false;
    }
    let mut salted = [0u8; 80];
    salted[..salt.len()].copy_from_slice(salt);
    salted[salt.len()..salt.len() + password.len()].copy_from_slice(password);
    let digest = sha256(&salted[..salt.len() + password.len()]);
    stored == to_hex(&digest)
}

/// Read `/etc/passwd` into `buf`; returns the number of bytes read.
///
/// # Safety
///
/// Must be called with the VFS server running.
#[cfg(target_os = "minix")]
pub unsafe fn read_passwd(buf: &mut [u8]) -> Result<usize, crate::MinixErr> {
    let fd = unsafe { crate::fs::open(b"/etc/passwd", crate::fs::O_RDONLY, 0) }?;
    let mut total = 0usize;
    loop {
        let n = unsafe { crate::fs::read(fd, &mut buf[total..]) }?;
        if n <= 0 {
            break;
        }
        total += n as usize;
        if total >= buf.len() {
            break;
        }
    }
    let _ = crate::fs::close(fd);
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &[u8] = b"root::0:0:root:/:/bin/sh\n\
test:$5$minix$9d0f2a2a9c9c6c5e6f7f8f9f0e1d2c3b4a5968778899aabbccddeeff00112233:1000:1000:test user:/:/bin/sh\n";

    #[test]
    fn test_parse_line() {
        let e = parse_passwd_line(b"root::0:0:root:/:/bin/sh").unwrap();
        assert_eq!(e.name, b"root");
        assert_eq!(e.passwd, b"");
        assert_eq!(e.uid, 0);
        assert_eq!(e.gid, 0);
        assert_eq!(e.shell, b"/bin/sh");
    }

    #[test]
    fn test_parse_line_short() {
        // Missing gecos/dir/shell defaults to empty.
        let e = parse_passwd_line(b"root::0:0").unwrap();
        assert_eq!(e.gecos, b"");
        assert_eq!(e.dir, b"");
        assert_eq!(e.shell, b"");
    }

    #[test]
    fn test_parse_line_bad_uid() {
        assert!(parse_passwd_line(b"root::x:0:::").is_none());
        assert!(parse_passwd_line(b"::0:0:::").is_none());
    }

    #[test]
    fn test_find_passwd() {
        assert_eq!(find_passwd(PASSWD, b"test").unwrap().uid, 1000);
        assert_eq!(find_passwd(PASSWD, b"root").unwrap().uid, 0);
        assert!(find_passwd(PASSWD, b"nobody").is_none());
    }

    #[test]
    fn test_find_passwd_uid() {
        assert_eq!(find_passwd_uid(PASSWD, 1000).unwrap().name, b"test");
        assert!(find_passwd_uid(PASSWD, 999).is_none());
    }

    #[test]
    fn test_empty_password_matches() {
        assert!(passwd_matches(b"", b"anything"));
    }

    #[test]
    fn test_locked_password_never_matches() {
        assert!(!passwd_matches(b"*", b"anything"));
        assert!(!passwd_matches(b"!", b"anything"));
    }

    #[test]
    fn test_malformed_hash_never_matches() {
        assert!(!passwd_matches(b"$5$minix", b"test123"));
        assert!(!passwd_matches(b"$5$minix$abcd", b"test123"));
    }

    #[test]
    fn test_sha256_hash_matches() {
        // Build a field the same way su's verifier expects: sha256("minix" + pw).
        let mut salted = [0u8; 80];
        salted[..5].copy_from_slice(b"minix");
        salted[5..5 + 7].copy_from_slice(b"test123");
        let digest = sha256(&salted[..12]);
        let hex = to_hex(&digest);
        let mut full = [0u8; 9 + 64];
        full[..9].copy_from_slice(b"$5$minix$");
        full[9..].copy_from_slice(&hex);
        assert!(passwd_matches(&full, b"test123"));
        assert!(!passwd_matches(&full, b"wrong"));
    }
}
