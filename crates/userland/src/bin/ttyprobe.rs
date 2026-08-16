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
    // PTY round-trip probe (Phase M2): open a pty pair, push bytes both
    // ways through the tty line discipline, and verify they arrive.
    //   master write "ab\n"  → echo on the master, line on the slave
    //   slave  write "XY"    → bytes on the master
    let master = unsafe { minix_std::fs::open(b"/dev/ptyp0", minix_std::fs::O_RDWR, 0) };
    let master = match master {
        Ok(fd) => fd,
        Err(_) => {
            userland::write_err(b"ttyprobe: open /dev/ptyp0 failed\n");
            return 1;
        }
    };
    let slave = unsafe { minix_std::fs::open(b"/dev/ttyp0", minix_std::fs::O_RDWR, 0) };
    let slave = match slave {
        Ok(fd) => fd,
        Err(_) => {
            userland::write_err(b"ttyprobe: open /dev/ttyp0 failed\n");
            return 1;
        }
    };

    // Master → slave: the line lands in the slave's input queue and the
    // echo (ECHO is on by default) lands on the master's output buffer.
    if unsafe { minix_std::fs::write(master, b"ab\n") }.is_err() {
        userland::write_err(b"ttyprobe: master write failed\n");
        return 1;
    }
    let mut echo = [0u8; 16];
    let n = unsafe { minix_std::fs::read(master, &mut echo) };
    let n = match n {
        Ok(n) => n as usize,
        Err(_) => {
            userland::write_err(b"ttyprobe: echo read failed\n");
            return 1;
        }
    };
    if n == 0 || echo[0] != b'a' {
        userland::write_err(b"ttyprobe: echo mismatch\n");
        return 1;
    }
    let mut line = [0u8; 16];
    let m = unsafe { minix_std::fs::read(slave, &mut line) };
    let m = match m {
        Ok(m) => m as usize,
        Err(_) => {
            userland::write_err(b"ttyprobe: slave read failed\n");
            return 1;
        }
    };
    if m < 3 || line[0] != b'a' || line[1] != b'b' || line[2] != b'\n' {
        userland::write_err(b"ttyprobe: slave line mismatch\n");
        return 1;
    }

    // Slave → master: the output buffer carries the bytes to the master.
    if unsafe { minix_std::fs::write(slave, b"XY") }.is_err() {
        userland::write_err(b"ttyprobe: slave write failed\n");
        return 1;
    }
    let mut out = [0u8; 16];
    let k = unsafe { minix_std::fs::read(master, &mut out) };
    let k = match k {
        Ok(k) => k as usize,
        Err(_) => {
            userland::write_err(b"ttyprobe: master read failed\n");
            return 1;
        }
    };
    if k != 2 || out[0] != b'X' || out[1] != b'Y' {
        userland::write_err(b"ttyprobe: slave->master mismatch\n");
        return 1;
    }

    let _ = minix_std::fs::close(master);
    let _ = minix_std::fs::close(slave);
    userland::write_out(b"ttyprobe: PASS\n");
    0
}
