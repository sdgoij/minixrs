//! Kernel thread support — 1:1 threads as `Proc` slots.
//!
//! A thread is a `Proc` table entry sharing its process's endpoint, page
//! table (`p_seg.p_cr3`), privilege structure, and file-descriptor state
//! with the process's main slot. The main slot heads a singly-linked list
//! of extra threads (`p_t_next`); every slot's `p_group` points back to
//! the main slot. Threads are scheduled and blocked in IPC exactly like
//! processes — the register frame, runnable flags, and IPC block state are
//! all per-slot — so `enqueue`/`pick_proc`/`restore`/`mini_send` work on a
//! thread slot unchanged.
//!
//! Lifecycle is kernel-side (no PM/VM involvement): `create` allocates a
//! free slot from the process table and enqueues it, `exit` frees the
//! caller's slot (after waking a joiner), `join` blocks the caller until
//! the target thread exits, and `yield_self` hands the CPU to another
//! runnable thread. Main-thread `exit` is a process exit (handled by the
//! syscall layer, not here).

use core::sync::atomic::Ordering;

use crate::hal;
use crate::proc::{Proc, RtsFlags};
use crate::sched::{dequeue, enqueue};
use crate::table::{is_empty_proc, proc_addr};

/// Maximum threads per process. Bounds the tid scan and keeps one process
/// from exhausting the whole `Proc` table.
pub const MAX_THREADS_PER_PROC: u32 = 64;

/// Default per-thread time quantum in ms (threads are kernel-scheduled and
/// the kernel renews the quantum on expiry, like other user processes).
pub const DEFAULT_QUANTUM_MS: u32 = 50;

/// The process's main slot for `rp` (self when `rp` is not part of a group).
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn group(rp: *mut Proc) -> *mut Proc {
    unsafe { (*rp).group() }
}

/// Is `rp` a non-main thread of its process?
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn is_thread(rp: *mut Proc) -> bool {
    unsafe { (*rp).is_thread() }
}

/// Create a new thread in `caller`'s process.
///
/// `entry` is the user-space start function, called with `arg` in the first
/// argument register; `stack` is the top of the new thread's user stack
/// (allocated by the caller — the kernel only records it in the frame). The
/// kernel restores the thread directly to `entry` with no call instruction,
/// so `stack` must match the ABI entry convention of the target: ≡ 8
/// (mod 16) on x86_64 (as if a return address had been pushed), 16-aligned
/// on RISC-V/AArch64 (return address lives in a register).
///
/// Returns the new thread's tid (> 0) or a negative errno.
///
/// # Safety
///
/// `caller` must point to a live `Proc`; `entry`/`stack` must be valid
/// user-space addresses in the caller's address space.
pub unsafe fn create(caller: *mut Proc, entry: u64, stack: u64, arg: u64) -> i32 {
    unsafe {
        let main = group(caller);

        // Next tid: one past the highest in the group. Tids are never
        // reused while any thread of the process is alive, so a stale
        // `thread_join` cannot rendezvous with the wrong thread.
        let mut tid: u32 = 1;
        let mut t = main;
        loop {
            if (*t).p_tid >= tid {
                tid = (*t).p_tid + 1;
            }
            t = (*t).p_t_next;
            if t.is_null() {
                break;
            }
        }
        if tid > MAX_THREADS_PER_PROC {
            return crate::ipc::EAGAIN;
        }

        // Find a free slot. Thread slots are ordinary SLOT_FREE entries
        // once freed, so they recycle like process slots; while in use they
        // are non-empty, so fork/VM will not hand them out. Boot procs
        // occupy proc_nr -5..14, so the first free user slot is NR_BOOT_PROCS.
        for slot in crate::table::NR_BOOT_PROCS as i32..crate::proc::NR_PROCS_TOTAL as i32 {
            let rp = proc_addr(slot);
            if rp.is_null() || !is_empty_proc(rp) {
                continue;
            }
            // Fresh slate (rts = 0, runnable; magic set).
            *rp = Proc::default();
            (*rp).p_nr = slot;
            (*rp).p_endpoint = (*main).p_endpoint;
            (*rp).p_priv = (*main).p_priv;
            // Same address space; fresh FPU state (per-thread FPU is
            // per-slot, though the kernel does not save/restore FPU yet).
            (*rp).p_seg.p_cr3 = (*main).p_seg.p_cr3;
            (*rp).p_seg.p_cr3_v = (*main).p_seg.p_cr3_v;
            (*rp).p_seg.p_kern_trap_style = (*main).p_seg.p_kern_trap_style;
            (*rp).p_fd_vfs = (*main).p_fd_vfs;
            (*rp).p_priority = (*main).p_priority;
            (*rp).p_scheduler = (*main).p_scheduler;
            (*rp).p_tid = tid;
            (*rp).p_group = main;

            // Link into the thread list (append at the tail).
            let mut tail = main;
            while !(*tail).p_t_next.is_null() {
                tail = (*tail).p_t_next;
            }
            (*tail).p_t_next = rp;

            // Register frame: start at `entry` with `stack` and `arg`.
            hal::set_initial_regs(&mut (*rp).p_reg, entry, stack, arg);
            // aarch64's set_initial_regs ignores the arg; write x0 directly.
            #[cfg(target_arch = "aarch64")]
            {
                (&mut (*rp).p_reg)[0..8].copy_from_slice(&arg.to_ne_bytes());
            }

            // Grant a full quantum so the thread is immediately runnable.
            (*rp).p_quantum_size_ms = DEFAULT_QUANTUM_MS;
            (*rp).p_cpu_time_left = crate::clock::ms_2_cpu_time(DEFAULT_QUANTUM_MS as usize);

            enqueue(rp);
            return tid as i32;
        }
        crate::ipc::ENOMEM
    }
}

/// Terminate the calling thread: wake a joiner if any, unlink the slot from
/// the process's thread list, clear IPC block state (a thread blocked in
/// SEND is linked into the destination's caller queue — a freed slot would
/// leave a dangling pointer), and free the slot.
///
/// Returns `EDONTREPLY` so the syscall-return path switches to another
/// thread without touching the (now freed) caller slot.
///
/// # Safety
///
/// `rp` must be the currently running thread slot.
pub unsafe fn exit(rp: *mut Proc) -> i32 {
    unsafe {
        let main = group(rp);

        // Wake a joiner before freeing the slot (the waiter is stored on
        // the target slot, which is about to be recycled).
        let joiner = (*rp).p_join_waiter;
        if !joiner.is_null() {
            (*rp).p_join_waiter = core::ptr::null_mut();
            let old = (*joiner).p_rts_flags.load(Ordering::Relaxed);
            let new = old & !RtsFlags::JOINING.bits();
            (*joiner).p_rts_flags.store(new, Ordering::Relaxed);
            hal::write_retval(&mut (*joiner).p_reg, 0);
            if new == 0 {
                enqueue(joiner);
            }
        }

        // Unlink from the process's thread list.
        let mut prev = main;
        let mut t = (*main).p_t_next;
        while !t.is_null() {
            if t == rp {
                (*prev).p_t_next = (*t).p_t_next;
                break;
            }
            prev = t;
            t = (*t).p_t_next;
        }

        // Unlink from any destination's caller queue and clear the rest of
        // the IPC block state.
        crate::system::clear_ipc(rp);

        hal::release_fpu(rp as *mut core::ffi::c_void);

        // Free the slot and remove it from the run queue. The running
        // thread stays linked in its run queue under this port's invariant,
        // so mark SLOT_FREE first (dequeue asserts non-runnable), then
        // unlink — a no-op when the thread was never enqueued.
        (*rp)
            .p_rts_flags
            .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        crate::sched::dequeue(rp);
        (*rp).p_t_next = core::ptr::null_mut();
        (*rp).p_group = core::ptr::null_mut();
        (*rp).p_join_waiter = core::ptr::null_mut();
        crate::system::EDONTREPLY
    }
}

/// Wait for the thread `tid` of `rp`'s process to exit. Blocks the caller
/// (`RtsFlags::JOINING`) until the target's `exit` wakes it; returns OK
/// immediately when the target has already exited. EINVAL for a join of
/// the main thread (tid 0), a self-join, or a second concurrent join of
/// the same thread.
///
/// # Safety
///
/// `rp` must point to a live `Proc`.
pub unsafe fn join(rp: *mut Proc, tid: u32) -> i32 {
    unsafe {
        let main = group(rp);
        if tid == 0 {
            // Joining the process itself would deadlock.
            return crate::ipc::EINVAL;
        }
        let mut t = (*main).p_t_next;
        while !t.is_null() {
            if (*t).p_tid == tid {
                if t == rp {
                    return crate::ipc::EINVAL; // self-join
                }
                if !(*t).p_join_waiter.is_null() {
                    return crate::ipc::EINVAL; // already joined by another thread
                }
                (*t).p_join_waiter = rp;
                let old = (*rp).p_rts_flags.load(Ordering::Relaxed);
                (*rp)
                    .p_rts_flags
                    .store(old | RtsFlags::JOINING.bits(), Ordering::Relaxed);
                if old == 0 {
                    dequeue(rp);
                }
                return crate::ipc::OK;
            }
            t = (*t).p_t_next;
        }
        // Target not found: it already exited, so the join succeeds at once.
        crate::ipc::OK
    }
}

/// Yield the CPU to another runnable thread: mark the caller `PREEMPTED` so
/// the syscall-return path re-enqueues it at the tail of its run queue and
/// picks another thread.
///
/// # Safety
///
/// `rp` must be the currently running process/thread.
pub unsafe fn yield_self(rp: *mut Proc) {
    unsafe {
        (*rp)
            .p_rts_flags
            .fetch_or(RtsFlags::PREEMPTED.bits(), Ordering::Relaxed);
    }
}

/// Block `rp` on the user-space futex at `addr` until `futex_wake` on the
/// same address. Returns OK when blocked, or -EAGAIN when `*addr` no longer
/// equals `expected` (the caller must retry its compare-and-set).
///
/// The check-and-block is atomic on this single-CPU kernel: syscalls run
/// with interrupts disabled, so no `futex_wake` can run between the value
/// load and the FUTEX_WAIT flag being set — a wakeup cannot be lost.
///
/// # Safety
///
/// `rp` must be the currently running thread; `addr` must be a readable
/// user-space address in its address space.
pub unsafe fn futex_wait(rp: *mut Proc, addr: u64, expected: u64) -> i32 {
    unsafe {
        let mut val = [0u8; 4];
        let r = crate::ipc::copy_from_user(rp, addr, val.as_mut_ptr(), 4);
        if r != 0 {
            return crate::ipc::EFAULT;
        }
        if u32::from_ne_bytes(val) != expected as u32 {
            return crate::ipc::EAGAIN;
        }
        (*rp).p_futex_addr = addr;
        let old = (*rp).p_rts_flags.load(Ordering::Relaxed);
        (*rp)
            .p_rts_flags
            .store(old | RtsFlags::FUTEX_WAIT.bits(), Ordering::Relaxed);
        if old == 0 {
            dequeue(rp);
        }
        crate::ipc::OK
    }
}

/// Wake up to `count` threads blocked in `futex_wait` on `addr`. Returns
/// the number woken. The waiters are found by scanning the process table
/// for threads with FUTEX_WAIT set and a matching `p_futex_addr` (fine for
/// our table size; single-CPU so no locking needed).
///
/// # Safety
///
/// `addr` must be a user-space address (unmapped addresses simply wake
/// nobody).
pub unsafe fn futex_wake(addr: u64, count: u32) -> i32 {
    unsafe {
        let base = crate::table::proc_table_base();
        let mut woken: i32 = 0;
        for i in 0..crate::proc::NR_PROCS_TOTAL {
            if woken >= count as i32 {
                break;
            }
            let p = base.add(i);
            if (*p).is_empty() {
                continue;
            }
            let rts = (*p).p_rts_flags.load(Ordering::Relaxed);
            if rts & RtsFlags::FUTEX_WAIT.bits() != 0 && (*p).p_futex_addr == addr {
                let new = rts & !RtsFlags::FUTEX_WAIT.bits();
                (*p).p_rts_flags.store(new, Ordering::Relaxed);
                crate::hal::write_retval(&mut (*p).p_reg, 0);
                if new == 0 {
                    enqueue(p);
                }
                woken += 1;
            }
        }
        woken
    }
}

/// Is `t` blocked in the RECEIVE phase of a sendrec to PM — the state a
/// thread is in while its `PM_FORK` request is being processed?
///
/// # Safety
///
/// `t` must point to a valid `Proc`.
unsafe fn in_fork_sendrec(t: *mut Proc) -> bool {
    unsafe {
        let rts = (*t).p_rts_flags.load(Ordering::Relaxed);
        let mf = (*t).p_misc_flags.load(Ordering::Relaxed);
        rts & RtsFlags::RECEIVING.bits() != 0
            && mf & crate::proc::MiscFlags::REPLY_PEND.bits() != 0
            && (*t).p_getfrom_e == arch_common::com::PM_PROC_NR
    }
}

/// Find the thread that is about to be forked: the thread blocked in the
/// RECEIVE phase of its `PM_FORK` sendrec. POSIX fork makes the child a
/// copy of the calling thread, so `do_fork` must copy this thread's frame,
/// not the main thread's. Prefers the main thread when it is itself in
/// that state (the common case), then scans the worker threads.
///
/// Best-effort: when two threads are simultaneously in a sendrec to PM the
/// choice is ambiguous; the main thread is preferred. Returns null when no
/// candidate is found (single-threaded processes have no worker threads).
///
/// # Safety
///
/// `main` must be the process's main slot with a consistent thread list.
pub unsafe fn find_forking_thread(main: *mut Proc) -> *mut Proc {
    unsafe {
        if in_fork_sendrec(main) {
            return main;
        }
        let mut t = (*main).p_t_next;
        while !t.is_null() {
            if in_fork_sendrec(t) {
                return t;
            }
            t = (*t).p_t_next;
        }
        core::ptr::null_mut()
    }
}

/// Free every extra thread of `main`'s process — used when the process dies
/// (main-thread exit, `exit()` from any thread, a fatal signal, or PM's
/// `SYS_CLEAR` reap) or when `exec` replaces the process image. Walks the
/// thread list, unlinks each thread from any destination's caller queue
/// (a thread blocked in SEND is linked there; a freed slot would leave a
/// dangling pointer) and from the run queue, and marks its slot `SLOT_FREE`.
/// The main slot itself is untouched — the caller owns its bookkeeping.
///
/// # Safety
///
/// `main` must be the process's main slot with a consistent thread list.
pub unsafe fn sweep_group(main: *mut Proc) {
    unsafe {
        let mut t = (*main).p_t_next;
        (*main).p_t_next = core::ptr::null_mut();
        while !t.is_null() {
            let next = (*t).p_t_next;
            crate::system::clear_ipc(t);
            hal::release_fpu(t as *mut core::ffi::c_void);
            (*t).p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            crate::sched::dequeue(t);
            (*t).p_t_next = core::ptr::null_mut();
            (*t).p_group = core::ptr::null_mut();
            (*t).p_join_waiter = core::ptr::null_mut();
            t = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::{MiscFlags, PMAGIC};
    use crate::table::proc_init;
    use core::sync::atomic::AtomicBool;

    /// Serialization lock — thread tests share the static proc table and
    /// cpu-local run queues and cannot run concurrently.
    static THREAD_TEST_LOCK: AtomicBool = AtomicBool::new(false);

    struct ThreadTestLock;
    impl ThreadTestLock {
        fn acquire() -> Self {
            while THREAD_TEST_LOCK
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                core::hint::spin_loop();
            }
            Self
        }
    }
    impl Drop for ThreadTestLock {
        fn drop(&mut self) {
            THREAD_TEST_LOCK.store(false, Ordering::SeqCst);
        }
    }

    /// Reset the proc table + run queues for test isolation.
    unsafe fn reset_state() {
        unsafe {
            proc_init();
            crate::hal::init_cpulocals();
            let head = crate::hal::sched_run_q_head() as *mut [*mut Proc; 16];
            let tail = crate::hal::sched_run_q_tail() as *mut [*mut Proc; 16];
            for q in 0..16 {
                (*head)[q] = core::ptr::null_mut();
                (*tail)[q] = core::ptr::null_mut();
            }
        }
    }

    /// Claim a user-range slot as a runnable main process.
    unsafe fn make_main(nr: i32) -> *mut Proc {
        unsafe {
            let rp = crate::table::proc_addr(nr);
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);
            (*rp).p_nr = nr;
            (*rp).p_endpoint = crate::table::make_endpoint(0, nr);
            (*rp).p_priority = 5;
            (*rp).p_cpu_time_left = 1_000_000;
            (*rp).p_magic = PMAGIC;
            (*rp).p_tid = 0;
            (*rp).p_t_next = core::ptr::null_mut();
            (*rp).p_group = core::ptr::null_mut();
            (*rp).p_join_waiter = core::ptr::null_mut();
            rp
        }
    }

    #[test]
    fn test_create_links_and_enqueues_thread() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(100);
            let tid = create(main, 0x401000, 0x7fff_f000, 0x1234);
            assert_eq!(tid, 1);
            // Thread slot linked off the main slot, same endpoint/priority.
            let t = (*main).p_t_next;
            assert!(!t.is_null());
            assert_eq!((*t).p_tid, 1);
            assert_eq!((*t).p_group, main);
            assert_eq!((*t).p_endpoint, (*main).p_endpoint);
            // Frame: entry/stack/arg.
            let frame = &(*t).p_reg;
            let entry = u64::from_ne_bytes(frame[16..24].try_into().unwrap());
            let rsp = u64::from_ne_bytes(frame[168..176].try_into().unwrap());
            let rdi = u64::from_ne_bytes(frame[40..48].try_into().unwrap());
            assert_eq!(entry, 0x401000);
            assert_eq!(rsp, 0x7fff_f000);
            assert_eq!(rdi, 0x1234);
            // Runnable and on the run queue.
            assert!((*t).is_runnable());
            assert_eq!(crate::sched::pick_proc(), Some(t));
            // Cleanup: unlink + free.
            (*main).p_t_next = core::ptr::null_mut();
            (*t).p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_create_second_thread_tid_increments() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(101);
            assert_eq!(create(main, 0x401000, 0x7fff_f000, 0), 1);
            let t1 = (*main).p_t_next;
            assert!(!t1.is_null());
            assert_eq!(create(main, 0x402000, 0x7fff_e000, 0), 2);
            let t2 = (*t1).p_t_next;
            assert!(!t2.is_null());
            assert_eq!((*t1).p_tid, 1);
            assert_eq!((*t2).p_tid, 2);
            // Cleanup.
            let mut t = (*main).p_t_next;
            while !t.is_null() {
                let next = (*t).p_t_next;
                (*t).p_rts_flags
                    .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
                t = next;
            }
            (*main).p_t_next = core::ptr::null_mut();
        }
    }

    #[test]
    fn test_join_blocks_then_exit_wakes() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(102);
            let tid = create(main, 0x401000, 0x7fff_f000, 0);
            let target = (*main).p_t_next;
            assert_eq!(tid, 1);

            // Caller (a second thread) joins tid 1: registers as waiter
            // and blocks with JOINING.
            let joiner = proc_addr(120);
            (*joiner).p_rts_flags.store(0, Ordering::Relaxed);
            (*joiner).p_nr = 3;
            (*joiner).p_endpoint = (*main).p_endpoint;
            (*joiner).p_magic = PMAGIC;
            (*joiner).p_tid = 2;
            (*joiner).p_group = main;
            let r = join(joiner, tid as u32);
            assert_eq!(r, crate::ipc::OK);
            assert_eq!((*target).p_join_waiter, joiner);
            assert!((*joiner).rts_isset(RtsFlags::JOINING));
            assert!(!(*joiner).is_runnable());

            // Target exits: joiner woken with retval 0, slot freed.
            let r2 = exit(target);
            assert_eq!(r2, crate::system::EDONTREPLY);
            assert!((*joiner).is_runnable());
            assert!((*target).is_empty());
            assert_eq!((*target).p_join_waiter, core::ptr::null_mut());
            // Thread list no longer contains the target.
            assert!((*main).p_t_next.is_null());
            // Cleanup.
            (*joiner)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_join_already_exited_returns_ok() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(103);
            // No threads: joining any tid succeeds immediately.
            assert_eq!(join(main, 7), crate::ipc::OK);
            // Joining the main thread itself is rejected.
            assert_eq!(join(main, 0), crate::ipc::EINVAL);
        }
    }

    #[test]
    fn test_self_join_rejected() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(104);
            let tid = create(main, 0x401000, 0x7fff_f000, 0);
            let target = (*main).p_t_next;
            // The target joining itself is rejected.
            assert_eq!(join(target, tid as u32), crate::ipc::EINVAL);
            // Cleanup.
            (*main).p_t_next = core::ptr::null_mut();
            (*target)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_group_and_is_thread() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(105);
            // Main slot: no group, not a thread.
            assert_eq!(group(main), main);
            assert!(!is_thread(main));
            let tid = create(main, 0x401000, 0x7fff_f000, 0);
            let t = (*main).p_t_next;
            assert_eq!(tid, 1);
            assert_eq!(group(t), main);
            assert!(is_thread(t));
            // Cleanup.
            (*main).p_t_next = core::ptr::null_mut();
            (*t).p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_futex_wait_blocks_and_wake() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(114);
            // Futex word in kernel memory; host copy_from_user is a no-op,
            // so the read-back value is 0.
            let word: u32 = 0;
            let addr = core::ptr::addr_of!(word) as u64;

            // Matching value: blocks with FUTEX_WAIT, dequeued.
            assert_eq!(futex_wait(main, addr, 0), crate::ipc::OK);
            assert!((*main).rts_isset(RtsFlags::FUTEX_WAIT));
            assert!(!(*main).is_runnable());

            // Mismatched value: EAGAIN, flags untouched.
            assert_eq!(futex_wait(main, addr, 1), crate::ipc::EAGAIN);
            assert!((*main).rts_isset(RtsFlags::FUTEX_WAIT));

            // Wake: runnable again, flag cleared.
            assert_eq!(futex_wake(addr, 1), 1);
            assert!((*main).is_runnable());
            assert!(!(*main).rts_isset(RtsFlags::FUTEX_WAIT));

            // No waiters: nothing woken.
            assert_eq!(futex_wake(addr, 1), 0);

            (*main)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_futex_wake_respects_count() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(115);
            assert_eq!(create(main, 0x401000, 0x7fff_f000, 0), 1);
            let t1 = (*main).p_t_next;
            assert_eq!(create(main, 0x402000, 0x7fff_e000, 0), 2);
            let t2 = (*t1).p_t_next;
            let addr = 0x1234_0000u64; // never dereferenced in wake

            // Both waiters blocked.
            for t in [main, t1, t2] {
                (*t).p_futex_addr = addr;
                (*t).p_rts_flags
                    .store(RtsFlags::FUTEX_WAIT.bits(), Ordering::Relaxed);
            }

            // Wake one: exactly one becomes runnable.
            assert_eq!(futex_wake(addr, 1), 1);
            let mut woken = 0;
            for t in [main, t1, t2] {
                if (*t).is_runnable() {
                    woken += 1;
                }
            }
            assert_eq!(woken, 1);

            // Wake all: the remaining two.
            assert_eq!(futex_wake(addr, i32::MAX as u32), 2);

            // Cleanup.
            (*main).p_t_next = core::ptr::null_mut();
            (*t1)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            (*t2)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_yield_marks_preempted() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(106);
            assert!(!(*main).is_preempted());
            yield_self(main);
            assert!((*main).is_preempted());
            (*main)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_exit_unlinks_blocked_sender() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(107);
            let tid = create(main, 0x401000, 0x7fff_f000, 0);
            let t = (*main).p_t_next;
            assert_eq!(tid, 1);
            // Simulate the thread blocked in SEND to a destination: linked
            // on the destination's caller queue.
            let dst = make_main(108);
            (*t).p_rts_flags
                .store(RtsFlags::SENDING.bits(), Ordering::Relaxed);
            (*t).p_sendto_e = (*dst).p_endpoint;
            (*t).p_q_link = core::ptr::null_mut();
            (*dst).p_caller_q = t;
            // Exit must unlink it from the destination's caller queue.
            exit(t);
            assert!((*dst).p_caller_q.is_null());
            assert!((*main).p_t_next.is_null());
            assert!((*t).is_empty());
            // Cleanup.
            (*dst)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_sweep_group_frees_all_threads() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(110);
            assert_eq!(create(main, 0x401000, 0x7fff_f000, 0), 1);
            let t1 = (*main).p_t_next;
            assert_eq!(create(main, 0x402000, 0x7fff_e000, 0), 2);
            let t2 = (*t1).p_t_next;

            // t2 blocked as a sender on a destination's caller queue.
            let dst = make_main(111);
            (*t2)
                .p_rts_flags
                .store(RtsFlags::SENDING.bits(), Ordering::Relaxed);
            (*t2).p_sendto_e = (*dst).p_endpoint;
            (*t2).p_q_link = core::ptr::null_mut();
            (*dst).p_caller_q = t2;
            // t1 blocked in RECEIVE (not linked anywhere).
            (*t1)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*t1).p_getfrom_e = crate::system::NONE;

            sweep_group(main);
            // All worker slots freed, thread list empty, caller queue clean.
            assert!((*t1).is_empty());
            assert!((*t2).is_empty());
            assert!((*main).p_t_next.is_null());
            assert!((*dst).p_caller_q.is_null());
            // Main slot untouched (still runnable).
            assert!((*main).is_runnable());
            // Cleanup.
            (*main)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            (*dst)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }

    #[test]
    fn test_find_forking_thread_prefers_main_then_scans() {
        let _l = ThreadTestLock::acquire();
        unsafe {
            reset_state();
            let main = make_main(112);
            assert_eq!(create(main, 0x401000, 0x7fff_f000, 0), 1);
            let t1 = (*main).p_t_next;

            // No thread in the fork sendrec: null (falls back to main).
            assert!(find_forking_thread(main).is_null());

            // Worker in the fork sendrec (RECEIVE phase of a sendrec to PM):
            // found.
            (*t1)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*t1)
                .p_misc_flags
                .store(MiscFlags::REPLY_PEND.bits(), Ordering::Relaxed);
            (*t1).p_getfrom_e = arch_common::com::PM_PROC_NR;
            assert_eq!(find_forking_thread(main), t1);

            // Main thread in the fork sendrec: main is preferred.
            (*main)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*main)
                .p_misc_flags
                .store(MiscFlags::REPLY_PEND.bits(), Ordering::Relaxed);
            (*main).p_getfrom_e = arch_common::com::PM_PROC_NR;
            assert_eq!(find_forking_thread(main), main);

            // Cleanup.
            (*main).p_t_next = core::ptr::null_mut();
            (*t1)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            (*main)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        }
    }
}
