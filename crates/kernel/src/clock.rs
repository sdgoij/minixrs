//! Clock/timer module — adapted from `minix/kernel/clock.c`
//!
//! Manages the monotonic and realtime clocks, watchdog timer queue, per-process
//! virtual timers, and load-average accounting. The timer interrupt handler
//! (`timer_int_handler`) is called on every clock tick on the BSP.

use core::sync::atomic::{AtomicI64, AtomicPtr, AtomicU64, Ordering};

use crate::glo::LOADINFO;
use crate::glo::SYSTEM_HZ;
use crate::r#priv::{MinixTimer, PrivFlags};
use crate::proc::{MiscFlags, Proc};
use crate::system::cause_sig;

// Constants

/// Special expiration time meaning "no timer is set".
pub const TMR_NEVER: u64 = u64::MAX;

/// Load average history length.
const _LOAD_HISTORY: usize = 150;

/// Granularity of each load sample (seconds).
const _LOAD_UNIT_SECS: u64 = 1;

/// POSIX signal: virtual timer alarm (SIGVTALRM).
const SIGVTALRM: i32 = 26;

/// POSIX signal: profiling timer alarm (SIGPROF).
const SIGPROF: i32 = 27;

// Clock state

/// Monotonic time since boot in ticks (BSP only).
static MONOTONIC: AtomicU64 = AtomicU64::new(0);

/// Wall time since boot in ticks.  May be slowed/sped up via adjtime.
static REALTIME: AtomicU64 = AtomicU64::new(0);

/// Number of ticks to adjust realtime by (positive = speed up, negative = slow down).
static ADJTIME_DELTA: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

/// System boot time in seconds since epoch (for SYS_STIME/SYS_SETTIME).
static BOOTTIME: AtomicI64 = AtomicI64::new(0);

/// Queue of CLOCK timers.
static CLOCK_TIMERS: AtomicPtr<MinixTimer> = AtomicPtr::new(core::ptr::null_mut());

/// Monotonic time at which the next timer in the queue expires.
static NEXT_TIMEOUT: AtomicU64 = AtomicU64::new(TMR_NEVER);

// Timer queue management  (from `minix/include/minix/timers.h`)

/// Insert a timer into the timer queue, sorted by expiration time.
///
/// # Safety
///
/// `tp` must point to a valid, unqueued `MinixTimer`. `timers` must point to
/// the queue head.
pub unsafe fn tmrs_settimer(
    timers: *mut *mut MinixTimer,
    tp: *mut MinixTimer,
    exp_time: u64,
    watchdog: usize,
    _param: *mut u8,
) {
    unsafe {
        (*tp).tmr_exp_time = exp_time;
        (*tp).tmr_func = watchdog;

        let mut prev: *mut MinixTimer = core::ptr::null_mut();
        let mut cur = *timers;
        while !cur.is_null() && (*cur).tmr_exp_time <= exp_time {
            prev = cur;
            cur = (*cur).tmr_next;
        }
        (*tp).tmr_next = cur;
        if !prev.is_null() {
            (*prev).tmr_next = tp;
        } else {
            *timers = tp;
        }
    }
}

/// Remove a timer from the timer queue.
///
/// # Safety
///
/// `tp` must point to a timer that may or may not be queued.
pub unsafe fn tmrs_clrtimer(timers: *mut *mut MinixTimer, tp: *mut MinixTimer, _param: *mut u8) {
    unsafe {
        let mut prev: *mut MinixTimer = core::ptr::null_mut();
        let mut cur = *timers;
        while !cur.is_null() {
            if cur == tp {
                if !prev.is_null() {
                    (*prev).tmr_next = (*tp).tmr_next;
                } else {
                    *timers = (*tp).tmr_next;
                }
                break;
            }
            prev = cur;
            cur = (*cur).tmr_next;
        }
        (*tp).tmr_next = core::ptr::null_mut();
    }
}

/// Check for expired timers and run their watchdog functions.
///
/// Returns the number of timers that expired.
///
/// # Safety
///
/// All timers in the queue must have valid function pointers.
pub unsafe fn tmrs_exptimers(timers: *mut *mut MinixTimer, now: u64, _param: *mut u8) -> usize {
    let mut count = 0;
    unsafe {
        while !(*timers).is_null() && (**timers).tmr_exp_time <= now {
            let tp = *timers;
            *timers = (*tp).tmr_next;
            (*tp).tmr_next = core::ptr::null_mut();
            if (*tp).tmr_func != 0 {
                let func: fn(*mut MinixTimer) = core::mem::transmute((*tp).tmr_func);
                func(tp);
            }
            count += 1;
        }
    }
    count
}

// Clock accessors

/// Get monotonic time since boot in ticks.
pub fn get_monotonic() -> u64 {
    MONOTONIC.load(Ordering::Relaxed)
}

/// Set monotonic time.
pub fn set_monotonic(val: u64) {
    MONOTONIC.store(val, Ordering::Relaxed);
}

/// Get wall time since boot in ticks.
pub fn get_realtime() -> u64 {
    REALTIME.load(Ordering::Relaxed)
}

/// Set wall time.
pub fn set_realtime(val: u64) {
    REALTIME.store(val, Ordering::Relaxed);
}

/// Set the adjtime delta.
/// Get the system boot time (seconds since epoch).
pub fn get_boottime() -> i64 {
    BOOTTIME.load(Ordering::Relaxed)
}

/// Set the system boot time (seconds since epoch).
pub fn set_boottime(val: i64) {
    BOOTTIME.store(val, Ordering::Relaxed);
}

pub fn set_adjtime_delta(ticks: i32) {
    ADJTIME_DELTA.store(ticks, Ordering::Relaxed);
}

/// Get the adjtime delta.
pub fn get_adjtime_delta() -> i32 {
    ADJTIME_DELTA.load(Ordering::Relaxed)
}

// Kernel timer interface

/// Insert a new timer in the active timers list and update `next_timeout`.
///
/// # Safety
///
/// `tp` must point to a valid, unqueued `MinixTimer`.
pub unsafe fn set_kernel_timer(tp: *mut MinixTimer, exp_time: u64, watchdog: usize) {
    unsafe {
        let timers = CLOCK_TIMERS.as_ptr();
        tmrs_settimer(timers, tp, exp_time, watchdog, core::ptr::null_mut());
        let head = CLOCK_TIMERS.load(Ordering::Relaxed);
        NEXT_TIMEOUT.store(
            if head.is_null() {
                TMR_NEVER
            } else {
                (*head).tmr_exp_time
            },
            Ordering::Relaxed,
        );
    }
}

/// Remove a timer from the active timers list and update `next_timeout`.
///
/// # Safety
///
/// `tp` must point to a timer that may or may not be queued.
pub unsafe fn reset_kernel_timer(tp: *mut MinixTimer) {
    unsafe {
        let timers = CLOCK_TIMERS.as_ptr();
        tmrs_clrtimer(timers, tp, core::ptr::null_mut());
        let head = CLOCK_TIMERS.load(Ordering::Relaxed);
        NEXT_TIMEOUT.store(
            if head.is_null() {
                TMR_NEVER
            } else {
                (*head).tmr_exp_time
            },
            Ordering::Relaxed,
        );
    }
}

// Virtual timer check

/// Check if a process's virtual or profiling timer has expired and send the
/// appropriate signal.
///
/// Called from the timer interrupt handler on every tick.
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
unsafe fn vtimer_check(rp: *mut Proc) {
    unsafe {
        let mf = (*rp).p_misc_flags.load(Ordering::Relaxed);

        // Check if the virtual timer expired.
        if (mf & MiscFlags::VIRT_TIMER.bits()) != 0 && (*rp).p_virt_left == 0 {
            (*rp)
                .p_misc_flags
                .fetch_and(!MiscFlags::VIRT_TIMER.bits(), Ordering::Relaxed);
            (*rp).p_virt_left = 0;
            cause_sig((*rp).p_nr, SIGVTALRM);
        }

        // Check if the profile timer expired.
        if (mf & MiscFlags::PROF_TIMER.bits()) != 0 && (*rp).p_prof_left == 0 {
            (*rp)
                .p_misc_flags
                .fetch_and(!MiscFlags::PROF_TIMER.bits(), Ordering::Relaxed);
            (*rp).p_prof_left = 0;
            cause_sig((*rp).p_nr, SIGPROF);
        }
    }
}

// Load average update

/// Update the load-average circular buffer.
///
/// Called from `timer_int_handler` on every tick.
unsafe fn load_update() {
    unsafe {
        let mono = MONOTONIC.load(Ordering::Relaxed);
        let hz = SYSTEM_HZ.load(Ordering::Relaxed) as u64;

        let slot = mono
            .checked_div(hz)
            .map(|m| (m / _LOAD_UNIT_SECS) % _LOAD_HISTORY as u64)
            .unwrap_or(0) as u16;

        let loadinfo = LOADINFO.get();
        if slot != (*loadinfo).proc_last_slot {
            (*loadinfo).proc_load_history[slot as usize] = 0;
            (*loadinfo).proc_last_slot = slot;
        }

        // Count how many processes are ready across all priority queues
        let mut enqueued: u16 = 0;
        let head_ptr = crate::hal::sched_run_q_head();
        for q in 0..crate::hal::sched_nr_queues() {
            let mut p = (*head_ptr)[q];
            while !p.is_null() {
                enqueued = enqueued.saturating_add(1);
                let proc_ptr = p as *mut Proc;
                p = (*proc_ptr).p_nextready as *mut core::ffi::c_void;
            }
        }

        (*loadinfo).proc_load_history[slot as usize] =
            (*loadinfo).proc_load_history[slot as usize].saturating_add(enqueued);
        (*loadinfo).last_clock = mono;
    }
}

// Timer interrupt handler

/// Timer interrupt handler — called on every clock tick on the BSP.
///
/// Updates monotonic/realtime clocks, process accounting, virtual timers,
/// and checks for expired watchdog timers.
///
/// # Safety
///
/// Must only be called from the timer interrupt context.
pub unsafe fn timer_int_handler() {
    unsafe {
        let mono = MONOTONIC.fetch_add(1, Ordering::Relaxed) + 1;

        // Limit adjtime changes to every other tick.
        let delta = ADJTIME_DELTA.load(Ordering::Relaxed);
        if delta != 0 && (mono & 0x1) != 0 {
            if delta > 0 {
                REALTIME.fetch_add(2, Ordering::Relaxed);
                ADJTIME_DELTA.store(delta - 1, Ordering::Relaxed);
            } else {
                // Negative adjtime: don't increment this tick.
                // (delta is negative, so adding 1 brings it closer to 0)
                ADJTIME_DELTA.store(delta + 1, Ordering::Relaxed);
            }
        } else {
            REALTIME.fetch_add(1, Ordering::Relaxed);
        }

        let p = crate::hal::sched_current_proc() as *mut Proc;
        let billp = crate::hal::sched_bill_proc() as *mut Proc;

        if !p.is_null() {
            (*p).p_user_time += 1;

            // If the current process is not billable, charge the billable
            // process for system time instead.
            let is_billable = if !(*p).p_priv.is_null() {
                (*(*p).p_priv).s_flags.contains(PrivFlags::BILLABLE)
            } else {
                false
            };

            if !is_billable && !billp.is_null() {
                (*billp).p_sys_time += 1;
            }

            // Decrement virtual timers.
            let p_mf = (*p).p_misc_flags.load(Ordering::Relaxed);
            if (p_mf & MiscFlags::VIRT_TIMER.bits()) != 0 && (*p).p_virt_left > 0 {
                (*p).p_virt_left -= 1;
            }
            if (p_mf & MiscFlags::PROF_TIMER.bits()) != 0 && (*p).p_prof_left > 0 {
                (*p).p_prof_left -= 1;
            }

            // If current process is not billable, also decrement profile
            // timer of the billed process.
            if !is_billable && !billp.is_null() {
                let bill_mf = (*billp).p_misc_flags.load(Ordering::Relaxed);
                if (bill_mf & MiscFlags::PROF_TIMER.bits()) != 0 && (*billp).p_prof_left > 0 {
                    (*billp).p_prof_left -= 1;
                }
            }

            // Check if a process-virtual timer expired.
            vtimer_check(p);

            if !billp.is_null() && p != billp {
                vtimer_check(billp);
            }
        }

        load_update();

        if NEXT_TIMEOUT.load(Ordering::Relaxed) <= mono {
            let timers = CLOCK_TIMERS.as_ptr();
            tmrs_exptimers(timers, mono, core::ptr::null_mut());
            let head = CLOCK_TIMERS.load(Ordering::Relaxed);
            NEXT_TIMEOUT.store(
                if head.is_null() {
                    TMR_NEVER
                } else {
                    (*head).tmr_exp_time
                },
                Ordering::Relaxed,
            );
        }
    }
}

// Time conversion utilities

/// Convert milliseconds to CPU cycles.
pub fn ms_2_cpu_time(ms: usize) -> u64 {
    let freq_hz = crate::glo::cpu_get_freq(0);
    if freq_hz == 0 {
        return 0;
    }
    (ms as u64) * (freq_hz / 1000)
}

/// Convert CPU cycles to milliseconds.
pub fn cpu_time_2_ms(cpu_time: u64) -> usize {
    let freq_hz = crate::glo::cpu_get_freq(0);
    if freq_hz == 0 {
        return 0;
    }
    (cpu_time / (freq_hz / 1000)) as usize
}

/// Set the system clock frequency (HZ).
pub fn set_system_hz(hz: u32) {
    SYSTEM_HZ.store(hz, Ordering::Relaxed);
}

// Cycle accounting (architecture-dependent stubs)

/// Initialize cycle accounting.
pub fn cycles_accounting_init() {
    // No-op: TSC-based cycle accounting is initialized by the architecture layer.
}

/// Stop accounting for the current process and account for kernel or idle time.
///
/// # Safety
///
/// Must be called in a context where the interrupt handler is guaranteed not to
/// run concurrently with this operation (e.g., interrupts disabled).
pub unsafe fn context_stop(_p: *mut Proc) {
    // Will read TSC and update cycle counters when the arch layer provides
    // the necessary primitives.
}

/// Wrapper for `context_stop` suitable for calling from assembly.
///
/// # Safety
///
/// Must be called in a context where the interrupt handler is guaranteed
/// not to run concurrently (e.g., interrupts disabled).
pub unsafe fn context_stop_idle() {
    // No-op for now; see `context_stop`.
}

// Timer initialization stubs

/// Initialize the boot CPU timer.
///
/// # Safety
///
/// Must be called exactly once on the BSP.
pub unsafe fn boot_cpu_init_timer(_freq: u32) -> i32 {
    // Delegates to the arch layer; currently a stub.
    0
}

/// Initialize an application CPU timer.
///
/// # Safety
///
/// Must be called once per AP.
pub unsafe fn app_cpu_init_timer(_freq: u32) -> i32 {
    0
}

// Compile-time size assertions

const _: () = {
    let _ = core::mem::transmute::<*mut MinixTimer, [u8; 8]>;
    let _ = core::mem::transmute::<MinixTimer, [u8; 32]>;
    assert!(size_of::<MinixTimer>() == 32);
};

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    /// TMR_NEVER should be the maximum u64 value.
    #[test]
    fn test_tmr_never_value() {
        assert_eq!(TMR_NEVER, u64::MAX);
    }

    /// MinixTimer should be 32 bytes (pointer + u64 + usize + usize).
    #[test]
    fn test_minix_timer_size() {
        assert_eq!(size_of::<MinixTimer>(), 32);
    }

    /// Clock accessors round-trip correctly.
    #[test]
    fn test_clock_accessors() {
        set_monotonic(0);
        set_realtime(0);
        assert_eq!(get_monotonic(), 0);
        assert_eq!(get_realtime(), 0);

        set_monotonic(12345);
        set_realtime(67890);
        assert_eq!(get_monotonic(), 12345);
        assert_eq!(get_realtime(), 67890);
    }

    /// Adjtime delta round-trips.
    #[test]
    fn test_adjtime_delta() {
        set_adjtime_delta(0);
        assert_eq!(get_adjtime_delta(), 0);

        set_adjtime_delta(42);
        assert_eq!(get_adjtime_delta(), 42);

        set_adjtime_delta(-5);
        assert_eq!(get_adjtime_delta(), -5);
    }

    /// Timer queue: insert and remove a single timer.
    #[test]
    fn test_timer_queue_single() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr = MinixTimer::default();

        unsafe {
            tmrs_settimer(
                &mut timers as *mut *mut _,
                &mut tmr,
                100,
                0,
                core::ptr::null_mut(),
            );
            assert!(!timers.is_null());
            assert_eq!((*timers).tmr_exp_time, 100);
            assert!((*timers).tmr_next.is_null());

            tmrs_clrtimer(&mut timers as *mut *mut _, &mut tmr, core::ptr::null_mut());
            assert!(timers.is_null());
        }
    }

    /// Timer queue: multiple timers inserted in expiration order.
    #[test]
    fn test_timer_queue_ordered() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr1 = MinixTimer::default();
        let mut tmr2 = MinixTimer::default();
        let mut tmr3 = MinixTimer::default();
        let timers_ptr: *mut *mut MinixTimer = &mut timers as *mut _;

        unsafe {
            // Insert in non-sorted order
            tmrs_settimer(timers_ptr, &mut tmr3, 300, 0, core::ptr::null_mut());
            tmrs_settimer(timers_ptr, &mut tmr1, 100, 0, core::ptr::null_mut());
            tmrs_settimer(timers_ptr, &mut tmr2, 200, 0, core::ptr::null_mut());

            // Verify order
            assert_eq!((*timers).tmr_exp_time, 100);
            assert_eq!((*(*timers).tmr_next).tmr_exp_time, 200);
            assert_eq!((*(*(*timers).tmr_next).tmr_next).tmr_exp_time, 300);
            assert!((*(*(*timers).tmr_next).tmr_next).tmr_next.is_null());
        }
    }

    /// Timer queue: expiration runs the watchdog.
    #[test]
    fn test_timer_expiration() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr = MinixTimer::default();
        static mut CALLED: bool = false;

        unsafe extern "C" fn watchdog(tp: *mut MinixTimer) {
            unsafe {
                CALLED = true;
                assert!(!tp.is_null());
            }
        }

        unsafe {
            let timers_ptr: *mut *mut MinixTimer = &mut timers as *mut _;
            let wd = watchdog as *const () as usize;
            tmrs_settimer(timers_ptr, &mut tmr, 50, wd, core::ptr::null_mut());

            let count = tmrs_exptimers(timers_ptr, 100, core::ptr::null_mut());
            assert_eq!(count, 1);
            assert!(CALLED);
            assert!(timers.is_null());
        }
    }

    /// Timer queue: no expiration if time not reached.
    #[test]
    fn test_timer_not_expired() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr = MinixTimer::default();

        unsafe {
            let timers_ptr: *mut *mut MinixTimer = &mut timers as *mut _;
            tmrs_settimer(timers_ptr, &mut tmr, 100, 0, core::ptr::null_mut());
            let count = tmrs_exptimers(timers_ptr, 50, core::ptr::null_mut());
            assert_eq!(count, 0);
            assert!(!timers.is_null());
        }
    }

    /// Timer queue: removing a timer that is not in the queue is safe.
    #[test]
    fn test_timer_clrtimer_not_queued() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr = MinixTimer::default();

        unsafe {
            let timers_ptr: *mut *mut MinixTimer = &mut timers as *mut _;
            // Remove a timer that was never inserted — should not crash.
            tmrs_clrtimer(timers_ptr, &mut tmr, core::ptr::null_mut());
            assert!(timers.is_null());
        }
    }

    /// Multiple expirations in a single call.
    #[test]
    fn test_timer_multiple_expirations() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr1 = MinixTimer::default();
        let mut tmr2 = MinixTimer::default();
        let timers_ptr: *mut *mut MinixTimer = &mut timers as *mut _;

        unsafe {
            tmrs_settimer(timers_ptr, &mut tmr1, 50, 0, core::ptr::null_mut());
            tmrs_settimer(timers_ptr, &mut tmr2, 100, 0, core::ptr::null_mut());

            let count = tmrs_exptimers(timers_ptr, 200, core::ptr::null_mut());
            assert_eq!(count, 2);
            assert!(timers.is_null());
        }
    }

    /// Partial expiration: only early timers expire.
    #[test]
    fn test_timer_partial_expiration() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr1 = MinixTimer::default();
        let mut tmr2 = MinixTimer::default();
        let mut tmr3 = MinixTimer::default();
        let timers_ptr: *mut *mut MinixTimer = &mut timers as *mut _;

        unsafe {
            tmrs_settimer(timers_ptr, &mut tmr3, 300, 0, core::ptr::null_mut());
            tmrs_settimer(timers_ptr, &mut tmr1, 50, 0, core::ptr::null_mut());
            tmrs_settimer(timers_ptr, &mut tmr2, 100, 0, core::ptr::null_mut());

            let count = tmrs_exptimers(timers_ptr, 150, core::ptr::null_mut());
            assert_eq!(count, 2);

            // Only tmr3 should remain
            assert!(!timers.is_null());
            assert_eq!((*timers).tmr_exp_time, 300);
        }
    }

    /// Removing the head of the timer queue.
    #[test]
    fn test_timer_remove_head() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr1 = MinixTimer::default();
        let mut tmr2 = MinixTimer::default();
        let timers_ptr: *mut *mut MinixTimer = &mut timers as *mut _;

        unsafe {
            tmrs_settimer(timers_ptr, &mut tmr1, 50, 0, core::ptr::null_mut());
            tmrs_settimer(timers_ptr, &mut tmr2, 100, 0, core::ptr::null_mut());

            // Remove head (tmr1)
            tmrs_clrtimer(timers_ptr, &mut tmr1, core::ptr::null_mut());
            assert_eq!((*timers).tmr_exp_time, 100);
            assert!((*timers).tmr_next.is_null());
        }
    }

    /// Removing a middle element from the timer queue.
    #[test]
    fn test_timer_remove_middle() {
        let mut timers: *mut MinixTimer = core::ptr::null_mut();
        let mut tmr1 = MinixTimer::default();
        let mut tmr2 = MinixTimer::default();
        let mut tmr3 = MinixTimer::default();
        let timers_ptr: *mut *mut MinixTimer = &mut timers as *mut _;

        unsafe {
            tmrs_settimer(timers_ptr, &mut tmr1, 50, 0, core::ptr::null_mut());
            tmrs_settimer(timers_ptr, &mut tmr2, 100, 0, core::ptr::null_mut());
            tmrs_settimer(timers_ptr, &mut tmr3, 150, 0, core::ptr::null_mut());

            // Remove middle (tmr2)
            tmrs_clrtimer(timers_ptr, &mut tmr2, core::ptr::null_mut());
            assert_eq!((*timers).tmr_exp_time, 50);
            assert_eq!((*(*timers).tmr_next).tmr_exp_time, 150);
            assert!((*(*timers).tmr_next).tmr_next.is_null());
        }
    }

    /// Kernel timer interface: set and reset.
    #[test]
    fn test_kernel_timer_interface() {
        let mut tmr = MinixTimer::default();

        unsafe {
            set_kernel_timer(&mut tmr, 200, 0);
            // Should not crash
            reset_kernel_timer(&mut tmr);
        }
    }

    /// Time conversion functions.
    #[test]
    fn test_time_conversion() {
        // Just check they don't panic and return reasonable values
        let _ = ms_2_cpu_time(0);
        let _ = cpu_time_2_ms(0);
    }

    /// set_system_hz updates the global.
    #[test]
    fn test_set_system_hz() {
        set_system_hz(100);
        assert_eq!(SYSTEM_HZ.load(Ordering::Relaxed), 100);
        set_system_hz(60); // restore default
    }

    /// Clock state starts at zero.
    #[test]
    fn test_clock_starts_zero() {
        // These tests rely on global state; run in sequence.
        set_monotonic(0);
        set_realtime(0);
        assert_eq!(get_monotonic(), 0);
        assert_eq!(get_realtime(), 0);
    }

    /// Monotonic increments (fetch_add pattern check).
    #[test]
    fn test_monotonic_increment() {
        set_monotonic(0);
        let prev = MONOTONIC.fetch_add(1, Ordering::Relaxed);
        assert_eq!(prev, 0);
        assert_eq!(get_monotonic(), 1);
    }
}
