#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// The console read timer (`MIN`/`TIME`): `VMIN = 0` with a nonzero `VTIME` makes a
/// raw-mode read return zero bytes once `VTIME` tenths of a second pass without
/// input, instead of blocking until a byte arrives. Poised on stdin so it exercises
/// the line the shell is really on.
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let mut saved = minix_std::termios::Termios::zeroed();
    if unsafe { minix_std::termios::tcgetattr(0, &mut saved) }.is_err() {
        userland::write_err(b"ttytimeout: tcgetattr failed\n");
        return 1;
    }

    // Two tenths of a second, no minimum byte count. ICANON off is what makes
    // MIN and TIME apply at all; ECHO off keeps the probe's own output clean.
    let mut raw = saved;
    raw.c_lflag &= !(minix_std::termios::ICANON | minix_std::termios::ECHO);
    raw.c_cc[minix_std::termios::VMIN] = 0;
    raw.c_cc[minix_std::termios::VTIME] = 2;
    if unsafe { minix_std::termios::tcsetattr(0, minix_std::termios::TIOCSETA, &raw) }.is_err() {
        userland::write_err(b"ttytimeout: tcsetattr failed\n");
        return 1;
    }

    let started = minix_std::time::clock_gettime(minix_std::time::CLOCK_MONOTONIC);
    let mut buf = [0u8; 8];
    let n = unsafe { minix_std::fs::read(0, &mut buf) };
    let elapsed = minix_std::time::clock_gettime(minix_std::time::CLOCK_MONOTONIC);

    // Restore the shell's terminal before reporting: leaving the console raw would
    // break every later step of the session.
    let restore = unsafe { minix_std::termios::tcsetattr(0, minix_std::termios::TIOCSETA, &saved) };

    let n = match n {
        Ok(n) => n,
        Err(_) => {
            userland::write_err(b"ttytimeout: read failed\n");
            return 1;
        }
    };
    if restore.is_err() {
        userland::write_err(b"ttytimeout: could not restore the terminal\n");
        return 1;
    }
    if n != 0 {
        userland::write_err(b"ttytimeout: the read returned a byte it was not given\n");
        return 1;
    }

    // The deadline is two tenths of a second; anything under one means the read
    // returned without waiting, and the pump makes it late by at most a few ticks.
    let (Ok(t0), Ok(t1)) = (started, elapsed) else {
        userland::write_err(b"ttytimeout: clock unavailable\n");
        return 1;
    };
    let ns = (t1.tv_sec - t0.tv_sec) * 1_000_000_000 + (t1.tv_nsec - t0.tv_nsec);
    if !(100_000_000..=2_000_000_000).contains(&ns) {
        userland::write_err(b"ttytimeout: the read returned outside the deadline window\n");
        return 1;
    }

    userland::write_out(b"ttytimeout: PASS\n");
    0
}
