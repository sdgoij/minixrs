//! `/var/run/utmpx`: the login-record file.
//!
//! One `struct utmpx` image (`tools/c-include/utmpx.h`) follows another, so
//! record `n` starts at `n * 520`. The entries are written in this machine's
//! byte order, like every other binary structure the port keeps, and the
//! types here are `repr(C)` so `size_of` is the record stride on every target.
//!
//! The port does not implement NetBSD's on-disk `utmp` v2 encoding: the file
//! holds the struct itself, which is what the readers in `crates/minix-libc`
//! hand back from `getutxent`.
//!
//! The writers are init (a `BOOT_TIME` entry and the console session's
//! `USER_PROCESS`) and the shell (which retires that session on exit). Only
//! that one session exists, so "rewrite the whole file" is a fine write
//! strategy: it keeps the file the same size however often a shell comes and
//! goes, and needs no locking.

/// `_UTX_NAMESIZE`: the login name's field, NUL-terminated.
pub const UT_NAMESIZE: usize = 32;
/// `_UTX_IDSIZE`: the inittab id.
pub const UT_IDSIZE: usize = 4;
/// `_UTX_LINESIZE`: the terminal's name, without `/dev/`.
pub const UT_LINESIZE: usize = 32;
/// `_UTX_HOSTSIZE`: the host the session came from.
pub const UT_HOSTSIZE: usize = 256;
/// The trailing reserved bytes of the struct.
pub const UT_PADSIZE: usize = 36;

/// `sizeof(struct utmpx)`, and therefore the file's stride.
pub const UT_RECORD_SIZE: usize = 520;

/// `EMPTY` and the record kinds the port writes or reports.
pub const EMPTY: u16 = 0;
pub const RUN_LVL: u16 = 1;
pub const BOOT_TIME: u16 = 2;
pub const OLD_TIME: u16 = 3;
pub const NEW_TIME: u16 = 4;
pub const INIT_PROCESS: u16 = 5;
pub const LOGIN_PROCESS: u16 = 6;
pub const USER_PROCESS: u16 = 7;
pub const DEAD_PROCESS: u16 = 8;

/// Where the port keeps the login records.
///
/// `_PATH_UTMPX` on a full system is `/var/run/utmpx`; this image has no `/var`, so
/// the file lives in the writable tmpfs the image already carries. `who`, `users`,
/// `pinky` and `uptime` name this path in their help and read it by default.
pub const DEFAULT_FILE: &[u8] = b"/tmp/utmpx";

/// The most entries the file may hold. A fixed bound is what lets every reader
/// and writer work in a static buffer: there is one console session, so 16 is
/// far more than the two live entries a boot ever has.
pub const MAX_RECORDS: usize = 16;

/// `struct sockaddr_storage`, as `utmpx` embeds it: 128 bytes, 8-aligned.
/// Nothing the port writes or reads looks inside it, so it is an opaque block
/// rather than a union of address families.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct SockaddrStorage {
    pub bytes: [u8; 128],
}

/// `struct timeval`, the type of `utmpx::ut_tv`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// `utmpx::ut_exit`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExitStatus {
    pub e_termination: u16,
    pub e_exit: u16,
}

/// `struct utmpx`, in header order.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Utmpx {
    pub ut_user: [u8; UT_NAMESIZE],
    pub ut_id: [u8; UT_IDSIZE],
    pub ut_line: [u8; UT_LINESIZE],
    pub ut_host: [u8; UT_HOSTSIZE],
    pub ut_session: u16,
    pub ut_type: u16,
    pub ut_pid: i32,
    pub ut_exit: ExitStatus,
    pub ut_ss: SockaddrStorage,
    pub ut_tv: TimeVal,
    pub ut_pad: [u8; UT_PADSIZE],
}

/// An all-zero entry, which is what a free slot holds.
pub const EMPTY_RECORD: Utmpx = Utmpx {
    ut_user: [0; UT_NAMESIZE],
    ut_id: [0; UT_IDSIZE],
    ut_line: [0; UT_LINESIZE],
    ut_host: [0; UT_HOSTSIZE],
    ut_session: 0,
    ut_type: EMPTY,
    ut_pid: 0,
    ut_exit: ExitStatus {
        e_termination: 0,
        e_exit: 0,
    },
    ut_ss: SockaddrStorage { bytes: [0; 128] },
    ut_tv: TimeVal {
        tv_sec: 0,
        tv_usec: 0,
    },
    ut_pad: [0; UT_PADSIZE],
};

// The header and the file stride both assume this size on every target; a
// layout change that broke it would silently shift every record.
const _: () = assert!(core::mem::size_of::<Utmpx>() == UT_RECORD_SIZE);

/// Copy `src` into a fixed-width, NUL-padded field.
fn set_field(dst: &mut [u8], src: &[u8]) {
    dst.fill(0);
    let n = core::cmp::min(dst.len() - 1, src.len());
    dst[..n].copy_from_slice(&src[..n]);
}

impl Utmpx {
    /// A record of `kind` for `pid` on `line`, in session `user`.
    pub fn new(kind: u16, pid: i32, user: &[u8], line: &[u8]) -> Self {
        let mut rec = EMPTY_RECORD;
        rec.ut_type = kind;
        rec.ut_pid = pid;
        set_field(&mut rec.ut_user, user);
        set_field(&mut rec.ut_line, line);
        set_field(&mut rec.ut_id, line);
        rec
    }

    /// Stamp the entry with `sec`/`usec` since the epoch.
    pub fn at(mut self, sec: i64, usec: i64) -> Self {
        self.ut_tv.tv_sec = sec;
        self.ut_tv.tv_usec = usec;
        self
    }

    /// The entry as it is stored: `UT_RECORD_SIZE` bytes of this struct.
    ///
    /// There is no padding hole in `struct utmpx`, so the image is exactly
    /// the fields; the cast is a reborrow of the caller's value, not a copy
    /// into foreign memory.
    pub fn as_bytes(&self) -> &[u8; UT_RECORD_SIZE] {
        // SAFETY: `Utmpx` is `repr(C)` and sized `UT_RECORD_SIZE` by the
        // assertion above.
        unsafe { &*(self as *const Self).cast::<[u8; UT_RECORD_SIZE]>() }
    }

    /// The entry stored in the first `UT_RECORD_SIZE` bytes of `src`.
    pub fn from_bytes(src: &[u8]) -> Option<Self> {
        if src.len() < UT_RECORD_SIZE {
            return None;
        }
        let mut rec = EMPTY_RECORD;
        // SAFETY: `rec` is a `Utmpx` and `src` has at least its size; the
        // source might not be aligned, so the copy is unaligned.
        unsafe {
            core::ptr::copy_nonoverlapping(
                src.as_ptr(),
                core::ptr::addr_of_mut!(rec).cast::<u8>(),
                UT_RECORD_SIZE,
            );
        }
        Some(rec)
    }

    /// The NUL-terminated user field as bytes.
    pub fn user(&self) -> &[u8] {
        let n = self
            .ut_user
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(UT_NAMESIZE);
        &self.ut_user[..n]
    }

    /// The NUL-terminated line field as bytes, with `/dev/` dropped.
    pub fn line(&self) -> &[u8] {
        let n = self
            .ut_line
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(UT_LINESIZE);
        &self.ut_line[..n]
    }
}

/// Read the records at `path` into `out`, returning how many were read.
///
/// A file whose length is not a whole number of records is truncated to the
/// whole records it holds; an absent file is `ENOENT`, which callers that
/// merely report a count treat as "no records".
pub fn read_records(path: &[u8], out: &mut [Utmpx]) -> Result<usize, crate::MinixErr> {
    let mut file = [0u8; UT_RECORD_SIZE * MAX_RECORDS];
    let n = unsafe { read_file(path, &mut file) }?;
    let count = core::cmp::min(n / UT_RECORD_SIZE, out.len());
    for (i, slot) in out.iter_mut().take(count).enumerate() {
        let off = i * UT_RECORD_SIZE;
        *slot = Utmpx::from_bytes(&file[off..off + UT_RECORD_SIZE]).unwrap_or(EMPTY_RECORD);
    }
    Ok(count)
}

/// Replace the file at `path` with `recs`.
pub fn write_records(path: &[u8], recs: &[Utmpx]) -> Result<(), crate::MinixErr> {
    let mut file = [0u8; UT_RECORD_SIZE * MAX_RECORDS];
    let count = core::cmp::min(recs.len(), MAX_RECORDS);
    for (i, rec) in recs.iter().take(count).enumerate() {
        let off = i * UT_RECORD_SIZE;
        file[off..off + UT_RECORD_SIZE].copy_from_slice(rec.as_bytes());
    }
    unsafe { write_file(path, &file[..count * UT_RECORD_SIZE]) }
}

/// Insert `rec` into the first `n` entries of `recs`, replacing the entry for
/// the same pid and kind, or appending. Returns the new entry count.
///
/// A full slice drops its oldest `DEAD_PROCESS` entry first, and then its oldest
/// entry: the record being written is always kept.
fn insert_record(recs: &mut [Utmpx], n: usize, rec: &Utmpx) -> usize {
    let mut n = n;
    for slot in recs.iter_mut().take(n) {
        if slot.ut_pid == rec.ut_pid && slot.ut_type == rec.ut_type {
            *slot = *rec;
            return n;
        }
    }
    if n >= MAX_RECORDS {
        let victim = recs[..n]
            .iter()
            .position(|r| r.ut_type == DEAD_PROCESS)
            .unwrap_or(0);
        recs.copy_within(victim + 1..n, victim);
        n -= 1;
    }
    recs[n] = *rec;
    n + 1
}

/// Turn the live session `pid` in the first `n` entries into `DEAD_PROCESS`.
/// True when a record was changed.
fn mark_dead(recs: &mut [Utmpx], n: usize, pid: i32) -> bool {
    let mut found = false;
    for slot in recs.iter_mut().take(n) {
        if slot.ut_pid == pid && slot.ut_type == USER_PROCESS {
            slot.ut_type = DEAD_PROCESS;
            found = true;
        }
    }
    found
}

/// Insert `rec` into the file at `path` (see [`insert_record`]).
pub fn put_record(path: &[u8], rec: &Utmpx) -> Result<(), crate::MinixErr> {
    let mut recs = [EMPTY_RECORD; MAX_RECORDS];
    let n = read_records(path, &mut recs).unwrap_or(0);
    let n = insert_record(&mut recs, n, rec);
    write_records(path, &recs[..n])
}

/// Mark the live session `pid` as ended (`DEAD_PROCESS`), keeping its time.
///
/// A pid with no record is not an error: a shell that never registered one (a
/// subshell, or a shell on a system whose session file could not be written)
/// simply has nothing to retire.
pub fn retire_record(path: &[u8], pid: i32) -> Result<(), crate::MinixErr> {
    let mut recs = [EMPTY_RECORD; MAX_RECORDS];
    let n = read_records(path, &mut recs)?;
    if mark_dead(&mut recs, n, pid) {
        write_records(path, &recs[..n])?;
    }
    Ok(())
}

/// The whole file at `path` into `buf`, stopping at its end or `buf`'s.
#[cfg(target_os = "minix")]
unsafe fn read_file(path: &[u8], buf: &mut [u8]) -> Result<usize, crate::MinixErr> {
    let fd = unsafe { crate::fs::open(path, crate::fs::O_RDONLY, 0) }?;
    let mut total = 0usize;
    loop {
        let n = match unsafe { crate::fs::read(fd, &mut buf[total..]) } {
            Ok(n) => n,
            Err(e) => {
                let _ = crate::fs::close(fd);
                return Err(e);
            }
        };
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

/// Replace `path`'s contents with `data`.
#[cfg(target_os = "minix")]
unsafe fn write_file(path: &[u8], data: &[u8]) -> Result<(), crate::MinixErr> {
    let flags = crate::fs::O_WRONLY | crate::fs::O_CREAT | crate::fs::O_TRUNC;
    let fd = unsafe { crate::fs::open(path, flags, 0o644) }?;
    let mut written = 0usize;
    while written < data.len() {
        match unsafe { crate::fs::write(fd, &data[written..]) } {
            Ok(n) if n > 0 => written += n as usize,
            Ok(_) => {
                let _ = crate::fs::close(fd);
                return Err(crate::MinixErr::EIO);
            }
            Err(e) => {
                let _ = crate::fs::close(fd);
                return Err(e);
            }
        }
    }
    let _ = crate::fs::close(fd);
    Ok(())
}

#[cfg(not(target_os = "minix"))]
unsafe fn read_file(_path: &[u8], _buf: &mut [u8]) -> Result<usize, crate::MinixErr> {
    Err(crate::MinixErr::ENOSYS)
}

#[cfg(not(target_os = "minix"))]
unsafe fn write_file(_path: &[u8], _data: &[u8]) -> Result<(), crate::MinixErr> {
    Err(crate::MinixErr::ENOSYS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_size_is_the_file_stride() {
        assert_eq!(UT_RECORD_SIZE, 520);
        assert_eq!(core::mem::size_of::<Utmpx>(), UT_RECORD_SIZE);
    }

    #[test]
    fn a_record_survives_the_round_trip() {
        let rec = Utmpx::new(USER_PROCESS, 42, b"root", b"tty").at(1_700_000_000, 500_000);
        let bytes = *rec.as_bytes();
        let back = Utmpx::from_bytes(&bytes).expect("a full record");
        assert_eq!(back.ut_type, USER_PROCESS);
        assert_eq!(back.ut_pid, 42);
        assert_eq!(back.user(), b"root");
        assert_eq!(back.line(), b"tty");
        assert_eq!(back.ut_tv.tv_sec, 1_700_000_000);
        assert_eq!(back.ut_tv.tv_usec, 500_000);
    }

    #[test]
    fn a_short_buffer_is_not_a_record() {
        assert!(Utmpx::from_bytes(&[0u8; UT_RECORD_SIZE - 1]).is_none());
    }

    #[test]
    fn fields_are_nul_terminated_and_truncated() {
        let long = [b'x'; 40];
        let rec = Utmpx::new(USER_PROCESS, 1, &long, &long);
        assert_eq!(rec.user().len(), UT_NAMESIZE - 1);
        assert_eq!(rec.ut_user[UT_NAMESIZE - 1], 0);
        assert_eq!(rec.line().len(), UT_LINESIZE - 1);
        assert_eq!(rec.ut_line[UT_LINESIZE - 1], 0);
    }

    #[test]
    fn putting_a_record_twice_replaces_it() {
        let mut recs = [EMPTY_RECORD; MAX_RECORDS];
        let first = Utmpx::new(USER_PROCESS, 7, b"root", b"tty");
        let n = insert_record(&mut recs, 0, &first);
        assert_eq!(n, 1);
        let second = Utmpx::new(USER_PROCESS, 7, b"root", b"tty").at(9, 0);
        let n = insert_record(&mut recs, n, &second);
        assert_eq!(n, 1, "the same pid and kind must not append");
        assert_eq!(recs[0].ut_tv.tv_sec, 9);
    }

    #[test]
    fn a_full_file_drops_a_dead_entry_before_a_live_one() {
        let mut recs = [EMPTY_RECORD; MAX_RECORDS];
        for (i, slot) in recs.iter_mut().enumerate() {
            let kind = if i == 3 { DEAD_PROCESS } else { USER_PROCESS };
            *slot = Utmpx::new(kind, i as i32 + 1, b"root", b"tty");
        }
        let n = insert_record(&mut recs, MAX_RECORDS, &Utmpx::new(BOOT_TIME, 99, b"", b""));
        assert_eq!(n, MAX_RECORDS);
        assert!(
            !recs
                .iter()
                .any(|r| r.ut_pid == 4 && r.ut_type == DEAD_PROCESS)
        );
        assert!(
            recs.iter()
                .any(|r| r.ut_pid == 99 && r.ut_type == BOOT_TIME)
        );
    }

    #[test]
    fn retiring_needs_a_live_user_process() {
        let mut recs = [EMPTY_RECORD; MAX_RECORDS];
        recs[0] = Utmpx::new(USER_PROCESS, 5, b"root", b"tty");
        assert!(mark_dead(&mut recs, 1, 5));
        assert_eq!(recs[0].ut_type, DEAD_PROCESS);
        assert!(!mark_dead(&mut recs, 1, 5), "already retired");
        assert!(!mark_dead(&mut recs, 1, 6), "no such pid");
    }
}
