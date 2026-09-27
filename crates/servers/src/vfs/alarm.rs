//! VFS server deadlines — the timer behind every blocking readiness wait.
//!
//! `select(2)` and `poll(2)` must return when their timeout expires even if no
//! descriptor became ready. The kernel's only "wake this server later" facility
//! is `SYS_SETALARM`, which the input server already uses for its periodic
//! virtio poll: it arms a per-process timer and the kernel notifies the process
//! from `CLOCK` (`-3`) when it fires. VFS arms exactly one — the earliest
//! deadline among its suspended waits — and re-arms it whenever that set
//! changes. `vfs/main.rs` routes a notification back here (`alarm_ticks`).
//!
//! Deadlines are absolute monotonic ticks. `SYS_SETALARM` with `abs_time` set
//! takes an absolute tick, so re-arming for a later wait does not accumulate
//! rounding drift.

#[cfg(target_os = "minix")]
mod imp {
    use core::sync::atomic::{AtomicU64, Ordering};

    /// SYS_SETALARM, the kernel call that arms the caller's alarm timer.
    const SYS_SETALARM: i32 = 24;
    /// SYS_TIMES, the kernel call that reports the clock.
    const SYS_TIMES: i32 = 25;

    // SYS_SETALARM request: exp_time u64 @8, abs_time i32 @24. Bytes 0..8 hold
    // the call number/source, which the kernel dispatch overwrites.
    const SETALARM_EXP_TIME_OFF: usize = 8;
    const SETALARM_ABS_TIME_OFF: usize = 24;

    // SYS_TIMES reply: monotonic ticks u64 @8, system_hz u64 @40
    // (`kernel/src/system.rs` do_times_handler).
    const TIMES_MONOTONIC_OFF: usize = 8;
    const TIMES_HZ_OFF: usize = 40;

    /// Tick rate, cached on first use. It never changes at run time.
    static HZ: AtomicU64 = AtomicU64::new(0);

    /// Monotonic ticks since boot.
    pub fn now() -> u64 {
        let mut msg = [0u8; 64];
        let _ = minix_rt::kernel_call(SYS_TIMES, &mut msg);
        u64::from_ne_bytes(
            msg[TIMES_MONOTONIC_OFF..TIMES_MONOTONIC_OFF + 8]
                .try_into()
                .unwrap_or([0; 8]),
        )
    }

    /// Ticks per second.
    pub fn hz() -> u64 {
        let cached = HZ.load(Ordering::Relaxed);
        if cached != 0 {
            return cached;
        }
        let mut msg = [0u8; 64];
        let _ = minix_rt::kernel_call(SYS_TIMES, &mut msg);
        let hz = u64::from_ne_bytes(
            msg[TIMES_HZ_OFF..TIMES_HZ_OFF + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );
        // A missing hz field would divide by zero everywhere below; 60 is the
        // port's default (`kernel/src/glo.rs`).
        let hz = if hz == 0 { 60 } else { hz };
        HZ.store(hz, Ordering::Relaxed);
        hz
    }

    /// Arm the process alarm for an absolute tick, or cancel it (`0`).
    pub fn set(deadline: u64) {
        let mut msg = [0u8; 64];
        msg[SETALARM_EXP_TIME_OFF..SETALARM_EXP_TIME_OFF + 8]
            .copy_from_slice(&deadline.to_ne_bytes());
        msg[SETALARM_ABS_TIME_OFF..SETALARM_ABS_TIME_OFF + 4].copy_from_slice(&1i32.to_ne_bytes()); // absolute
        let _ = minix_rt::kernel_call(SYS_SETALARM, &mut msg);
    }

    /// The kernel clock in one reading: (monotonic ticks, realtime ticks,
    /// boottime seconds). Used to place a `CLOCK_REALTIME` deadline on the
    /// monotonic timeline this module arms. `do_times_handler` writes realtime at
    /// offset 0, monotonic at 8, boottime at 16.
    pub fn times_raw() -> (u64, u64, i64) {
        let mut msg = [0u8; 64];
        let _ = minix_rt::kernel_call(SYS_TIMES, &mut msg);
        let real = u64::from_ne_bytes(msg[0..8].try_into().unwrap_or([0; 8]));
        let mono = u64::from_ne_bytes(
            msg[TIMES_MONOTONIC_OFF..TIMES_MONOTONIC_OFF + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );
        let boot = i64::from_ne_bytes(msg[16..24].try_into().unwrap_or([0; 8]));
        (mono, real, boot)
    }
}

#[cfg(not(target_os = "minix"))]
mod imp {
    /// No kernel clock on the host; the host tests never observe a real time.
    pub fn now() -> u64 {
        0
    }
    pub fn hz() -> u64 {
        60
    }
    pub fn set(_deadline: u64) {}
    pub fn times_raw() -> (u64, u64, i64) {
        (0, 0, 0)
    }
}

pub use imp::{hz, now, set, times_raw};

/// `CLOCK_REALTIME` (matches `minix-std`'s `time::CLOCK_REALTIME`).
pub const CLOCK_REALTIME: i32 = 0;

/// Ticks to wait for a timeout of `ms` milliseconds, rounding up so a sub-tick
/// timeout still waits one tick rather than returning immediately.
pub fn ms_to_ticks(ms: i32) -> u64 {
    if ms <= 0 {
        return 0;
    }
    (ms as u64 * hz()).div_ceil(1000)
}

/// The absolute deadline `ms` from now, or 0 for "no deadline".
pub fn deadline_ms(ms: i32) -> u64 {
    let ticks = ms_to_ticks(ms);
    if ticks == 0 { 0 } else { now() + ticks }
}

/// The absolute deadline `sec` seconds and `usec` microseconds from now, or 0
/// when both are zero (which the caller treats as a poll, not a timeout).
pub fn deadline_timeval(sec: i32, usec: i32) -> u64 {
    let hz = hz();
    let ticks = (sec.max(0) as u64) * hz + ((usec.max(0) as u64) * hz).div_ceil(1_000_000);
    if ticks == 0 { 0 } else { now() + ticks }
}

/// Ticks for a `timespec`-shaped value, rounding up so a nonzero value waits at
/// least one tick; 0 only for a value of zero.
pub fn timespec_to_ticks(sec: i64, nsec: i64) -> u64 {
    if sec <= 0 && nsec <= 0 {
        return 0;
    }
    let hz = hz();
    let t = (sec.max(0) as u64) * hz + ((nsec.max(0) as u64) * hz).div_ceil(1_000_000_000);
    t.max(1)
}

/// A tick count as (seconds, nanoseconds).
pub fn ticks_to_timespec(ticks: u64) -> (i64, i64) {
    let hz = hz();
    (
        (ticks / hz) as i64,
        ((ticks % hz) * 1_000_000_000 / hz) as i64,
    )
}

/// The absolute monotonic tick a `timerfd` value names, or 0 to disarm.
///
/// A zero `value` disarms (the caller handles that). Otherwise a relative value
/// is `now + value`; an absolute `CLOCK_MONOTONIC` value is already in ticks; an
/// absolute `CLOCK_REALTIME` value is placed on the monotonic timeline through
/// the realtime/boottime relation. A time already in the past is clamped so the
/// timer fires at once rather than never.
pub fn deadline_at(clock_id: i32, absolute: bool, sec: i64, nsec: i64) -> u64 {
    let ticks = timespec_to_ticks(sec, nsec);
    if ticks == 0 {
        return 0;
    }
    if !absolute {
        return now() + ticks;
    }
    if clock_id != CLOCK_REALTIME {
        // CLOCK_MONOTONIC (or any other clock): the value is already absolute.
        return ticks;
    }
    let (mono, real, boot) = times_raw();
    let hz = hz();
    // The target wall time as ticks since boot...
    let target =
        ((sec - boot).max(0) as u64) * hz + ((nsec.max(0) as u64) * hz).div_ceil(1_000_000_000);
    // ...then its offset from the current realtime reading.
    if target > real {
        mono + (target - real)
    } else {
        mono.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_to_ticks_rounds_up_and_zeroes_nonpositive() {
        assert_eq!(ms_to_ticks(0), 0);
        assert_eq!(ms_to_ticks(-1), 0);
        // Any positive timeout waits at least one tick.
        assert!(ms_to_ticks(1) >= 1);
        // A full second is exactly hz ticks.
        assert_eq!(ms_to_ticks(1000), hz());
    }

    #[test]
    fn deadline_timeval_is_zero_only_when_timeout_is_zero() {
        assert_eq!(deadline_timeval(0, 0), 0);
        assert!(deadline_timeval(0, 1) != 0);
        assert!(deadline_timeval(1, 0) != 0);
    }
}
