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
const _MSG_OFF_TYPE: usize = 0;
const _MSG_OFF_SOURCE: usize = 4;
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
/// Supports CLOCK_GETTIME, CLOCK_SETTIME, and CLOCK_GETRES.
///
/// The IPC receive call is stubbed — real IPC comes in Phase 13.
pub fn clock_server_main() {
    // TODO: Phase 13 — replace with real sef_receive + ipc_send loop:
    //
    //   loop {
    //       let mut msg = [0u8; 64];
    //       let r = sef_receive(ANY, &mut msg, &mut ipc_status);
    //       if r != OK { continue; }
    //       let call_nr = msg_i32(&msg, MSG_OFF_TYPE);
    //       let result = dispatch_clock(call_nr, &mut msg);
    //       msg_set_i32(&mut msg, MSG_OFF_TYPE, result);
    //       ipc_send(msg_i32(&msg, MSG_OFF_SOURCE), &mut msg);
    //   }
}

/// Dispatch a single clock request.
///
/// Returns the result code and modifies `msg` with response data.
pub fn dispatch_clock(call_nr: i32, msg: &mut [u8; 64]) -> i32 {
    match call_nr as u32 {
        CLOCK_GETTIME | CLOCK_GETRES => {
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
            // Would set the clock — stub for now.
            OK
        }
        _ => EINVAL,
    }
}

/// Read an i32 from a message buffer.
fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
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
        // Stub must not panic
        clock_server_main();
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
    fn test_dispatch_settime_returns_ok() {
        let mut msg = [0u8; 64];
        let r = dispatch_clock(CLOCK_SETTIME as i32, &mut msg);
        assert_eq!(r, OK);
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
