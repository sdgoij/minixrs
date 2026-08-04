#![no_std]
#![no_main]

use core::sync::atomic::{AtomicI32, Ordering};

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "none")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// Number of SIGINTs delivered. The handler runs, then sigreturn resumes the
/// interrupted pipe/tty read; the loop checks this counter and exits after 3
/// deliveries (SIGNALS.md Phase 4 verification).
static SIGINT_COUNT: AtomicI32 = AtomicI32::new(0);

/// A SIGINT handler: prints and returns. The interrupted read resumes via
/// the sigreturn trampoline.
///
/// # Safety
///
/// Installed as a raw signal handler; must not panic or unwind.
#[unsafe(no_mangle)]
unsafe extern "C" fn sigtest_handler(_sig: i32) {
    let n = SIGINT_COUNT.fetch_add(1, Ordering::Relaxed);
    if n >= 2 {
        let msg = b"sigtest: got 3, exiting\n";
        unsafe { minix_rt::write(1, msg.as_ptr(), msg.len()) };
    } else {
        let msg = b"sigtest: caught SIGINT\n";
        unsafe { minix_rt::write(1, msg.as_ptr(), msg.len()) };
    }
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let handler: unsafe extern "C" fn(i32) = sigtest_handler;
    let handler_addr = handler as usize as u64;
    if minix_std::time::sigaction(minix_std::time::SIGINT, handler_addr, 0, 0).is_err() {
        let msg = b"sigtest: cannot install handler\n";
        unsafe { minix_rt::write(1, msg.as_ptr(), msg.len()) };
        return 1;
    }
    let ready = b"sigtest: ready\n";
    unsafe { minix_rt::write(1, ready.as_ptr(), ready.len()) };

    // Read the console until 3 SIGINTs have been caught. ^C (or kill(pid,
    // SIGINT)) delivers to the handler; sigreturn resumes this read, which
    // the tty released with EINTR.
    loop {
        let mut buf = [0u8; 64];
        let _n = minix_rt::read(0, &mut buf);
        if SIGINT_COUNT.load(Ordering::Relaxed) >= 3 {
            let msg = b"sigtest: done\n";
            unsafe { minix_rt::write(1, msg.as_ptr(), msg.len()) };
            return 0;
        }
    }
}
