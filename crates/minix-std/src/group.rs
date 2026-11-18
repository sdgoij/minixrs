//! `/etc/group` parsing for the `grp.h` lookups.
//!
//! The format is `name:passwd:gid:member,member,...`, one entry per line. The
//! member list is kept as the raw comma-separated field: `getgrgid`/`getgrnam`
//! are the only consumers and they walk it themselves, so there is no reason to
//! build a second representation here.

/// One parsed `/etc/group` entry (fields borrow the line).
pub struct GroupEntry<'a> {
    pub name: &'a [u8],
    pub passwd: &'a [u8],
    pub gid: u32,
    /// The comma-separated member list, empty when there are no members.
    pub members: &'a [u8],
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

/// Parse one group line (`name:passwd:gid:members`); `None` on a malformed
/// line. The member field may be empty or missing.
pub fn parse_group_line(line: &[u8]) -> Option<GroupEntry<'_>> {
    let mut fields = line.split(|&b| b == b':');
    let name = fields.next()?;
    if name.is_empty() {
        return None;
    }
    let passwd = fields.next()?;
    let gid = parse_u32(fields.next()?)?;
    let members = fields.next().unwrap_or(b"");
    Some(GroupEntry {
        name,
        passwd,
        gid,
        members,
    })
}

/// Find the first entry whose name matches `name`.
pub fn find_group<'a>(data: &'a [u8], name: &[u8]) -> Option<GroupEntry<'a>> {
    for line in data.split(|&b| b == b'\n') {
        if let Some(e) = parse_group_line(line)
            && e.name == name
        {
            return Some(e);
        }
    }
    None
}

/// Find the first entry with gid `gid`.
pub fn find_group_gid<'a>(data: &'a [u8], gid: u32) -> Option<GroupEntry<'a>> {
    for line in data.split(|&b| b == b'\n') {
        if let Some(e) = parse_group_line(line)
            && e.gid == gid
        {
            return Some(e);
        }
    }
    None
}

/// Read `/etc/group` into `buf`; returns the number of bytes read.
///
/// A guest with no `/etc/group` is not an error: the lookups then simply find
/// no entry, which is what `ENOENT` reports.
///
/// # Safety
///
/// Must be called with the VFS server running.
#[cfg(target_os = "minix")]
pub unsafe fn read_group(buf: &mut [u8]) -> Result<usize, crate::MinixErr> {
    let fd = match unsafe { crate::fs::open(b"/etc/group", crate::fs::O_RDONLY, 0) } {
        Ok(fd) => fd,
        Err(e) if e == crate::MinixErr::from_i32(crate::ENOENT) => return Ok(0),
        Err(e) => return Err(e),
    };
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

    const GROUP: &[u8] = b"root:x:0:\ntest:x:1000:alice,bob\nwheel:x:10:\n";

    #[test]
    fn test_parse_line() {
        let e = parse_group_line(b"test:x:1000:alice,bob").unwrap();
        assert_eq!(e.name, b"test");
        assert_eq!(e.passwd, b"x");
        assert_eq!(e.gid, 1000);
        assert_eq!(e.members, b"alice,bob");
    }

    #[test]
    fn test_parse_line_no_members() {
        let e = parse_group_line(b"root:x:0:").unwrap();
        assert_eq!(e.members, b"");
        // A line that stops after the gid is still an entry.
        let e = parse_group_line(b"root:x:0").unwrap();
        assert_eq!(e.gid, 0);
        assert_eq!(e.members, b"");
    }

    #[test]
    fn test_parse_line_malformed() {
        assert!(parse_group_line(b"root:x:abc:").is_none());
        assert!(parse_group_line(b"root:x::").is_none());
        assert!(parse_group_line(b":x:0:").is_none());
        assert!(parse_group_line(b"root:x").is_none());
    }

    #[test]
    fn test_find_group() {
        assert_eq!(find_group(GROUP, b"test").unwrap().gid, 1000);
        assert_eq!(find_group(GROUP, b"root").unwrap().gid, 0);
        assert!(find_group(GROUP, b"nobody").is_none());
    }

    #[test]
    fn test_find_group_gid() {
        assert_eq!(find_group_gid(GROUP, 1000).unwrap().name, b"test");
        assert_eq!(find_group_gid(GROUP, 10).unwrap().name, b"wheel");
        assert!(find_group_gid(GROUP, 999).is_none());
    }

    #[test]
    fn test_find_skips_blank_and_malformed_lines() {
        let data = b"\nbadline\ntest:x:1000:\n";
        assert_eq!(find_group(data, b"test").unwrap().gid, 1000);
    }
}
