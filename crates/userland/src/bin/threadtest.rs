//! Thread smoke test — `/bin/threadtest`.
//!
//! Spawns N kernel threads that share this process's address space. Each
//! worker blocks in a PM IPC round trip (`getpid`, a `sendrec`) — proving a
//! thread stuck in IPC does not stall its siblings (the whole point of 1:1
//! kernel threads) — bumps a shared atomic counter, yields the CPU a few
//! times, and exits. The main thread joins all workers and reports the
//! counter. Expect `counter=N (expected N)` and exit status 0.

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

use core::sync::atomic::{AtomicUsize, Ordering};

const NTHREADS: usize = 4;
const STACK_SIZE: usize = 16 * 1024;

/// Shared across all threads (same address space): how many workers finished.
static COUNTER: AtomicUsize = AtomicUsize::new(0);

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

/// Worker thread entry: called by the kernel with `arg` in the first
/// argument register and a fresh stack. Never returns.
fn worker(arg: usize) -> ! {
    // Block in a PM round trip. A thread in sendrec must not stall its
    // siblings; the scheduler switches to another runnable thread instead.
    let pid = minix_rt::getpid();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst) + 1;

    let mut buf = [0u8; 96];
    let mut i = 0;
    push(&mut buf, &mut i, b"  thread ");
    push_num(&mut buf, &mut i, arg);
    push(&mut buf, &mut i, b" pid=");
    push_num(&mut buf, &mut i, pid as usize);
    push(&mut buf, &mut i, b" n=");
    push_num(&mut buf, &mut i, n);
    push(&mut buf, &mut i, b"\n");
    unsafe { minix_rt::write(1, buf.as_ptr(), i) };

    // Hand the CPU around a few times.
    for _ in 0..3 {
        minix_rt::thread_yield();
    }
    minix_rt::thread_exit(0)
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let mut tids = [0i32; NTHREADS];
    for (i, slot) in tids.iter_mut().enumerate() {
        // Per-thread user stack from the heap (VM-backed via sbrk). The
        // kernel only records the stack top; the stack pages are mapped in
        // the shared address space, so every thread can use them.
        let base64 = unsafe { minix_rt::sbrk(STACK_SIZE as isize) };
        if base64 < 0 {
            return 1;
        }
        let stack_top = (((base64 as usize) + STACK_SIZE) & !0xF) - 8;
        // The kernel restores the thread directly to `entry` with no call,
        // so the initial RSP must match the x86_64 entry convention: ≡ 8
        // (mod 16), as if a return address had been pushed.
        let entry: fn(usize) -> ! = worker;
        let tid = minix_rt::thread_create(entry as usize, stack_top, i);
        if tid <= 0 {
            let mut buf = [0u8; 64];
            let mut j = 0;
            push(&mut buf, &mut j, b"threadtest: create failed (errno ");
            push_num(&mut buf, &mut j, tid.unsigned_abs() as usize);
            push(&mut buf, &mut j, b")\n");
            unsafe { minix_rt::write(2, buf.as_ptr(), j) };
            return 1;
        }
        *slot = tid;
    }
    let msg = b"threadtest: spawned\n";
    unsafe { minix_rt::write(1, msg.as_ptr(), msg.len()) };

    for &tid in &tids {
        let r = minix_rt::thread_join(tid);
        if r != 0 {
            let mut buf = [0u8; 64];
            let mut j = 0;
            push(&mut buf, &mut j, b"threadtest: join failed (errno ");
            push_num(&mut buf, &mut j, r.unsigned_abs() as usize);
            push(&mut buf, &mut j, b")\n");
            unsafe { minix_rt::write(2, buf.as_ptr(), j) };
            return 1;
        }
    }

    let total = COUNTER.load(Ordering::SeqCst);
    let mut buf = [0u8; 96];
    let mut j = 0;
    push(&mut buf, &mut j, b"threadtest: all joined, counter=");
    push_num(&mut buf, &mut j, total);
    push(&mut buf, &mut j, b" (expected ");
    push_num(&mut buf, &mut j, NTHREADS);
    push(&mut buf, &mut j, b")\n");
    unsafe { minix_rt::write(1, buf.as_ptr(), j) };
    if total == NTHREADS { 0 } else { 1 }
}
