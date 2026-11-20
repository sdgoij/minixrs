//! Pipe/PFS smoke test — `/bin/pipetest`.
//!
//! Exercises the PFS-backed pipe data plane through VFS: `pipe()` creates a
//! named-pipe inode on PFS, `fstat` on the fds must report S_IFIFO with
//! size 0 (routing to PFS `fs_stat`), bytes flow write→read through the
//! block-cache buffer, and `ftruncate`/`fchmod` on the fds round-trip to
//! PFS `fs_ftrunc`/`fs_chmod` instead of erroring.
//!
//! It is also the gate for a pipe that *blocks* (KNOWN_ISSUES.md item 32): a read of an
//! empty pipe with a writer open, and a write that does not fit, both wait for the other
//! end instead of answering `EAGAIN`. Each case forks, and the other side deliberately
//! does not move for a moment, so the waiting is what makes the exchange work rather than
//! an accident of scheduling. Expect `pipetest: OK` and exit 0.

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(argc: i32, argv: *const *const u8) -> i32 {
    // `pipetest fds <path>` writes this process's open descriptors to `path` instead of running
    // the suite. It is how KNOWN_ISSUES 79's pipeline state was read: a probe started as a
    // pipeline stage reports what the shell's wiring actually gave it.
    let mut arg_buf = [""; 64];
    let args = unsafe { userland::parse_args(argc, argv, &mut arg_buf) };
    match args.get(1).copied() {
        Some("fds") => return dump_fds(args.get(2).copied()),
        // An argument this binary does not know is a caller's mistake, not a reason to run
        // the whole suite and report `pipetest: OK` for it.
        Some(other) if !other.is_empty() => {
            userland::write_err(b"pipetest: unknown mode\n");
            return 61;
        }
        _ => {}
    }

    let (r, w) = match minix_std::fs::pipe() {
        Ok(p) => p,
        Err(_) => {
            userland::write_err(b"pipetest: pipe failed\n");
            return 1;
        }
    };

    // fstat on a pipe fd must reach PFS fs_stat: FIFO mode, zero size.
    let st = match minix_std::fs::fstat(r) {
        Ok(st) => st,
        Err(_) => {
            userland::write_err(b"pipetest: fstat failed\n");
            return 2;
        }
    };
    if st.st_mode & minix_std::fs::S_IFMT != minix_std::fs::S_IFIFO {
        userland::write_err(b"pipetest: pipe fd is not S_IFIFO\n");
        return 3;
    }
    if st.st_size != 0 {
        userland::write_err(b"pipetest: fresh pipe not empty\n");
        return 4;
    }

    // Data plane: bytes go through the PFS block-cache buffer.
    let n = unsafe { minix_std::fs::write(w, b"hello") };
    if n != Ok(5) {
        userland::write_err(b"pipetest: write failed\n");
        return 5;
    }
    let mut buf = [0u8; 8];
    let n = unsafe { minix_std::fs::read(r, &mut buf) };
    if n != Ok(5) || &buf[..5] != b"hello" {
        userland::write_err(b"pipetest: read failed\n");
        return 6;
    }

    // ftruncate on the pipe fd routes to PFS fs_ftrunc (size 0 is valid).
    if minix_std::fs::truncate(w, 0).is_err() {
        userland::write_err(b"pipetest: ftruncate failed\n");
        return 7;
    }
    // A nonzero ftruncate on a pipe must be rejected (pipes only truncate
    // to 0) — pins the req_ftrunc arg order (trc_start = new size).
    if minix_std::fs::truncate(w, 5).is_ok() {
        userland::write_err(b"pipetest: pipe ftruncate to nonzero accepted\n");
        return 11;
    }

    // fchmod on the pipe fd routes to PFS fs_chmod; fstat must show it.
    if minix_std::fs::fchmod(r, 0o640).is_err() {
        userland::write_err(b"pipetest: fchmod failed\n");
        return 8;
    }
    let st = match minix_std::fs::fstat(r) {
        Ok(st) => st,
        Err(_) => {
            userland::write_err(b"pipetest: fstat after fchmod failed\n");
            return 9;
        }
    };
    if st.st_mode & 0o7777 != 0o640 {
        userland::write_err(b"pipetest: fchmod did not stick\n");
        return 10;
    }

    let _ = minix_std::fs::close(r);
    let _ = minix_std::fs::close(w);

    // Regular-file ftruncate to a smaller size must shrink the file (MFS
    // truncate_inode) — same req_ftrunc arg order on the MFS path.
    let f = match unsafe {
        minix_std::fs::open(
            b"pipetest.dat",
            minix_std::fs::O_CREAT | minix_std::fs::O_WRONLY,
            0o644,
        )
    } {
        Ok(f) => f,
        Err(_) => {
            userland::write_err(b"pipetest: file open failed\n");
            return 12;
        }
    };
    if unsafe { minix_std::fs::write(f, b"0123456789") } != Ok(10) {
        userland::write_err(b"pipetest: file write failed\n");
        return 13;
    }
    if minix_std::fs::truncate(f, 4).is_err() {
        userland::write_err(b"pipetest: file ftruncate failed\n");
        return 14;
    }
    let st = match minix_std::fs::fstat(f) {
        Ok(st) => st,
        Err(_) => {
            userland::write_err(b"pipetest: file fstat failed\n");
            return 15;
        }
    };
    if st.st_size != 4 {
        userland::write_err(b"pipetest: file not truncated to 4\n");
        return 16;
    }
    let _ = minix_std::fs::close(f);

    // Named pipe via a directory entry (Stage 3): mknod a FIFO, then open
    // both ends — the data plane routes through the PFS-mapped inode while
    // the directory entry stays on MFS.
    if minix_rt::mknod(b"/pipetest.fifo", 0o10000, 0) < 0 {
        userland::write_err(b"pipetest: mknod fifo failed\n");
        return 17;
    }
    // O_RDONLY|O_NONBLOCK read end opens with no writer present.
    let fr = match unsafe { minix_std::fs::open(b"/pipetest.fifo", minix_std::fs::O_NONBLOCK, 0) } {
        Ok(fr) => fr,
        Err(e) => {
            // Report the errno so a boot probe can pin down the failing step.
            let mut m = [0u8; 48];
            let msg = b"pipetest: fifo read open errno=";
            m[..msg.len()].copy_from_slice(msg);
            let mut v = e.0.unsigned_abs();
            let mut i = msg.len();
            loop {
                m[i] = b'0' + (v % 10) as u8;
                v /= 10;
                i += 1;
                if v == 0 {
                    break;
                }
            }
            m[i] = b'\n';
            userland::write_err(&m[..i + 1]);
            return 18;
        }
    };
    // O_WRONLY|O_NONBLOCK write end opens once a reader is present.
    let fw = match unsafe {
        minix_std::fs::open(
            b"/pipetest.fifo",
            minix_std::fs::O_WRONLY | minix_std::fs::O_NONBLOCK,
            0,
        )
    } {
        Ok(fw) => fw,
        Err(_) => {
            userland::write_err(b"pipetest: fifo write open failed\n");
            return 19;
        }
    };
    // Data flows through the PFS-mapped inode.
    if unsafe { minix_std::fs::write(fw, b"fifo-hello") } != Ok(10) {
        userland::write_err(b"pipetest: fifo write failed\n");
        return 20;
    }
    let mut fbuf = [0u8; 16];
    let n = unsafe { minix_std::fs::read(fr, &mut fbuf) };
    if n != Ok(10) || &fbuf[..10] != b"fifo-hello" {
        userland::write_err(b"pipetest: fifo read failed\n");
        return 21;
    }
    // fstat on the FIFO fd reports S_IFIFO (the entry's mode from MFS).
    let st = match minix_std::fs::fstat(fr) {
        Ok(st) => st,
        Err(_) => {
            userland::write_err(b"pipetest: fifo fstat failed\n");
            return 22;
        }
    };
    if st.st_mode & minix_std::fs::S_IFMT != minix_std::fs::S_IFIFO {
        userland::write_err(b"pipetest: fifo not S_IFIFO\n");
        return 23;
    }
    let _ = minix_std::fs::close(fr);
    let _ = minix_std::fs::close(fw);
    // After the last close the pipe data is gone: a fresh open reads EOF
    // (no writer), not the old bytes.
    let fr2 = match unsafe { minix_std::fs::open(b"/pipetest.fifo", minix_std::fs::O_NONBLOCK, 0) }
    {
        Ok(fr2) => fr2,
        Err(_) => {
            userland::write_err(b"pipetest: fifo reopen failed\n");
            return 24;
        }
    };
    let n = unsafe { minix_std::fs::read(fr2, &mut fbuf) };
    if n != Ok(0) {
        userland::write_err(b"pipetest: fifo data persisted after close\n");
        return 25;
    }
    let _ = minix_std::fs::close(fr2);
    let _ = minix_rt::unlink(b"/pipetest.fifo");

    if let Some(code) = blocking_cases() {
        return code;
    }

    userland::write_out(b"pipetest: OK\n");
    0
}

/// Write this process's open descriptors to `path`, one line each.
///
/// `pipetest fds <path>` is how a probe started as a pipeline stage reports what it inherited:
/// the file type says whether fd 0 is the pipe or the console, and an fd above 2 that is still
/// open is one the caller did not release. The trailing write checks tell the two pipe ends
/// apart, because a write to a read end is refused and a write to a write end is not.
#[cfg(target_os = "minix")]
fn dump_fds(path: Option<&str>) -> i32 {
    let Some(path) = path else {
        userland::write_err(b"pipetest: fds needs a path\n");
        return 1;
    };
    let fd = match unsafe {
        minix_std::fs::open(
            path.as_bytes(),
            minix_std::fs::O_CREAT | minix_std::fs::O_WRONLY | minix_std::fs::O_TRUNC,
            0o644,
        )
    } {
        Ok(fd) => fd,
        Err(_) => {
            userland::write_err(b"pipetest: fds open failed\n");
            return 2;
        }
    };
    let mut line = [0u8; 64];
    for i in 0..64i32 {
        let Ok(st) = minix_std::fs::fstat(i) else {
            continue;
        };
        let mut n = 0usize;
        let head = b"fd ";
        line[..head.len()].copy_from_slice(head);
        n += head.len();
        n = put_dec(&mut line, n, i);
        let mid = b" mode ";
        line[n..n + mid.len()].copy_from_slice(mid);
        n += mid.len();
        n = put_dec(&mut line, n, (st.st_mode & 0o170000) as i32);
        let ino = b" ino ";
        line[n..n + ino.len()].copy_from_slice(ino);
        n += ino.len();
        n = put_dec(&mut line, n, st.st_ino as i32);
        line[n] = b'\n';
        n += 1;
        let _ = unsafe { minix_std::fs::write(fd, &line[..n]) };
    }
    // Direction check: a write to a pipe's read end is `EBADF`, so a write that succeeds means
    // the descriptor is the write end of the very pipe it was supposed to read.
    let w0 = unsafe { minix_std::fs::write(0, b"Z") };
    let mut line = [0u8; 48];
    let mut n = 0usize;
    let head = b"write0 ";
    line[..head.len()].copy_from_slice(head);
    n += head.len();
    n = put_dec(&mut line, n, w0.map(|v| v as i32).unwrap_or(-1));
    line[n] = b'\n';
    n += 1;
    let _ = unsafe { minix_std::fs::write(fd, &line[..n]) };

    let _ = minix_std::fs::close(fd);
    0
}

#[cfg(not(target_os = "minix"))]
fn dump_fds(_path: Option<&str>) -> i32 {
    0
}

/// Append `v` in decimal to `buf` at `at`, returning the new length.
#[cfg(target_os = "minix")]
fn put_dec(buf: &mut [u8], at: usize, v: i32) -> usize {
    let mut n = at;
    let mut digits = [0u8; 12];
    let mut k = 0usize;
    let mut x = v.unsigned_abs();
    loop {
        digits[k] = b'0' + (x % 10) as u8;
        x /= 10;
        k += 1;
        if x == 0 {
            break;
        }
    }
    if v < 0 {
        buf[n] = b'-';
        n += 1;
    }
    while k > 0 {
        k -= 1;
        buf[n] = digits[k];
        n += 1;
    }
    n
}

/// Busy-wait `ms` of the monotonic clock. True when the clock was readable.
///
/// The port has no `nanosleep` (it answers `ENOSYS`), and both cases below only need the
/// *other* side to stand still for a moment, so a spin on a clock that already works is
/// enough: an implementation that skipped the delay has to fail rather than pass, and the
/// caller checks that it did not.
#[cfg(target_os = "minix")]
fn spin_ms(ms: i64) -> bool {
    use minix_std::time::{CLOCK_MONOTONIC, clock_gettime};
    let Ok(start) = clock_gettime(CLOCK_MONOTONIC) else {
        return false;
    };
    loop {
        let Ok(now) = clock_gettime(CLOCK_MONOTONIC) else {
            return false;
        };
        let ms_secs = (now.tv_sec - start.tv_sec) * 1000;
        let ms_nanos = (now.tv_nsec - start.tv_nsec) / 1_000_000;
        if ms_secs + ms_nanos >= ms {
            return true;
        }
    }
}



/// Say which errno a `minix_std` result carried, for a failure a boot gate has to report.
#[cfg(target_os = "minix")]
fn write_errno(prefix: &[u8], e: minix_std::MinixErr) {
    let mut msg = [0u8; 64];
    let n = prefix.len().min(48);
    msg[..n].copy_from_slice(&prefix[..n]);
    let mut i = n;
    let mut v = e.0 as i64;
    if v < 0 {
        msg[i] = b'-';
        i += 1;
        v = -v;
    }
    let mut digits = [0u8; 20];
    let mut k = 0usize;
    loop {
        digits[k] = b'0' + (v % 10) as u8;
        v /= 10;
        k += 1;
        if v == 0 {
            break;
        }
    }
    while k > 0 {
        k -= 1;
        msg[i] = digits[k];
        i += 1;
    }
    msg[i] = b'\n';
    userland::write_err(&msg[..i + 1]);
}

/// The 20 KB a blocked writer has to push through a 7 KB pipe.
#[cfg(target_os = "minix")]
static BIG: [u8; 20_000] = [b'x'; 20_000];

/// How long the other end stands still, comfortably longer than the microseconds it takes
/// this process to reach the call that must wait, and short enough not to slow the gate.
#[cfg(target_os = "minix")]
const STILL_MS: i64 = 200;

/// A read that waits for its writer, and a write that waits for its reader.
///
/// Returns an exit code on failure, `None` when both exchanges completed.
#[cfg(target_os = "minix")]
fn blocking_cases() -> Option<i32> {
    use minix_std::fs::{close, pipe, read, write};

    // The reader must get the mark that only arrives after it has started waiting. The child
    // reports whether it managed to stand still first, so a broken clock cannot make this
    // pass by accident: without the delay the mark may already be there.
    let (br, bw) = match pipe() {
        Ok(p) => p,
        Err(_) => {
            userland::write_err(b"pipetest: blocking pipe failed\n");
            return Some(26);
        }
    };
    let pid = minix_rt::fork();
    if pid == 0 {
        let _ = close(br);
        let waited = spin_ms(STILL_MS);
        let wrote = unsafe { write(bw, b"late") } == Ok(4);
        let _ = close(bw);
        minix_rt::exit(if waited && wrote { 0 } else { 1 });
    }
    if pid < 0 {
        userland::write_err(b"pipetest: fork failed\n");
        return Some(27);
    }
    let _ = close(bw);
    let mut late = [0u8; 8];
    let n = unsafe { read(br, &mut late) };
    let _ = close(br);
    if n != Ok(4) || &late[..4] != b"late" {
        userland::write_err(b"pipetest: blocking read did not wait for its writer\n");
        return Some(28);
    }
    if minix_rt::waitpid_status(pid) != 0 {
        userland::write_err(b"pipetest: the writer could not stand still first\n");
        return Some(29);
    }

    // The writer must push 20 000 bytes through a 7 168-byte pipe, so at least part of the
    // write has to wait for the reader, which does not start reading for a moment.
    let (wr, ww) = match pipe() {
        Ok(p) => p,
        Err(_) => {
            userland::write_err(b"pipetest: blocking pipe failed\n");
            return Some(30);
        }
    };
    let pid = minix_rt::fork();
    if pid == 0 {
        let _ = close(ww);
        let waited = spin_ms(STILL_MS);
        let mut buf = [0u8; 512];
        let mut total = 0usize;
        let mut failed = false;
        loop {
            match unsafe { read(wr, &mut buf) } {
                Ok(0) => break,
                Ok(n) => total += n as usize,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        let _ = close(wr);
        let ok = waited && !failed && total == BIG.len();
        minix_rt::exit(if ok { 0 } else { 1 });
    }
    if pid < 0 {
        userland::write_err(b"pipetest: fork failed\n");
        return Some(31);
    }
    let _ = close(wr);
    let mut off = 0usize;
    while off < BIG.len() {
        match unsafe { write(ww, &BIG[off..]) } {
            Ok(0) => break,
            Ok(n) => off += n as usize,
            Err(e) => {
                let _ = close(ww);
                write_errno(b"pipetest: blocking write was refused, errno ", e);
                return Some(32);
            }
        }
    }
    let _ = close(ww);
    if off != BIG.len() {
        userland::write_err(b"pipetest: the write stopped early\n");
        return Some(33);
    }
    if minix_rt::waitpid_status(pid) != 0 {
        userland::write_err(b"pipetest: the reader did not receive every byte\n");
        return Some(34);
    }

    None
}

#[cfg(not(target_os = "minix"))]
fn blocking_cases() -> Option<i32> {
    None
}
