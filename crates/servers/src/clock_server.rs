//! Clock server types and infrastructure.
//!
//! Provides userspace-facing clock types (mirroring POSIX `struct timespec`)
//! and clock resolution queries. A full IPC server loop is deferred until
//! the scheduler and PM are running (Phase 12+).

/// Clock RQ base (0xE00), matching com.h conventions.
pub const CLOCK_RQ_BASE: u32 = 0xE00;

const NSEC_PER_SEC: i64 = 1_000_000_000;

/// Message type: get clock time.
pub const CLOCK_GETTIME: u32 = CLOCK_RQ_BASE;
/// Message type: set clock time.
pub const CLOCK_SETTIME: u32 = CLOCK_RQ_BASE + 1;
/// Message type: get clock resolution.
pub const CLOCK_GETRES: u32 = CLOCK_RQ_BASE + 2;

/// Message offsets for clock requests (64-byte message buffer).
/// The standard MINIX message layout is source@0, type@4, payload@8.
#[cfg_attr(not(target_os = "minix"), allow(dead_code))]
const MSG_OFF_TYPE: usize = 4;
pub const MSG_OFF_CLOCK_ID: usize = 8; // i32 — ClockId
pub const MSG_OFF_SEC: usize = 12; // i64 — tv_sec
pub const MSG_OFF_NSEC: usize = 20; // i64 — tv_nsec

const OK: i32 = 0;
const EINVAL: i32 = -22;

/// Clock time specification with seconds and nanoseconds.
/// Mirrors POSIX `struct timespec` for userspace compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ClockTimeSpec {
    pub tv_sec: i64,  // seconds
    pub tv_nsec: i64, // nanoseconds
}

impl ClockTimeSpec {
    /// Convert from kernel ticks to a `ClockTimeSpec`.
    ///
    /// `hz` is the number of ticks per second (the kernel's tick rate).
    ///
    /// # Panics
    ///
    /// Panics if `hz` is zero.
    pub fn from_ticks(ticks: u64, hz: u64) -> Self {
        assert!(hz > 0, "hz must be non-zero");
        let total_ns = ticks.saturating_mul(NSEC_PER_SEC as u64 / hz);
        Self {
            tv_sec: (total_ns / NSEC_PER_SEC as u64) as i64,
            tv_nsec: (total_ns % NSEC_PER_SEC as u64) as i64,
        }
    }

    /// Convert this `ClockTimeSpec` to kernel ticks.
    ///
    /// `hz` is the number of ticks per second.
    ///
    /// # Panics
    ///
    /// Panics if `hz` is zero.
    pub fn as_ticks(&self, hz: u64) -> u64 {
        assert!(hz > 0, "hz must be non-zero");
        let total_ns = self.tv_sec.saturating_mul(NSEC_PER_SEC) + self.tv_nsec;
        if total_ns <= 0 {
            return 0;
        }
        let ns_per_tick = NSEC_PER_SEC as u64 / hz;
        (total_ns as u64).div_ceil(ns_per_tick)
    }

    /// Returns `true` if both seconds and nanoseconds are zero.
    pub fn is_zero(&self) -> bool {
        self.tv_sec == 0 && self.tv_nsec == 0
    }
}

impl core::ops::Add for ClockTimeSpec {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        let mut sec = self.tv_sec + rhs.tv_sec;
        let mut nsec = self.tv_nsec + rhs.tv_nsec;
        if nsec >= NSEC_PER_SEC {
            nsec -= NSEC_PER_SEC;
            sec += 1;
        }
        Self {
            tv_sec: sec,
            tv_nsec: nsec,
        }
    }
}

impl core::ops::Sub for ClockTimeSpec {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        let mut sec = self.tv_sec - rhs.tv_sec;
        let mut nsec = self.tv_nsec - rhs.tv_nsec;
        if nsec < 0 {
            // Borrow from seconds.
            nsec += NSEC_PER_SEC;
            sec -= 1;
        }
        if sec < 0 {
            // Clamp to zero rather than underflowing — callers that need
            // negative durations should handle sign explicitly.
            Self {
                tv_sec: 0,
                tv_nsec: 0,
            }
        } else {
            Self {
                tv_sec: sec,
                tv_nsec: nsec,
            }
        }
    }
}

/// Identifies which clock to query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ClockId {
    Realtime = 0,
    Monotonic = 1,
}

/// Return the resolution of the given clock in nanoseconds.
pub fn clock_getres(_clock_id: ClockId) -> ClockTimeSpec {
    // Kernel tick rate determines resolution.
    // Default: 100 Hz = 10 ms = 10,000,000 ns
    ClockTimeSpec {
        tv_sec: 0,
        tv_nsec: 10_000_000,
    }
}

/// Clock server main loop.
///
/// Receives messages from clients and dispatches clock requests.
/// Supports CLOCK_GETTIME, CLOCK_SETTIME, and CLOCK_GETRES. Replies carry
/// the result code in `m_type` (offset 4); GETTIME/GETRES responses travel
/// in the `sec`/`nsec` payload fields.
pub fn clock_server_main() {
    #[cfg(target_os = "minix")]
    {
        const ANY: i32 = 0x0000ffff;

        loop {
            let mut msg = [0u8; 64];
            let src = unsafe {
                minix_rt::syscall2(minix_rt::RECEIVE_CALL, ANY as u64, msg.as_mut_ptr() as u64)
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;
            let call_nr = msg_i32(&msg, MSG_OFF_TYPE);
            let result = dispatch_clock(call_nr, &mut msg);
            msg_set_i32(&mut msg, MSG_OFF_TYPE, result);
            unsafe {
                minix_rt::syscall2(
                    minix_rt::SENDNB_CALL,
                    src_ep as u64,
                    msg.as_mut_ptr() as u64,
                );
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // No kernel IPC on host builds — dispatch is tested directly.
    }
}

/// Dispatch a single clock request.
///
/// Returns the result code and modifies `msg` with response data.
pub fn dispatch_clock(call_nr: i32, msg: &mut [u8; 64]) -> i32 {
    match call_nr as u32 {
        CLOCK_GETTIME => {
            let clock_id = msg_i32(msg, MSG_OFF_CLOCK_ID);
            let (realtime, monotonic, boottime, hz) = kernel_clock();
            let ts = match clock_time_to_ts(clock_id, realtime, monotonic, boottime, hz) {
                Ok(ts) => ts,
                Err(e) => return e,
            };
            msg_set_i64(msg, MSG_OFF_SEC, ts.tv_sec);
            msg_set_i64(msg, MSG_OFF_NSEC, ts.tv_nsec);
            OK
        }
        CLOCK_GETRES => {
            let clock_id = msg_i32(msg, MSG_OFF_CLOCK_ID);
            let clock = match clock_id {
                0 => ClockId::Realtime,
                1 => ClockId::Monotonic,
                _ => return EINVAL,
            };
            let ts = clock_getres(clock);
            msg_set_i64(msg, MSG_OFF_SEC, ts.tv_sec);
            msg_set_i64(msg, MSG_OFF_NSEC, ts.tv_nsec);
            OK
        }
        CLOCK_SETTIME => {
            // Set the kernel realtime clock. Only CLOCK_REALTIME is
            // settable; the kernel rejects other clock ids.
            let sec = msg_i64(msg, MSG_OFF_SEC);
            let nsec = msg_i64(msg, MSG_OFF_NSEC);
            let r = kernel_settime(sec, nsec, true);
            if r != 0 {
                return r;
            }
            OK
        }
        _ => EINVAL,
    }
}

/// Build the SYS_SETTIME kernel-call message (kernel call 40).
///
/// Layout matches the kernel's `do_settime_handler`: sec @ 8, nsec @ 16,
/// now @ 24, clock_id @ 28.
fn build_settime_msg(sec: i64, nsec: i64, now: bool) -> [u8; 64] {
    let mut msg = [0u8; 64];
    msg[8..16].copy_from_slice(&sec.to_ne_bytes());
    msg[16..24].copy_from_slice(&nsec.to_ne_bytes());
    msg[24..28].copy_from_slice(&(now as i32).to_ne_bytes());
    msg[28..32].copy_from_slice(&0i32.to_ne_bytes()); // CLOCK_REALTIME
    msg
}

/// Set the kernel realtime clock via SYS_SETTIME (kernel call 40).
///
/// `now = true` sets the clock; `false` applies an adjtime delta. Returns
/// OK or a negative errno from the kernel handler.
fn kernel_settime(sec: i64, nsec: i64, now: bool) -> i32 {
    let mut msg = build_settime_msg(sec, nsec, now);
    minix_rt::kernel_call(40, &mut msg) // SYS_SETTIME
}

/// Read the kernel clock via SYS_TIMES (kernel call 25).
///
/// Returns `(realtime_ticks, monotonic_ticks, boottime_sec, hz)`. On host
/// (no kernel) returns all zeros with the default 100 Hz tick rate.
fn kernel_clock() -> (u64, u64, i64, u64) {
    let mut msg = [0u8; 64];
    let r = minix_rt::kernel_call(25, &mut msg); // SYS_TIMES
    if r != 0 {
        return (0, 0, 0, 100);
    }
    let realtime = u64::from_ne_bytes(msg[0..8].try_into().unwrap_or([0; 8]));
    let monotonic = u64::from_ne_bytes(msg[8..16].try_into().unwrap_or([0; 8]));
    let boottime = i64::from_ne_bytes(msg[16..24].try_into().unwrap_or([0; 8]));
    let hz = u64::from_ne_bytes(msg[40..48].try_into().unwrap_or([0; 8]));
    (realtime, monotonic, boottime, hz)
}

/// Convert raw kernel clock values to a `ClockTimeSpec` for `clock_id`.
///
/// Realtime wall time is `boottime + realtime/hz` (the kernel stores
/// realtime as ticks since boot, with boottime in seconds since the epoch).
fn clock_time_to_ts(
    clock_id: i32,
    realtime: u64,
    monotonic: u64,
    boottime: i64,
    hz: u64,
) -> Result<ClockTimeSpec, i32> {
    let hz = if hz == 0 { 100 } else { hz };
    match clock_id {
        0 => {
            let sec = boottime + (realtime / hz) as i64;
            let nsec = ((realtime % hz) * (NSEC_PER_SEC as u64 / hz)) as i64;
            Ok(ClockTimeSpec {
                tv_sec: sec,
                tv_nsec: nsec,
            })
        }
        1 => Ok(ClockTimeSpec::from_ticks(monotonic, hz)),
        _ => Err(EINVAL),
    }
}

/// Read an i32 from a message buffer.
fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

/// Read an i64 from a message buffer.
fn msg_i64(msg: &[u8; 64], off: usize) -> i64 {
    i64::from_ne_bytes(msg[off..off + 8].try_into().unwrap())
}

/// Write an i32 into a message buffer.
#[cfg_attr(not(target_os = "minix"), allow(dead_code))]
fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

/// Write an i64 into a message buffer.
fn msg_set_i64(msg: &mut [u8; 64], off: usize, val: i64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_timespec_ticks_conversion() {
        // Roundtrip: 100 ticks @ 100 Hz → 1 sec → back to 100 ticks
        let ts = ClockTimeSpec::from_ticks(100, 100);
        assert_eq!(ts.tv_sec, 1);
        assert_eq!(ts.tv_nsec, 0);
        assert_eq!(ts.as_ticks(100), 100);
    }

    #[test]
    fn test_timespec_from_ticks() {
        // 50 ticks @ 100 Hz = 0.5 sec = 500,000,000 ns
        let ts50 = ClockTimeSpec::from_ticks(50, 100);
        assert_eq!(ts50.tv_sec, 0);
        assert_eq!(ts50.tv_nsec, 500_000_000);

        // 150 ticks @ 100 Hz = 1.5 sec
        let ts150 = ClockTimeSpec::from_ticks(150, 100);
        assert_eq!(ts150.tv_sec, 1);
        assert_eq!(ts150.tv_nsec, 500_000_000);

        // 200 ticks @ 1000 Hz = 0.2 sec = 200,000,000 ns
        let ts200 = ClockTimeSpec::from_ticks(200, 1000);
        assert_eq!(ts200.tv_sec, 0);
        assert_eq!(ts200.tv_nsec, 200_000_000);
    }

    #[test]
    fn test_timespec_as_ticks() {
        // 1 sec @ 100 Hz → 100 ticks
        let ts = ClockTimeSpec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        assert_eq!(ts.as_ticks(100), 100);

        // 0.5 sec @ 100 Hz → 50 ticks
        let ts = ClockTimeSpec {
            tv_sec: 0,
            tv_nsec: 500_000_000,
        };
        assert_eq!(ts.as_ticks(100), 50);

        // 0 sec, 0 nsec → 0 ticks
        let ts = ClockTimeSpec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(ts.as_ticks(100), 0);
    }

    #[test]
    fn test_timespec_is_zero() {
        let zero = ClockTimeSpec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert!(zero.is_zero());

        let non_zero = ClockTimeSpec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        assert!(!non_zero.is_zero());

        let non_zero2 = ClockTimeSpec {
            tv_sec: 0,
            tv_nsec: 1,
        };
        assert!(!non_zero2.is_zero());
    }

    #[test]
    fn test_timespec_add_ns_overflow() {
        let a = ClockTimeSpec {
            tv_sec: 1,
            tv_nsec: 900_000_000,
        };
        let b = ClockTimeSpec {
            tv_sec: 0,
            tv_nsec: 200_000_000,
        };
        let sum = a + b;
        // 1,900,000,000 ns overflows: should be 2 sec, 100,000,000 ns
        assert_eq!(sum.tv_sec, 2);
        assert_eq!(sum.tv_nsec, 100_000_000);
    }

    #[test]
    fn test_timespec_sub_underflow() {
        let a = ClockTimeSpec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        let b = ClockTimeSpec {
            tv_sec: 0,
            tv_nsec: 500_000_000,
        };
        let diff = a - b;
        // Underflows to zero (no panic)
        assert_eq!(diff.tv_sec, 0);
        assert_eq!(diff.tv_nsec, 0);
    }

    #[test]
    fn test_timespec_sub_normal() {
        let a = ClockTimeSpec {
            tv_sec: 5,
            tv_nsec: 300_000_000,
        };
        let b = ClockTimeSpec {
            tv_sec: 2,
            tv_nsec: 100_000_000,
        };
        let diff = a - b;
        assert_eq!(diff.tv_sec, 3);
        assert_eq!(diff.tv_nsec, 200_000_000);
    }

    #[test]
    fn test_timespec_sub_borrow() {
        let a = ClockTimeSpec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        let b = ClockTimeSpec {
            tv_sec: 4,
            tv_nsec: 500_000_000,
        };
        let diff = a - b;
        // Borrow from seconds: 4 sec, 500,000,000 ns
        assert_eq!(diff.tv_sec, 0);
        assert_eq!(diff.tv_nsec, 500_000_000);
    }

    #[test]
    fn test_clock_id_values() {
        assert_eq!(ClockId::Realtime as i32, 0);
        assert_eq!(ClockId::Monotonic as i32, 1);
    }

    #[test]
    fn test_clock_getres_default() {
        let res = clock_getres(ClockId::Realtime);
        // Default resolution: 10 ms = 10,000,000 ns
        assert_eq!(res.tv_sec, 0);
        assert_eq!(res.tv_nsec, 10_000_000);

        let res2 = clock_getres(ClockId::Monotonic);
        assert_eq!(res2, res);
    }

    #[test]
    fn test_clock_server_main_callable() {
        // Must not panic or hang on host (the minix loop is cfg-gated).
        clock_server_main();
    }

    #[test]
    fn test_clock_time_to_ts_realtime() {
        // boottime=1000s, realtime=12345 ticks @ 100 Hz → 1123.45s.
        let ts = clock_time_to_ts(0, 12345, 0, 1000, 100).unwrap();
        assert_eq!(ts.tv_sec, 1123);
        assert_eq!(ts.tv_nsec, 450_000_000);
    }

    #[test]
    fn test_clock_time_to_ts_monotonic() {
        // 250 ticks @ 100 Hz → 2.5s.
        let ts = clock_time_to_ts(1, 999, 250, 5, 100).unwrap();
        assert_eq!(ts.tv_sec, 2);
        assert_eq!(ts.tv_nsec, 500_000_000);
    }

    #[test]
    fn test_clock_time_to_ts_invalid_clock() {
        assert_eq!(clock_time_to_ts(42, 0, 0, 0, 100), Err(EINVAL));
    }

    #[test]
    fn test_clock_time_to_ts_zero_hz_falls_back() {
        // A zero hz from the kernel must not panic; treat it as 100 Hz.
        let ts = clock_time_to_ts(1, 50, 50, 0, 0).unwrap();
        assert_eq!(ts.tv_sec, 0);
        assert_eq!(ts.tv_nsec, 500_000_000);
    }

    #[test]
    fn test_dispatch_getres_realtime() {
        let mut msg = [0u8; 64];
        unsafe {
            core::ptr::write_unaligned(
                msg.as_mut_ptr().add(8) as *mut i32,
                ClockId::Realtime as i32,
            )
        };
        let r = dispatch_clock(CLOCK_GETRES as i32, &mut msg);
        assert_eq!(r, OK);
        // Resolution should be 10ms = 10,000,000 ns
        let sec = unsafe { core::ptr::read_unaligned(msg.as_ptr().add(12) as *const i64) };
        let nsec = unsafe { core::ptr::read_unaligned(msg.as_ptr().add(20) as *const i64) };
        assert_eq!(sec, 0);
        assert_eq!(nsec, 10_000_000);
    }

    #[test]
    fn test_dispatch_getres_monotonic() {
        let mut msg = [0u8; 64];
        unsafe {
            core::ptr::write_unaligned(
                msg.as_mut_ptr().add(8) as *mut i32,
                ClockId::Monotonic as i32,
            )
        };
        let r = dispatch_clock(CLOCK_GETRES as i32, &mut msg);
        assert_eq!(r, OK);
    }

    #[test]
    fn test_dispatch_gettime_realtime() {
        let mut msg = [0u8; 64];
        unsafe {
            core::ptr::write_unaligned(
                msg.as_mut_ptr().add(8) as *mut i32,
                ClockId::Realtime as i32,
            )
        };
        let r = dispatch_clock(CLOCK_GETTIME as i32, &mut msg);
        assert_eq!(r, OK);
    }

    #[test]
    fn test_dispatch_invalid_clock_id() {
        let mut msg = [0u8; 64];
        unsafe { core::ptr::write_unaligned(msg.as_mut_ptr().add(8) as *mut i32, 99) };
        let r = dispatch_clock(CLOCK_GETRES as i32, &mut msg);
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_dispatch_unknown_call() {
        let mut msg = [0u8; 64];
        let r = dispatch_clock(0xFFFF, &mut msg);
        assert_eq!(r, EINVAL);
    }

    #[test]
    fn test_build_settime_msg_layout() {
        // Matches the kernel's do_settime_handler readers:
        // sec @ 8, nsec @ 16, now @ 24, clock_id @ 28.
        let msg = build_settime_msg(1234, 5678, true);
        assert_eq!(i64::from_ne_bytes(msg[8..16].try_into().unwrap()), 1234);
        assert_eq!(i64::from_ne_bytes(msg[16..24].try_into().unwrap()), 5678);
        assert_eq!(i32::from_ne_bytes(msg[24..28].try_into().unwrap()), 1); // now
        assert_eq!(i32::from_ne_bytes(msg[28..32].try_into().unwrap()), 0); // CLOCK_REALTIME

        let msg2 = build_settime_msg(0, 0, false);
        assert_eq!(i32::from_ne_bytes(msg2[24..28].try_into().unwrap()), 0); // adjtime
    }

    #[test]
    fn test_dispatch_settime_forwards_to_kernel() {
        // CLOCK_SETTIME reads sec/nsec from the request message and forwards
        // them to the kernel. On host there is no kernel, so the call fails
        // with a negative status instead of the stub's unconditional OK.
        let mut msg = [0u8; 64];
        msg[12..20].copy_from_slice(&1000i64.to_ne_bytes());
        msg[20..28].copy_from_slice(&0i64.to_ne_bytes());
        let r = dispatch_clock(CLOCK_SETTIME as i32, &mut msg);
        assert!(r != OK, "settime must reach the kernel, not be a no-op");
    }

    #[test]
    fn test_timespec_size() {
        // `i64` + `i64` = 16 bytes on all supported targets
        assert_eq!(size_of::<ClockTimeSpec>(), 16);
    }

    #[test]
    fn test_timespec_add_no_overflow() {
        let a = ClockTimeSpec {
            tv_sec: 3,
            tv_nsec: 400_000_000,
        };
        let b = ClockTimeSpec {
            tv_sec: 1,
            tv_nsec: 200_000_000,
        };
        let sum = a + b;
        assert_eq!(sum.tv_sec, 4);
        assert_eq!(sum.tv_nsec, 600_000_000);
    }
}
