//! Pipe/PFS smoke test — `/bin/pipetest`.
//!
//! Exercises the PFS-backed pipe data plane through VFS: `pipe()` creates a
//! named-pipe inode on PFS, `fstat` on the fds must report S_IFIFO with
//! size 0 (routing to PFS `fs_stat`), bytes flow write→read through the
//! block-cache buffer, and `ftruncate`/`fchmod` on the fds round-trip to
//! PFS `fs_ftrunc`/`fs_chmod` instead of erroring. Expect `pipetest: OK`
//! and exit 0.

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
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
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

    userland::write_out(b"pipetest: OK\n");
    0
}
