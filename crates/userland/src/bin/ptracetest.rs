//! ptrace smoke test — `/bin/ptracetest`.
//!
//! Classic tracee flow: fork; the child opts into tracing (PT_TRACE_ME),
//! stops itself with SIGSTOP, and exits 7; the parent (tracer) waitpids
//! the stop (W_STOPCODE(SIGSTOP)), resumes the child (PT_CONTINUE), and
//! waitpids the exit. Exercises the PM_PTRACE handler, the traced-signal
//! stop path (sig_proc → trace_stop), and traced waitpid reporting.
//! Expect `ptracetest: OK` and exit 0.

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

const SIGSTOP: i32 = 19;
const CHILD_EXIT: i32 = 7;

/// Append `bytes` to `s` at `*i`, advancing `*i`.
fn push(s: &mut [u8], i: &mut usize, bytes: &[u8]) {
    let n = bytes.len().min(s.len().saturating_sub(*i));
    s[*i..*i + n].copy_from_slice(&bytes[..n]);
    *i += n;
}

/// Append the decimal representation of `v` to `s` at `*i`.
fn push_num(s: &mut [u8], i: &mut usize, mut v: usize) {
    let mut tmp = [0u8; 20];
    let mut j = tmp.len();
    loop {
        j -= 1;
        tmp[j] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    push(s, i, &tmp[j..]);
}

fn fail(msg: &[u8], code: i32) -> ! {
    let mut buf = [0u8; 96];
    let mut i = 0;
    push(&mut buf, &mut i, b"ptracetest: ");
    push(&mut buf, &mut i, msg);
    push(&mut buf, &mut i, b"\n");
    unsafe { minix_rt::write(2, buf.as_ptr(), i) };
    minix_std::process::exit(code)
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let pid = match unsafe { minix_std::process::fork() } {
        Ok(p) => p,
        Err(_) => fail(b"fork failed", 1),
    };
    if pid == 0 {
        // Child (tracee): opt into tracing by the parent, then stop.
        if unsafe { minix_std::process::ptrace(minix_std::process::PT_TRACE_ME, 0, 0, 0) }.is_err()
        {
            fail(b"child ptrace(TRACEME) failed", 1);
        }
        // SIGSTOP diverts to the tracer and stops the child.
        let _ = minix_std::time::kill(minix_rt::getpid(), SIGSTOP);
        // Resumed by the tracer; exit with a distinctive status.
        minix_std::process::exit(CHILD_EXIT)
    }

    // Parent (tracer).
    // 1. The child stopped on SIGSTOP: waitpid reports W_STOPCODE(SIGSTOP).
    let (wpid, status) = match minix_std::process::waitpid(pid, 0) {
        Ok(w) => w,
        Err(_) => fail(b"waitpid (stop) failed", 2),
    };
    if wpid != pid || status != (SIGSTOP << 8) {
        let mut buf = [0u8; 96];
        let mut i = 0;
        push(&mut buf, &mut i, b"ptracetest: bad stop status ");
        push_num(&mut buf, &mut i, status as usize);
        push(&mut buf, &mut i, b"\n");
        unsafe { minix_rt::write(2, buf.as_ptr(), i) };
        return 2;
    }

    // 2. Resume the child.
    if unsafe { minix_std::process::ptrace(minix_std::process::PT_CONTINUE, pid, 0, 0) }.is_err() {
        fail(b"ptrace(CONTINUE) failed", 3);
    }

    // 3. The child exited 7: waitpid reports the exit status.
    let (wpid, status) = match minix_std::process::waitpid(pid, 0) {
        Ok(w) => w,
        Err(_) => fail(b"waitpid (exit) failed", 4),
    };
    if wpid != pid || status != CHILD_EXIT {
        let mut buf = [0u8; 96];
        let mut i = 0;
        push(&mut buf, &mut i, b"ptracetest: bad exit status ");
        push_num(&mut buf, &mut i, status as usize);
        push(&mut buf, &mut i, b"\n");
        unsafe { minix_rt::write(2, buf.as_ptr(), i) };
        return 4;
    }

    userland::write_out(b"ptracetest: OK\n");
    0
}
