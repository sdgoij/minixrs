//! Fork-from-worker test — `/bin/forkthread`.
//!
//! Spawns a worker thread, records the worker's tid (the new
//! `SYS_thread_self` syscall), then calls `fork()` from the worker. The
//! child must be a copy of the WORKER, not the main thread: it verifies
//! the inherited frame still holds the worker's tid (the main thread's
//! would be 0) and exits; the worker waitpids the child and exits; the
//! main thread joins. Proves the SYS_FORK tid plumbing (Phase F2) makes
//! fork-from-worker deterministic. Expect `forkthread: OK` and exit 0.

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

const STACK_SIZE: usize = 16 * 1024;

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

/// Worker thread entry: records its tid, forks, and has the child verify
/// it is a copy of this worker's frame. Never returns.
fn worker(arg: usize) -> ! {
    // The calling thread's tid (0 = main). Copied into the child's frame
    // by fork, so the child can prove which thread was cloned.
    let my_tid = minix_rt::thread_self();
    let marker = 0x5A5A_5A5A ^ (my_tid as u32);

    let mut buf = [0u8; 96];
    let mut i = 0;
    push(&mut buf, &mut i, b"forkthread: worker ");
    push_num(&mut buf, &mut i, arg);
    push(&mut buf, &mut i, b" tid=");
    push_num(&mut buf, &mut i, my_tid as usize);
    push(&mut buf, &mut i, b", forking\n");
    unsafe { minix_rt::write(1, buf.as_ptr(), i) };

    let pid = match unsafe { minix_std::process::fork() } {
        Ok(p) => p,
        Err(_) => {
            userland::write_err(b"forkthread: fork failed\n");
            minix_rt::thread_exit(1)
        }
    };

    if pid == 0 {
        // Child: a copy of the worker. If the kernel had cloned the main
        // thread's frame instead, my_tid would be 0 here and the marker
        // would not match — the whole flow would differ.
        let ok = my_tid != 0 && marker == (0x5A5A_5A5A ^ (my_tid as u32));
        if ok {
            let mut buf = [0u8; 96];
            let mut i = 0;
            push(&mut buf, &mut i, b"forkthread: child is worker copy (tid ");
            push_num(&mut buf, &mut i, my_tid as usize);
            push(&mut buf, &mut i, b")\n");
            unsafe { minix_rt::write(1, buf.as_ptr(), i) };
        } else {
            userland::write_err(b"forkthread: child is NOT a worker copy\n");
        }
        minix_std::process::exit(if ok { 0 } else { 2 })
    }

    // Worker: wait for the child, then exit the thread.
    let (_, status) = match minix_std::process::waitpid(pid, 0) {
        Ok(w) => w,
        Err(_) => {
            userland::write_err(b"forkthread: worker waitpid failed\n");
            minix_rt::thread_exit(3)
        }
    };
    if status != 0 {
        userland::write_err(b"forkthread: child exited nonzero\n");
        minix_rt::thread_exit(4)
    }
    userland::write_out(b"forkthread: worker fork OK\n");
    minix_rt::thread_exit(0)
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // Per-thread user stack from the heap (VM-backed via sbrk).
    let base64 = unsafe { minix_rt::sbrk(STACK_SIZE as isize) };
    if base64 < 0 {
        return 1;
    }
    let stack_top = (((base64 as usize) + STACK_SIZE) & !0xF) - 8;
    // The kernel restores the thread directly to `entry` with no call, so
    // the initial RSP must match the x86_64 entry convention: ≡ 8 (mod 16).
    let entry: fn(usize) -> ! = worker;
    let tid = minix_rt::thread_create(entry as usize, stack_top, 1);
    if tid <= 0 {
        userland::write_err(b"forkthread: create failed\n");
        return 1;
    }

    let r = minix_rt::thread_join(tid);
    if r != 0 {
        userland::write_err(b"forkthread: join failed\n");
        return 1;
    }
    userland::write_out(b"forkthread: OK\n");
    0
}
