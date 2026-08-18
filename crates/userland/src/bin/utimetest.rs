//! utime smoke test — `/bin/utimetest`.
//!
//! Exercises the VFS_UTIMENS → MFS `fs_utime` chain through
//! `minix_std::fs::utime`: explicit times, UTIME_OMIT, UTIME_NOW, and
//! ENOENT for a missing file. Expect `utimetest: OK` and exit 0.

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

const PATH: &str = "utimetest.dat";

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // Create + write + close.
    let fd = match unsafe {
        minix_std::fs::open(
            PATH.as_bytes(),
            minix_std::fs::O_CREAT | minix_std::fs::O_WRONLY,
            0o644,
        )
    } {
        Ok(fd) => fd,
        Err(_) => {
            userland::write_err(b"utimetest: create failed\n");
            return 1;
        }
    };
    if unsafe { minix_std::fs::write(fd, b"x") }.is_err() {
        userland::write_err(b"utimetest: write failed\n");
        return 2;
    }
    if minix_std::fs::close(fd).is_err() {
        userland::write_err(b"utimetest: close failed\n");
        return 3;
    }

    // Explicit times are stored at second resolution.
    if unsafe { minix_std::fs::utime(PATH.as_bytes(), 1234, 5678, 0, 0) }.is_err() {
        userland::write_err(b"utimetest: utime explicit failed\n");
        return 4;
    }
    let st = match minix_std::fs::stat(PATH) {
        Ok(st) => st,
        Err(_) => {
            userland::write_err(b"utimetest: stat failed\n");
            return 5;
        }
    };
    if st.st_atime != 1234 || st.st_mtime != 5678 {
        userland::write_err(b"utimetest: bad explicit times\n");
        return 6;
    }

    // UTIME_OMIT leaves the mtime field alone.
    if unsafe { minix_std::fs::utime(PATH.as_bytes(), 9999, 0, 0, minix_std::fs::UTIME_OMIT) }
        .is_err()
    {
        userland::write_err(b"utimetest: utime omit failed\n");
        return 7;
    }
    let st = match minix_std::fs::stat(PATH) {
        Ok(st) => st,
        Err(_) => {
            userland::write_err(b"utimetest: stat failed\n");
            return 8;
        }
    };
    if st.st_atime != 9999 || st.st_mtime != 5678 {
        userland::write_err(b"utimetest: bad omit\n");
        return 9;
    }

    // UTIME_NOW stamps both fields with the current clock.
    if unsafe {
        minix_std::fs::utime(
            PATH.as_bytes(),
            0,
            0,
            minix_std::fs::UTIME_NOW,
            minix_std::fs::UTIME_NOW,
        )
    }
    .is_err()
    {
        userland::write_err(b"utimetest: utime now failed\n");
        return 10;
    }
    let st = match minix_std::fs::stat(PATH) {
        Ok(st) => st,
        Err(_) => {
            userland::write_err(b"utimetest: stat failed\n");
            return 11;
        }
    };
    if st.st_mtime <= 5678 {
        userland::write_err(b"utimetest: bad now\n");
        return 12;
    }

    // Missing file → ENOENT.
    if unsafe { minix_std::fs::utime(b"no-such-file-utimetest", 1, 2, 0, 0) }.is_ok() {
        userland::write_err(b"utimetest: missing file accepted\n");
        return 13;
    }

    userland::write_out(b"utimetest: OK\n");
    0
}
