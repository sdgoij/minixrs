//! pty select smoke test — `/bin/ptytest`.
//!
//! Proves a *blocking* `poll` on a pty master comes back when the slave writes.
//! That readiness is reported by the tty server — the master's output buffer
//! gaining bytes wakes the waiter through VFS — and before it was wired nothing
//! woke a master poller at all, so `/bin/wterm` drained the master in an `EAGAIN`
//! loop instead and spun. A poll with no deadline is therefore the assertion: if
//! the wake is missing or lost, nothing else will produce it.
//!
//! The master is opened before the fork so the child's slave open has a live pair,
//! which is what `/bin/wterm` does too. The child then spins before writing, so the
//! parent is blocked in its poll rather than scanning a buffer that already holds
//! the byte; that only makes the test stronger, since a poll satisfied at scan time
//! is still a pass.
//!
//! The child then *waits* rather than exiting. A pty pair with both ends closed is
//! reset, and a reset discards the output buffer — so a child that exited as soon
//! as it had written would take the byte with it (measured: the poll came back, the
//! read found nothing). The parent releases it down a pipe once the byte is read.
//!
//! The poll carries a long deadline rather than none, so a missing wake fails the
//! gate in seconds instead of hanging it. Failure codes: 1 master open, 2 pipe,
//! 3 fork, 4 poll failed, 5 never woken, 6 read failed, 7 wrong byte.

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

const POLLIN: i16 = 0x001;
/// What the child writes. Distinctive so a stale buffer cannot satisfy the read.
const BYTE: u8 = b'P';
/// Long enough that a missing wake is a clean failure, short enough that a real
/// wake is plainly not the deadline.
const TIMEOUT_MS: i32 = 5000;
/// Delay the child by more than the few syscalls the parent needs to reach its
/// poll, so the wake under test is the one that ends it.
const CHILD_SPIN: u32 = 500_000;

fn fail(code: i32, msg: &[u8]) -> i32 {
    userland::write_err(msg);
    code
}

/// The slave half. `master` is inherited and deliberately left open: closing it
/// here would signal the slave (`master_close` sends SIGHUP) before it has written
/// anything. `ack_r` releases the child once the parent has read.
fn child(ack_r: i32) -> ! {
    for _ in 0..CHILD_SPIN {
        core::hint::spin_loop();
    }
    // `minix_rt::open` rather than `fs::open`, mirroring `/bin/wterm`'s shell child.
    let fd = minix_rt::open(b"/dev/ttyp0", 0o2) as i32; // O_RDWR
    if fd < 0 {
        userland::write_err(b"ptytest: child could not open /dev/ttyp0\n");
        minix_std::process::exit(1);
    }
    let _ = unsafe { minix_std::fs::write(fd, &[BYTE]) };

    let mut ack = [0u8; 1];
    let _ = unsafe { minix_std::fs::read(ack_r, &mut ack) };
    let _ = minix_std::fs::close(fd);
    minix_std::process::exit(0);
}

fn run() -> Result<(), i32> {
    let master = match unsafe { minix_std::fs::open(b"/dev/ptyp0", minix_std::fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => return Err(fail(1, b"ptytest: open /dev/ptyp0 failed\n")),
    };
    let (ack_r, ack_w) = match minix_std::fs::pipe() {
        Ok(p) => p,
        Err(_) => return Err(fail(2, b"ptytest: pipe failed\n")),
    };

    let pid = match unsafe { minix_std::process::fork() } {
        Ok(0) => child(ack_r),
        Ok(p) => p,
        Err(_) => return Err(fail(3, b"ptytest: fork failed\n")),
    };
    let _ = minix_std::fs::close(ack_r);

    let mut fds = [minix_std::fs::PollFd {
        fd: master,
        events: POLLIN,
        revents: 0,
    }];
    match minix_std::fs::poll(&mut fds, TIMEOUT_MS) {
        Ok(n) if n > 0 && fds[0].revents & POLLIN != 0 => {}
        Ok(_) => return Err(fail(5, b"ptytest: the master poll was never woken\n")),
        Err(_) => return Err(fail(4, b"ptytest: poll failed\n")),
    }

    let mut buf = [0u8; 8];
    let got = match unsafe { minix_std::fs::read(master, &mut buf) } {
        Ok(n) if n > 0 => n as usize,
        _ => return Err(fail(6, b"ptytest: read after the wake failed\n")),
    };
    if got != 1 || buf[0] != BYTE {
        return Err(fail(7, b"ptytest: not the byte the slave wrote\n"));
    }

    // Release the child, now that the byte is out of the output buffer.
    let _ = unsafe { minix_std::fs::write(ack_w, b".") };
    let _ = minix_std::process::waitpid(pid, 0);
    let _ = minix_std::fs::close(ack_w);
    let _ = minix_std::fs::close(master);
    Ok(())
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    match run() {
        Ok(()) => {
            userland::write_out(b"ptytest: PASS\n");
            0
        }
        Err(code) => code,
    }
}
