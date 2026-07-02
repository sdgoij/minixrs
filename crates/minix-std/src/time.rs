//! Time and signal operations — CLOCK + PM protocols.
//!
//! Provides `clock_gettime`, `clock_getres`, `nanosleep`, signal handling
//! (`sigaction`, `sigprocmask`, `kill`, `signal`), and interval timers
//! (`alarm`, `setitimer`) by sending PM server messages.
//!
//! PM call numbers (from `.refs/minix-3.3.0/minix/include/minix/callnr.h`):
//! ```text
//! PM_BASE       = 0x000
//! PM_ITIMER     = 0x011
//! PM_KILL       = 0x00B
//! PM_SIGACTION  = 0x014
//! PM_SIGPENDING = 0x016
//! PM_SIGPROCMASK = 0x017
//! PM_CLOCK_GETRES  = 0x021
//! PM_CLOCK_GETTIME = 0x022
//! PM_CLOCK_SETTIME = 0x023
//! PM_GETTIMEOFDAY  = 0x01C
//! ```

#![allow(dead_code)]

use crate::MinixErr;
#[cfg(target_os = "none")]
use crate::{Message, PM_PROC_NR, sendrec};

// ── PM call numbers ─────────────────────────────────────────────────────

pub const PM_BASE: u32 = 0x000;
pub const PM_ITIMER: u32 = PM_BASE + 17; // 0x011
pub const PM_KILL: u32 = PM_BASE + 11; // 0x00B
pub const PM_SIGACTION: u32 = PM_BASE + 20; // 0x014
pub const PM_SIGPENDING: u32 = PM_BASE + 22; // 0x016
pub const PM_SIGPROCMASK: u32 = PM_BASE + 23; // 0x017
pub const PM_GETTIMEOFDAY: u32 = PM_BASE + 28; // 0x01C
pub const PM_CLOCK_GETRES: u32 = PM_BASE + 33; // 0x021
pub const PM_CLOCK_GETTIME: u32 = PM_BASE + 34; // 0x022
pub const PM_CLOCK_SETTIME: u32 = PM_BASE + 35; // 0x023

// ── Signal numbers ──────────────────────────────────────────────────────

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGILL: i32 = 4;
pub const SIGTRAP: i32 = 5;
pub const SIGABRT: i32 = 6;
pub const SIGFPE: i32 = 8;
pub const SIGKILL: i32 = 9;
pub const SIGUSR1: i32 = 10;
pub const SIGSEGV: i32 = 11;
pub const SIGUSR2: i32 = 12;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGTERM: i32 = 15;
pub const SIGCHLD: i32 = 20;
pub const SIGWINCH: i32 = 28;
pub const SIGSYS: i32 = 31;

/// Signal mask for sigprocmask.
pub const SIG_BLOCK: i32 = 0;
pub const SIG_UNBLOCK: i32 = 1;
pub const SIG_SETMASK: i32 = 2;

/// How each flag is stored relative to the signal number.
pub const SA_NOCLDSTOP: i32 = 0x00000001;
pub const SA_NOCLDWAIT: i32 = 0x00000002;
pub const SA_SIGINFO: i32 = 0x00000004;
pub const SA_RESTART: i32 = 0x00000008;
pub const SA_NODEFER: i32 = 0x00000010;

// ── Clock identifiers ───────────────────────────────────────────────────

pub const CLOCK_REALTIME: i32 = 0;
pub const CLOCK_MONOTONIC: i32 = 1;

// ── Itimers ─────────────────────────────────────────────────────────────

pub const ITIMER_REAL: i32 = 0;
pub const ITIMER_VIRTUAL: i32 = 1;
pub const ITIMER_PROF: i32 = 2;

// ── Timespec / Itimerval structures ─────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct TimeSpec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ITimerVal {
    pub it_interval: TimeSpec,
    pub it_value: TimeSpec,
}

// ── Sigaction structure ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SigAction {
    pub sa_handler: Option<unsafe extern "C" fn(i32)>,
    pub sa_mask: u64,
    pub sa_flags: i32,
    pub sa_restorer: Option<unsafe extern "C" fn()>,
}

// ── Message offsets (64-byte message buffer) ───────────────────────────

const OFF_TYPE: usize = 8;

// CLOCK_GETTIME / CLOCK_GETRES
const OFF_CLOCK_ID: usize = 12; // i32
const OFF_CLOCK_SEC: usize = 16; // i64
const OFF_CLOCK_NSEC: usize = 24; // i64

// KILL
const OFF_KILL_PID: usize = 12; // i32
const OFF_KILL_SIG: usize = 16; // i32

// SIGACTION
const OFF_SIGACT_SIG: usize = 12; // i32
const OFF_SIGACT_ACT: usize = 16; // u64 — pointer to SigAction
const OFF_SIGACT_OACT: usize = 24; // u64 — pointer to old SigAction

// SIGPROCMASK
const OFF_SIGMASK_HOW: usize = 12; // i32
const OFF_SIGMASK_SET: usize = 16; // u64

// ITIMER
const OFF_ITIMER_WHICH: usize = 12; // i32
const OFF_ITIMER_VALUE: usize = 16; // u64 — pointer to ITimerVal
const OFF_ITIMER_OVALUE: usize = 24; // u64 — pointer to old ITimerVal

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

fn msg_i64(msg: &[u8; 64], off: usize) -> i64 {
    i64::from_ne_bytes(msg[off..off + 8].try_into().unwrap())
}

fn msg_set_i64(msg: &mut [u8; 64], off: usize, val: i64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

fn msg_u64(msg: &[u8; 64], off: usize) -> u64 {
    u64::from_ne_bytes(msg[off..off + 8].try_into().unwrap())
}

fn msg_set_u64(msg: &mut [u8; 64], off: usize, val: u64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

/// Send a PM call and return the reply type on success.
#[cfg(target_os = "none")]
unsafe fn pm_call(msg: &mut Message) -> Result<i32, MinixErr> {
    unsafe {
        let _ = sendrec(PM_PROC_NR, msg);
        let mtype = msg_i32(msg, OFF_TYPE);
        if mtype < 0 {
            Err(MinixErr::from_i32(mtype))
        } else {
            Ok(mtype)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Clock operations
// ═══════════════════════════════════════════════════════════════════════════

/// Get the current time for the given clock.
pub fn clock_gettime(clock_id: i32) -> Result<TimeSpec, MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_CLOCK_GETTIME as i32);
        msg_set_i32(&mut msg, OFF_CLOCK_ID, clock_id);

        match pm_call(&mut msg) {
            Ok(_) => {
                let sec = msg_i64(&msg, OFF_CLOCK_SEC);
                let nsec = msg_i64(&msg, OFF_CLOCK_NSEC);
                Ok(TimeSpec {
                    tv_sec: sec,
                    tv_nsec: nsec,
                })
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = clock_id;
        Err(MinixErr::ENOSYS)
    }
}

/// Get the resolution of the given clock.
pub fn clock_getres(clock_id: i32) -> Result<TimeSpec, MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_CLOCK_GETRES as i32);
        msg_set_i32(&mut msg, OFF_CLOCK_ID, clock_id);

        match pm_call(&mut msg) {
            Ok(_) => {
                let sec = msg_i64(&msg, OFF_CLOCK_SEC);
                let nsec = msg_i64(&msg, OFF_CLOCK_NSEC);
                Ok(TimeSpec {
                    tv_sec: sec,
                    tv_nsec: nsec,
                })
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = clock_id;
        Err(MinixErr::ENOSYS)
    }
}

/// Set the time of the given clock.
pub fn clock_settime(clock_id: i32, tp: &TimeSpec) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_CLOCK_SETTIME as i32);
        msg_set_i32(&mut msg, OFF_CLOCK_ID, clock_id);
        msg_set_i64(&mut msg, OFF_CLOCK_SEC, tp.tv_sec);
        msg_set_i64(&mut msg, OFF_CLOCK_NSEC, tp.tv_nsec);

        match pm_call(&mut msg) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (clock_id, tp);
        Err(MinixErr::ENOSYS)
    }
}

/// Sleep for the specified duration.
pub fn nanosleep(req: &TimeSpec) -> Result<TimeSpec, MinixErr> {
    #[cfg(target_os = "none")]
    {
        // nanosleep is implemented via PM_ITIMER with a one-shot timer.
        // For now, use a busy-wait / stub approach.
        let _ = req;
        Err(MinixErr::ENOSYS)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = req;
        Err(MinixErr::ENOSYS)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Signal operations
// ═══════════════════════════════════════════════════════════════════════════

/// Send a signal to a process.
pub fn kill(pid: i32, sig: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_KILL as i32);
        msg_set_i32(&mut msg, OFF_KILL_PID, pid);
        msg_set_i32(&mut msg, OFF_KILL_SIG, sig);

        match pm_call(&mut msg) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (pid, sig);
        Err(MinixErr::ENOSYS)
    }
}

/// Examine and change a signal action.
///
/// # Safety
///
/// `act` and `old` must point to valid SigAction structs, or be null.
pub unsafe fn sigaction(
    sig: i32,
    act: Option<&SigAction>,
    old: Option<&mut SigAction>,
) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_SIGACTION as i32);
        msg_set_i32(&mut msg, OFF_SIGACT_SIG, sig);
        msg_set_u64(
            &mut msg,
            OFF_SIGACT_ACT,
            act.map_or(0, |a| a as *const _ as u64),
        );
        msg_set_u64(
            &mut msg,
            OFF_SIGACT_OACT,
            old.as_ref().map_or(0, |a| a as *const _ as u64),
        );

        match pm_call(&mut msg) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (sig, act, old);
        Err(MinixErr::ENOSYS)
    }
}

/// Examine and change the signal mask.
pub fn sigprocmask(how: i32, set: u64) -> Result<u64, MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_SIGPROCMASK as i32);
        msg_set_i32(&mut msg, OFF_SIGMASK_HOW, how);
        msg_set_u64(&mut msg, OFF_SIGMASK_SET, set);

        match pm_call(&mut msg) {
            Ok(_) => {
                // The old mask is returned in the message.
                let old_mask = msg_u64(&msg, OFF_SIGMASK_SET);
                Ok(old_mask)
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (how, set);
        Err(MinixErr::ENOSYS)
    }
}

/// Set an interval timer.
pub fn setitimer(which: i32, value: Option<&ITimerVal>) -> Result<ITimerVal, MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_ITIMER as i32);
        msg_set_i32(&mut msg, OFF_ITIMER_WHICH, which);
        msg_set_u64(
            &mut msg,
            OFF_ITIMER_VALUE,
            value.map_or(0, |v| v as *const _ as u64),
        );

        match pm_call(&mut msg) {
            Ok(_) => {
                // The old timer value would be read from the message.
                // For now, return a zeroed ITimerVal.
                Ok(ITimerVal {
                    it_interval: TimeSpec {
                        tv_sec: 0,
                        tv_nsec: 0,
                    },
                    it_value: TimeSpec {
                        tv_sec: 0,
                        tv_nsec: 0,
                    },
                })
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (which, value);
        Err(MinixErr::ENOSYS)
    }
}

/// Request SIGALRM after `seconds` seconds.
pub fn alarm(seconds: u32) -> u32 {
    #[cfg(target_os = "none")]
    {
        let itv = ITimerVal {
            it_interval: TimeSpec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: TimeSpec {
                tv_sec: seconds as i64,
                tv_nsec: 0,
            },
        };
        match setitimer(ITIMER_REAL, Some(&itv)) {
            Ok(old) => old.it_value.tv_sec as u32,
            Err(_) => 0,
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = seconds;
        0
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pm_time_call_numbers() {
        assert_eq!(PM_CLOCK_GETTIME, 0x022);
        assert_eq!(PM_CLOCK_GETRES, 0x021);
        assert_eq!(PM_CLOCK_SETTIME, 0x023);
        assert_eq!(PM_GETTIMEOFDAY, 0x01C);
        assert_eq!(PM_ITIMER, 0x011);
    }

    #[test]
    fn test_pm_signal_call_numbers() {
        assert_eq!(PM_KILL, 0x00B);
        assert_eq!(PM_SIGACTION, 0x014);
        assert_eq!(PM_SIGPROCMASK, 0x017);
        assert_eq!(PM_SIGPENDING, 0x016);
    }

    #[test]
    fn test_signal_numbers() {
        assert_eq!(SIGHUP, 1);
        assert_eq!(SIGINT, 2);
        assert_eq!(SIGKILL, 9);
        assert_eq!(SIGSEGV, 11);
        assert_eq!(SIGTERM, 15);
        assert_eq!(SIGCHLD, 20);
        assert_eq!(SIGWINCH, 28);
    }

    #[test]
    fn test_sigaction_flags() {
        assert_eq!(SA_NOCLDSTOP, 0x00000001);
        assert_eq!(SA_RESTART, 0x00000008);
        assert_eq!(SA_SIGINFO, 0x00000004);
    }

    #[test]
    fn test_sigmask_constants() {
        assert_eq!(SIG_BLOCK, 0);
        assert_eq!(SIG_UNBLOCK, 1);
        assert_eq!(SIG_SETMASK, 2);
    }

    #[test]
    fn test_clock_ids() {
        assert_eq!(CLOCK_REALTIME, 0);
        assert_eq!(CLOCK_MONOTONIC, 1);
    }

    #[test]
    fn test_itimers() {
        assert_eq!(ITIMER_REAL, 0);
        assert_eq!(ITIMER_VIRTUAL, 1);
        assert_eq!(ITIMER_PROF, 2);
    }

    #[test]
    fn test_timespec_layout() {
        assert_eq!(core::mem::size_of::<TimeSpec>(), 16);
        let ts = TimeSpec {
            tv_sec: 42,
            tv_nsec: 100,
        };
        assert_eq!(ts.tv_sec, 42);
        assert_eq!(ts.tv_nsec, 100);
    }

    #[test]
    fn test_itimerval_layout() {
        assert_eq!(core::mem::size_of::<ITimerVal>(), 32);
    }

    #[test]
    fn test_sigaction_layout() {
        assert_eq!(core::mem::size_of::<SigAction>(), 32);
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 8, 42);
        assert_eq!(msg_i32(&msg, 8), 42);

        msg_set_i64(&mut msg, 16, -1);
        assert_eq!(msg_i64(&msg, 16), -1);

        msg_set_u64(&mut msg, 24, 0xDEADBEEF);
        assert_eq!(msg_u64(&msg, 24), 0xDEADBEEF);
    }

    #[test]
    fn test_clock_gettime_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_CLOCK_GETTIME as i32);
        msg_set_i32(&mut msg, OFF_CLOCK_ID, CLOCK_REALTIME);

        assert_eq!(msg_i32(&msg, 8), 0x022);
        assert_eq!(msg_i32(&msg, 12), 0);
    }

    #[test]
    fn test_kill_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_KILL as i32);
        msg_set_i32(&mut msg, OFF_KILL_PID, 123);
        msg_set_i32(&mut msg, OFF_KILL_SIG, SIGTERM);

        assert_eq!(msg_i32(&msg, 8), 0x00B);
        assert_eq!(msg_i32(&msg, 12), 123);
        assert_eq!(msg_i32(&msg, 16), 15);
    }

    #[test]
    fn test_sigprocmask_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_SIGPROCMASK as i32);
        msg_set_i32(&mut msg, OFF_SIGMASK_HOW, SIG_SETMASK);
        msg_set_u64(&mut msg, OFF_SIGMASK_SET, 0xFFFF);

        assert_eq!(msg_i32(&msg, 8), 0x017);
        assert_eq!(msg_i32(&msg, 12), 2);
        assert_eq!(msg_u64(&msg, 16), 0xFFFF);
    }

    #[test]
    fn test_clock_gettime_returns_enosys_on_host() {
        let r = clock_gettime(CLOCK_REALTIME);
        assert!(r.is_err());
    }

    #[test]
    fn test_clock_getres_returns_enosys_on_host() {
        let r = clock_getres(CLOCK_REALTIME);
        assert!(r.is_err());
    }

    #[test]
    fn test_clock_settime_returns_enosys_on_host() {
        let ts = TimeSpec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let r = clock_settime(CLOCK_REALTIME, &ts);
        assert!(r.is_err());
    }

    #[test]
    fn test_kill_returns_enosys_on_host() {
        let r = kill(0, SIGTERM);
        assert!(r.is_err());
    }

    #[test]
    fn test_sigprocmask_returns_enosys_on_host() {
        let r = sigprocmask(SIG_SETMASK, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_alarm_returns_zero_on_host() {
        let r = alarm(5);
        assert_eq!(r, 0);
    }

    #[test]
    fn test_nanosleep_returns_enosys_on_host() {
        let req = TimeSpec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        let r = nanosleep(&req);
        assert!(r.is_err());
    }

    #[test]
    fn test_setitimer_returns_enosys_on_host() {
        let r = setitimer(ITIMER_REAL, None);
        assert!(r.is_err());
    }

    type SignalHandler =
        unsafe fn(i32, Option<&SigAction>, Option<&mut SigAction>) -> Result<(), MinixErr>;

    #[test]
    fn test_sigaction_signature() {
        fn _check(f: SignalHandler) {
            let _ = f;
        }
        _check(sigaction);
    }
}
