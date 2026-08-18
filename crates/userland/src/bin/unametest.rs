//! uname smoke test — `/bin/unametest`.
//!
//! Fetches the PM_SYSUNAME fields through `minix_std::process::sysuname`
//! and verifies sysname/nodename/machine. The machine field must match the
//! architecture — the pre-J6 libc `uname()` hardcoded x86_64 regardless of
//! arch. Expect `unametest: OK` and exit 0.

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// Fetch one uname field into `buf`; returns an owned copy of the
/// NUL-stripped value so a later `write(2)` cannot corrupt the caller's
/// comparison buffer.
fn fetch(field: i32, buf: &mut [u8]) -> Option<[u8; 8]> {
    let n = unsafe { minix_std::process::sysuname(field, buf.as_mut_ptr(), buf.len()) }.ok()?;
    let s = &buf[..n.min(buf.len())];
    let s = s.strip_suffix(b"\0").unwrap_or(s);
    let mut out = [0u8; 8];
    let m = s.len().min(out.len());
    out[..m].copy_from_slice(&s[..m]);
    Some(out)
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let mut buf = [0u8; 65];

    if fetch(minix_std::process::UTS_SYSNAME, &mut buf) != Some(*b"Minix\0\0\0") {
        userland::write_err(b"unametest: bad sysname\n");
        return 1;
    }
    if fetch(minix_std::process::UTS_NODENAME, &mut buf) != Some(*b"minix\0\0\0") {
        userland::write_err(b"unametest: bad nodename\n");
        return 2;
    }
    let expected: [u8; 8] = if cfg!(target_arch = "x86_64") {
        *b"x86_64\0\0"
    } else if cfg!(target_arch = "riscv64") {
        *b"riscv64\0"
    } else {
        *b"aarch64\0"
    };
    if fetch(minix_std::process::UTS_MACHINE, &mut buf) != Some(expected) {
        userland::write_err(b"unametest: bad machine\n");
        return 3;
    }
    userland::write_out(b"unametest: OK\n");
    0
}
