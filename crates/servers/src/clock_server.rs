//! Clock server types and infrastructure.
//!
//! Provides userspace-facing clock types (mirroring POSIX `struct timespec`)
//! and clock resolution queries. A full IPC server loop is deferred until
//! the scheduler and PM are running (Phase 12+).

const NSEC_PER_SEC: i64 = 1_000_000_000;

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
/// Currently a stub — will be wired up when the IPC server infrastructure
/// is running (Phase 12+).
pub fn clock_server_main() {
    // TODO: Phase 12 — receive messages and dispatch clock requests
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
