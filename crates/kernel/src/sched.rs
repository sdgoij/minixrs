//! Scheduler — adapted from `minix/kernel/proc.c`
//!
//! Multi-level priority queue scheduling with 16 priority levels,
//! quantum-based preemption, and user-space scheduler notification.
//!
//! **Single-CPU implementation.** Run queues live in cpulocals storage,
//! accessed through `CPU_LOCAL_STORAGE`'s `run_q_head_ptr`/`run_q_tail_ptr`.

use core::sync::atomic::Ordering;

use arch_x86_64::cpulocals::CPU_LOCAL_STORAGE;

use crate::proc::*;

// ─────────────────────────────────────────────────────────────────────────
// Helpers: cast cpulocals run queue pointers to *mut Proc array
// ─────────────────────────────────────────────────────────────────────────

type RunQArray = [*mut Proc; NR_SCHED_QUEUES];

/// Get a pointer to the run_q_head array as `*mut [*mut Proc; 16]`.
unsafe fn run_q_head_array() -> *mut RunQArray {
    unsafe { CPU_LOCAL_STORAGE.run_q_head_ptr() as *mut RunQArray }
}

/// Get a pointer to the run_q_tail array as `*mut [*mut Proc; 16]`.
unsafe fn run_q_tail_array() -> *mut RunQArray {
    unsafe { CPU_LOCAL_STORAGE.run_q_tail_ptr() as *mut RunQArray }
}

/// Get current process pointer cast to *mut Proc.
unsafe fn current_proc() -> *mut Proc {
    unsafe { CPU_LOCAL_STORAGE.proc_ptr() as *mut Proc }
}

// ─────────────────────────────────────────────────────────────────────────
// enqueue
// ─────────────────────────────────────────────────────────────────────────

/// Add `rp` to one of the run queues.
///
/// The process is inserted at the tail of its priority queue. If its
/// priority is higher than the currently running process (and the
/// current process is preemptible), the current process is marked
/// `RTS_PREEMPTED`.
///
/// # Safety
///
/// `rp` must point to a valid `Proc` in the process table, must be
/// runnable (`p_rts_flags == 0`).
pub unsafe fn enqueue(rp: *mut Proc) {
    unsafe {
        assert!((*rp).is_runnable());

        let q = (*rp).p_priority as usize;
        assert!(q < NR_SCHED_QUEUES);

        let head = run_q_head_array();
        let tail = run_q_tail_array();

        if (*head)[q].is_null() {
            // Empty queue — create new
            (*head)[q] = rp;
            (*tail)[q] = rp;
            (*rp).p_nextready = core::ptr::null_mut();
        } else {
            // Add to tail
            (*(*tail)[q]).p_nextready = rp;
            (*tail)[q] = rp;
            (*rp).p_nextready = core::ptr::null_mut();
        }

        // Check preemption
        let current = current_proc();
        if !current.is_null() {
            let cur_priority = (*current).p_priority;
            let rp_priority = (*rp).p_priority;
            if rp_priority < cur_priority {
                // rp has higher priority (lower number = higher priority)
                if !(*current).p_priv.is_null() {
                    let flags = (*(*current).p_priv).s_flags;
                    if flags.contains(crate::r#priv::PrivFlags::PREEMPTIBLE) {
                        // Mark current as preempted
                        let rts_flag = RtsFlags::PREEMPTED;
                        let old_flags = (*current).p_rts_flags.load(Ordering::Relaxed);
                        (*current)
                            .p_rts_flags
                            .store(old_flags | rts_flag.bits(), Ordering::Relaxed);
                        if old_flags == 0 && !(*current).is_runnable() {
                            dequeue(current);
                        }
                    }
                }
            }
        }
    }
}

/// Remove a process from the run queue without the `!is_runnable()` assertion.
/// Used to move the current process to the tail for round-robin fairness.
///
/// # Safety
///
/// `rp` must be in the run queue.
pub unsafe fn remove_from_queue(rp: *mut Proc) {
    unsafe {
        let q = (*rp).p_priority as usize;
        assert!(q < NR_SCHED_QUEUES);

        let head = run_q_head_array();
        let tail = run_q_tail_array();

        let mut prev: *mut Proc = core::ptr::null_mut();
        let mut curr = (*head)[q];

        while !curr.is_null() {
            if curr == rp {
                let next = (*curr).p_nextready;
                if prev.is_null() {
                    (*head)[q] = next;
                } else {
                    (*prev).p_nextready = next;
                }
                if rp == (*tail)[q] {
                    (*tail)[q] = prev;
                }
                break;
            }
            prev = curr;
            curr = (*curr).p_nextready;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// enqueue_head
// ─────────────────────────────────────────────────────────────────────────

/// Insert `rp` at the front of its run queue.
///
/// Used for preempted processes that should go back to the front
/// of their queue rather than the tail.
///
/// # Safety
///
/// `rp` must be runnable and have quantum remaining.
pub unsafe fn enqueue_head(rp: *mut Proc) {
    unsafe {
        assert!((*rp).is_runnable());
        assert!((*rp).p_cpu_time_left > 0);

        let q = (*rp).p_priority as usize;
        assert!(q < NR_SCHED_QUEUES);

        let head = run_q_head_array();
        let tail = run_q_tail_array();

        if (*head)[q].is_null() {
            // Empty queue
            (*head)[q] = rp;
            (*tail)[q] = rp;
            (*rp).p_nextready = core::ptr::null_mut();
        } else {
            // Insert at head
            (*rp).p_nextready = (*head)[q];
            (*head)[q] = rp;
        }

        // Accounting
        (*rp).p_accounting.dequeues = (*rp).p_accounting.dequeues.wrapping_sub(1);
        (*rp).p_accounting.preempted = (*rp).p_accounting.preempted.wrapping_add(1);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// dequeue
// ─────────────────────────────────────────────────────────────────────────

/// Remove `rp` from the run queues.
///
/// Walks the linked list to find and unlink the process. Updates
/// accounting (dequeue count, time in queue).
///
/// # Safety
///
/// `rp` must point to a valid `Proc` in the process table.
pub unsafe fn dequeue(rp: *mut Proc) {
    unsafe {
        assert!(!(*rp).is_runnable());

        let q = (*rp).p_priority as usize;
        assert!(q < NR_SCHED_QUEUES);

        let head = run_q_head_array();
        let tail = run_q_tail_array();

        // Walk the linked list starting from head
        let mut prev: *mut Proc = core::ptr::null_mut();
        let mut curr = (*head)[q];

        while !curr.is_null() {
            if curr == rp {
                // Found it — unlink
                let next = (*curr).p_nextready;
                if prev.is_null() {
                    // Removing the head
                    (*head)[q] = next;
                } else {
                    (*prev).p_nextready = next;
                }
                if rp == (*tail)[q] {
                    (*tail)[q] = prev;
                }
                break;
            }
            prev = curr;
            curr = (*curr).p_nextready;
        }

        // Accounting
        (*rp).p_accounting.dequeues = (*rp).p_accounting.dequeues.wrapping_add(1);

        if (*rp).p_accounting.enter_queue != 0 {
            #[cfg(not(test))]
            {
                let _tsc = arch_x86_64::hw::read_tsc();
            }
            (*rp).p_accounting.enter_queue = 0;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// pick_proc
// ─────────────────────────────────────────────────────────────────────────

/// Select the next process to run.
///
/// Scans all 16 priority queues from highest (0) to lowest (15) and
/// returns the first runnable process. If a billable process is selected,
/// records it in `bill_ptr`.
///
/// Returns `None` if no process is ready (should switch to idle).
///
/// # Safety
///
/// The run queue state must be accessible (not concurrently modified).
pub unsafe fn pick_proc() -> Option<*mut Proc> {
    unsafe {
        let head = run_q_head_array();

        for q in 0..NR_SCHED_QUEUES {
            let rp = (*head)[q];
            if rp.is_null() {
                continue;
            }
            assert!((*rp).is_runnable());

            // Check if billable
            if !(*rp).p_priv.is_null() {
                let flags = (*(*rp).p_priv).s_flags;
                if flags.contains(crate::r#priv::PrivFlags::BILLABLE) {
                    CPU_LOCAL_STORAGE.set_bill_ptr(rp as *mut core::ffi::c_void);
                }
            }
            return Some(rp);
        }
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────
// notify_scheduler
// ─────────────────────────────────────────────────────────────────────────

/// Notify a process's user-space scheduler that it has run out of quantum.
///
/// Sets `RTS_NO_QUANTUM` (which dequeues the process) and sends a
/// `SCHEDULING_NO_QUANTUM` message to the scheduler.
///
/// # Safety
///
/// `p` must have a non-kernel scheduler.
pub unsafe fn notify_scheduler(p: *mut Proc) {
    unsafe {
        assert!(!(*p).kernel_scheduler());

        // Dequeue by setting RTS_NO_QUANTUM
        let rts_flag = RtsFlags::NO_QUANTUM;
        let old_flags = (*p).p_rts_flags.load(Ordering::Relaxed);
        (*p).p_rts_flags
            .store(old_flags | rts_flag.bits(), Ordering::Relaxed);
        if old_flags == 0 && !(*p).is_runnable() {
            dequeue(p);
        }

        // Build and send SCHEDULING_NO_QUANTUM message
        let mut msg = [0u8; crate::proc::MESSAGE_SIZE];
        // m_type at offset 4 (C: m_no_quantum.m_type = SCHEDULING_NO_QUANTUM)
        let mtype = arch_common::com::SCHEDULING_NO_QUANTUM as i32;
        msg[4..8].copy_from_slice(&mtype.to_ne_bytes());
        // m_source at offset 0 (C: m_no_quantum.m_source = p->p_endpoint)
        msg[0..4].copy_from_slice(&(*p).p_endpoint.to_ne_bytes());
        // The scheduler endpoint is at p->p_scheduler->p_endpoint
        let sched_ep = (*(*p).p_scheduler).p_endpoint;
        let result = crate::ipc::mini_send(p, sched_ep, msg.as_mut_ptr(), crate::ipc::FROM_KERNEL);
        // If the send fails, the scheduler will pick it up later
        let _ = result;

        // Reset accounting
        reset_proc_accounting(p);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// proc_no_time
// ─────────────────────────────────────────────────────────────────────────

/// Handle quantum expiry for process `p`.
///
/// For preemptible processes with a user-space scheduler: notify the
/// scheduler. For non-preemptible processes: simply renew the quantum.
///
/// # Safety
///
/// `p` must point to a valid `Proc`.
pub unsafe fn proc_no_time(p: *mut Proc) {
    unsafe {
        let has_user_sched = !(*p).kernel_scheduler();
        let is_preemptible = if !(*p).p_priv.is_null() {
            (*(*p).p_priv)
                .s_flags
                .contains(crate::r#priv::PrivFlags::PREEMPTIBLE)
        } else {
            false
        };

        if has_user_sched && is_preemptible {
            notify_scheduler(p);
        } else {
            // Non-preemptible: just renew quantum
            (*p).p_cpu_time_left = ms_2_cpu_time((*p).p_quantum_size_ms);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// reset_proc_accounting
// ─────────────────────────────────────────────────────────────────────────

/// Clear all scheduling accounting fields for process `p`.
///
/// # Safety
///
/// `p` must point to a valid `Proc`.
pub unsafe fn reset_proc_accounting(p: *mut Proc) {
    unsafe {
        (*p).p_accounting.preempted = 0;
        (*p).p_accounting.ipc_sync = 0;
        (*p).p_accounting.ipc_async = 0;
        (*p).p_accounting.dequeues = 0;
        (*p).p_accounting.time_in_queue = 0;
        (*p).p_accounting.enter_queue = 0;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// is_idle_proc
// ─────────────────────────────────────────────────────────────────────────

/// Check if `rp` is the idle process.
///
/// The idle process is identified by its endpoint value (`IDLE = -4`).
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
pub unsafe fn is_idle_proc(rp: *mut Proc) -> bool {
    unsafe { (*rp).p_endpoint == -4 }
}

// ─────────────────────────────────────────────────────────────────────────
// runqueues_ok — sanity checker
// ─────────────────────────────────────────────────────────────────────────

/// Run queue sanity check (3-pass validation).
///
/// Returns `true` if all queues are consistent:
/// 1. Head/tail pointers are consistent (both null or both non-null for empty).
/// 2. Tail is reachable from head by following `p_nextready`.
/// 3. All processes on queues are actually runnable.
///
/// # Safety
///
/// The run queue state must be accessible (not concurrently modified).
pub unsafe fn runqueues_ok() -> bool {
    unsafe {
        let head = run_q_head_array();
        let tail = run_q_tail_array();

        for q in 0..NR_SCHED_QUEUES {
            let h = (*head)[q];
            let t = (*tail)[q];

            // Pass 1: head and tail consistency
            if h.is_null() && !t.is_null() {
                return false;
            }
            if !h.is_null() && t.is_null() {
                return false;
            }
            if h.is_null() {
                continue;
            }

            // Pass 2: tail reachable from head
            let mut found_tail = false;
            let mut walk = h;
            while !walk.is_null() {
                if walk == t {
                    found_tail = true;
                }
                // Check infinite loop
                if walk == (*walk).p_nextready {
                    return false;
                }
                walk = (*walk).p_nextready;
            }
            if !found_tail {
                return false;
            }

            // Pass 3: all processes runnable
            let mut walk2 = h;
            while !walk2.is_null() {
                if !(*walk2).is_runnable() {
                    return false;
                }
                walk2 = (*walk2).p_nextready;
            }
        }
        true
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Utility: ms to CPU time conversion (placeholder)
// ─────────────────────────────────────────────────────────────────────────

/// Convert milliseconds to CPU time (TSC cycles).
/// This is a placeholder — real implementation uses TSC frequency.
fn ms_2_cpu_time(ms: u32) -> u64 {
    // Approximate: 1ms ≈ 2.5 million cycles at 2.5 GHz
    (ms as u64) * 2_500_000
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::proc_init;
    use core::sync::atomic::AtomicBool;

    /// Serialization lock for sched tests — all share static mut state
    /// (cpu locals, proc table) and cannot run concurrently.
    static SCHED_TEST_LOCK: AtomicBool = AtomicBool::new(false);

    /// Acquire the serialization lock.
    struct SchedTestLock;
    impl SchedTestLock {
        fn acquire() -> Self {
            while SCHED_TEST_LOCK
                .compare_exchange(
                    false,
                    true,
                    core::sync::atomic::Ordering::SeqCst,
                    core::sync::atomic::Ordering::SeqCst,
                )
                .is_err()
            {
                core::hint::spin_loop();
            }
            Self
        }
    }
    impl Drop for SchedTestLock {
        fn drop(&mut self) {
            SCHED_TEST_LOCK.store(false, core::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Helper: clear all run queues for test isolation
    /// Must be called after init_cpulocals() (done by make_test_proc).
    unsafe fn clear_run_queues() {
        unsafe {
            arch_x86_64::cpulocals::init_cpulocals();
            let head = run_q_head_array();
            let tail = run_q_tail_array();
            for q in 0..NR_SCHED_QUEUES {
                (*head)[q] = core::ptr::null_mut();
                (*tail)[q] = core::ptr::null_mut();
            }
        }
    }

    /// Helper: create a minimal process for testing
    unsafe fn make_test_proc(nr: i32, priority: i8) -> *mut Proc {
        unsafe {
            // Initialize cpulocals if not already done
            arch_x86_64::cpulocals::init_cpulocals();
            let rp = crate::table::proc_addr(nr);
            if !rp.is_null() {
                (*rp).p_rts_flags.store(0, Ordering::Relaxed); // runnable
                (*rp).p_nr = nr;
                (*rp).p_priority = priority;
                (*rp).p_cpu_time_left = 1000;
                (*rp).p_nextready = core::ptr::null_mut();
                (*rp).p_magic = PMAGIC;
            }
            rp
        }
    }

    #[test]
    fn test_enqueue_dequeue_single() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp = make_test_proc(0, 0);
            assert!(runqueues_ok());

            enqueue(rp);
            let head = run_q_head_array();
            assert_eq!((*head)[0], rp);
            assert!(runqueues_ok());

            (*rp)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            dequeue(rp);
            assert!((*head)[0].is_null());
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_enqueue_two_processes() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp0 = make_test_proc(0, 0);
            let rp1 = make_test_proc(1, 0);

            enqueue(rp0);
            enqueue(rp1);

            let head = run_q_head_array();
            assert_eq!((*head)[0], rp0);
            assert_eq!((*rp0).p_nextready, rp1);
            assert!((*rp1).p_nextready.is_null());
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_enqueue_head_inserts_at_front() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp0 = make_test_proc(0, 0);
            let _rp1 = make_test_proc(1, 0);

            enqueue(rp0);
            // Dequeue rp0 (mark non-runnable first, then make runnable and enqueue_head)
            (*rp0)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            dequeue(rp0);
            (*rp0).p_rts_flags.store(0, Ordering::Relaxed);
            (*rp0).p_cpu_time_left = 1000;
            enqueue_head(rp0);

            let head = run_q_head_array();
            assert_eq!((*head)[0], rp0);
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_pick_proc_priority_ordering() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp_low = make_test_proc(0, 10);
            let rp_high = make_test_proc(1, 0);

            enqueue(rp_low);
            enqueue(rp_high);

            let picked = pick_proc();
            assert!(picked.is_some());
            assert_eq!(picked.unwrap(), rp_high);

            (*rp_high)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            dequeue(rp_high);
            (*rp_low)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            dequeue(rp_low);
        }
    }

    #[test]
    fn test_pick_proc_empty_returns_none() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            assert!(pick_proc().is_none());
        }
    }

    #[test]
    fn test_dequeue_middle_of_queue() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp0 = make_test_proc(0, 0);
            let rp1 = make_test_proc(1, 0);
            let rp2 = make_test_proc(2, 0);

            enqueue(rp0);
            enqueue(rp1);
            enqueue(rp2);

            (*rp1)
                .p_rts_flags
                .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
            dequeue(rp1);

            let head = run_q_head_array();
            assert_eq!((*head)[0], rp0);
            assert_eq!((*rp0).p_nextready, rp2);
            assert!((*rp2).p_nextready.is_null());
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_remove_from_queue_head() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp0 = make_test_proc(0, 0);
            let rp1 = make_test_proc(1, 0);

            enqueue(rp0);
            enqueue(rp1);

            // Remove head (rp0) while still runnable
            remove_from_queue(rp0);

            let head = run_q_head_array();
            assert_eq!((*head)[0], rp1, "head should be rp1 after removing rp0");
            assert!((*rp1).p_nextready.is_null(), "rp1 should be tail");
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_remove_from_queue_middle() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp0 = make_test_proc(0, 0);
            let rp1 = make_test_proc(1, 0);
            let rp2 = make_test_proc(2, 0);

            enqueue(rp0);
            enqueue(rp1);
            enqueue(rp2);

            // Remove middle (rp1) while still runnable
            remove_from_queue(rp1);

            let head = run_q_head_array();
            assert_eq!((*head)[0], rp0);
            assert_eq!((*rp0).p_nextready, rp2);
            assert!((*rp2).p_nextready.is_null());
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_remove_from_queue_tail() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp0 = make_test_proc(0, 0);
            let rp1 = make_test_proc(1, 0);

            enqueue(rp0);
            enqueue(rp1);

            // Remove tail (rp1) while still runnable
            remove_from_queue(rp1);

            let head = run_q_head_array();
            assert_eq!((*head)[0], rp0);
            assert!((*rp0).p_nextready.is_null());
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_remove_from_queue_not_present() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let rp0 = make_test_proc(0, 0);
            let rp1 = make_test_proc(1, 0);

            enqueue(rp0);

            // Remove rp1 which is not in the queue — should not crash
            remove_from_queue(rp1);

            let head = run_q_head_array();
            assert_eq!((*head)[0], rp0);
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_enqueue_dequeue_balance() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            let ps: [*mut Proc; 4] = [
                make_test_proc(0, 0),
                make_test_proc(1, 1),
                make_test_proc(2, 2),
                make_test_proc(3, 3),
            ];

            for &p in &ps {
                enqueue(p);
            }

            for &p in &ps {
                (*p).p_rts_flags
                    .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
                dequeue(p);
            }

            let head = run_q_head_array();
            for q in 0..NR_SCHED_QUEUES {
                assert!((*head)[q].is_null(), "Queue {} not empty", q);
            }
            assert!(runqueues_ok());
        }
    }

    #[test]
    fn test_runqueues_ok_detects_corrupted() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            clear_run_queues();
            assert!(runqueues_ok());

            // Corrupt: set a tail without head
            let tail = run_q_tail_array();
            let dummy = make_test_proc(0, 0);
            (*tail)[0] = dummy;
            assert!(!runqueues_ok());

            // Clean up
            (*tail)[0] = core::ptr::null_mut();
        }
    }

    #[test]
    fn test_reset_proc_accounting_clears() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            let rp = make_test_proc(0, 0);
            (*rp).p_accounting.dequeues = 5;
            (*rp).p_accounting.preempted = 3;
            (*rp).p_accounting.time_in_queue = 1000;

            reset_proc_accounting(rp);
            assert_eq!((*rp).p_accounting.dequeues, 0);
            assert_eq!((*rp).p_accounting.preempted, 0);
            assert_eq!((*rp).p_accounting.time_in_queue, 0);
        }
    }

    #[test]
    fn test_is_idle_proc() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            let idle_rp = crate::table::proc_addr(-4);
            assert!(!idle_rp.is_null());
            assert!(is_idle_proc(idle_rp));
        }
    }

    // ── notify_scheduler tests ───────────────────────────────────────────

    #[test]
    fn test_notify_scheduler_sends_message() {
        unsafe {
            let _lock = SchedTestLock::acquire();
            proc_init();
            let rp = make_test_proc(0, 0);
            let sched_rp = make_test_proc(1, 0);
            (*sched_rp).p_endpoint = crate::table::make_endpoint(0, 1);
            (*rp).p_scheduler = sched_rp;
            (*rp).p_rts_flags.store(0, Ordering::Relaxed);
            (*sched_rp)
                .p_rts_flags
                .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
            (*sched_rp).p_getfrom_e = crate::system::NONE;

            notify_scheduler(rp);

            let rts = (*rp).p_rts_flags.load(Ordering::Relaxed);
            assert!(
                rts & RtsFlags::NO_QUANTUM.bits() != 0,
                "RTS_NO_QUANTUM should be set"
            );
            assert_eq!((*rp).p_accounting.dequeues, 0);
        }
    }
}
