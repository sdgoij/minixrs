//! PM_GETEPINFO smoke test — `/bin/eptest`.
//!
//! Queries PM itself (endpoint 0) through `minix_std::process::getepinfo`
//! (PM_GETEPINFO): PM runs as root, so the reply must be a positive pid
//! with uid/gid 0. Also verifies ESRCH for an unknown endpoint. Expect
//! `eptest: OK` and exit 0.

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
    // PM (endpoint 0) runs as root; its pid is assigned at boot.
    let (pid, uid, gid) = match unsafe { minix_std::process::getepinfo(0) } {
        Ok(v) => v,
        Err(_) => {
            userland::write_err(b"eptest: getepinfo(PM) failed\n");
            return 1;
        }
    };
    if pid <= 0 {
        userland::write_err(b"eptest: bad pid\n");
        return 2;
    }
    if uid != 0 || gid != 0 {
        userland::write_err(b"eptest: bad uid/gid\n");
        return 3;
    }
    // Unknown endpoint → ESRCH (negative reply).
    if unsafe { minix_std::process::getepinfo(0x7FFF) }.is_ok() {
        userland::write_err(b"eptest: bad endpoint accepted\n");
        return 4;
    }
    userland::write_out(b"eptest: OK\n");
    0
}
