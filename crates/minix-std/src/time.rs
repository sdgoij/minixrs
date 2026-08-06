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
//!
//! Note: the PM server implements `PM_GETTIMEOFDAY` (0x01C) but has no
//! handler for `PM_CLOCK_GETTIME` (0x022) — dispatch falls through to
//! `no_sys`. `clock_gettime` therefore routes through `PM_GETTIMEOFDAY`,

#![allow(dead_code)]

use crate::MinixErr;
#[cfg(target_os = "none")]
use crate::{Message, PM_PROC_NR, sendrec};

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

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGILL: i32 = 4;

/// Signal disposition values: SIG_DFL (0) / SIG_IGN (1), as the kernel
/// (`do_sigaction`) reads them from the raw handler field.
pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;
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

pub const CLOCK_REALTIME: i32 = 0;
pub const CLOCK_MONOTONIC: i32 = 1;

pub const ITIMER_REAL: i32 = 0;
pub const ITIMER_VIRTUAL: i32 = 1;
pub const ITIMER_PROF: i32 = 2;

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

const OFF_TYPE: usize = 4;

// The PM message header: the call number lives in m_type at bytes 4-8
// (payload starts at 8). The old OFF_TYPE = 8 wrote the call number into
// m1i1, so PM read garbage as call 0 and every wrapper misdispatched.

// CLOCK_SETTIME (PM_CLOCK_SETTIME) — mess_lc_pm_time layout (C ipc.h):
//   sec @ 8 (time_t i64), clk_id @ 16 (i32), now @ 20 (i32), nsec @ 24 (i64).
const OFF_CLOCK_SEC: usize = 8; // i64 (time_t)
const OFF_CLOCK_ID: usize = 16; // i32 (clockid_t)
const OFF_CLOCK_NOW: usize = 20; // i32 (1 = set, 0 = adjtime)
const OFF_CLOCK_NSEC: usize = 24; // i64 (long)

// GETTIMEOFDAY — the PM call that actually answers clock_gettime. Reply
// layout: m2l1 = tv_sec, m2l2 = tv_usec. The M2 member starts at payload
// offset 8; m2l1 lands at absolute 24, m2l2 at absolute 32 (same as the
// SIGACTION m2l1/m2l2/m2l3 offsets below).
const OFF_TIMEVAL_SEC: usize = 24; // i64 (m2l1)
const OFF_TIMEVAL_USEC: usize = 32; // i64 (m2l2)

// KILL (PM handle_kill: m1i1 = signo, m1i2 = pid)
const OFF_KILL_PID: usize = 12; // i32
const OFF_KILL_SIG: usize = 8; // i32

// SIGACTION (PM do_sigaction: m1i1 = signo, m2l1 = act ptr, m2l2 = oact ptr,
// m2l3 = sigreturn trampoline ptr)
const OFF_SIGACT_SIG: usize = 8; // i32
const OFF_SIGACT_ACT: usize = 24; // u64 — pointer to C-style sigaction
const OFF_SIGACT_OACT: usize = 32; // u64 — pointer to old sigaction
const OFF_SIGACT_RESTORER: usize = 40; // u64 — sigreturn trampoline address

// SIGPROCMASK (PM do_sigprocmask: m1i1 = how, m2l1 = set ptr, m2l2 = old ptr)
const OFF_SIGMASK_HOW: usize = 8; // i32
const OFF_SIGMASK_SET: usize = 24; // u64 — pointer to 16-byte mask
const OFF_SIGMASK_OLD: usize = 32; // u64 — pointer to old mask

// ITIMER
const OFF_ITIMER_WHICH: usize = 12; // i32
const OFF_ITIMER_VALUE: usize = 16; // u64 — pointer to ITimerVal
const OFF_ITIMER_OVALUE: usize = 24; // u64 — pointer to old ITimerVal

// Helpers

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

// Clock operations

/// Get the current time for the given clock.
///
/// The PM server has no `PM_CLOCK_GETTIME` (34) handler — dispatch falls
/// through to `no_sys` — so this routes through `PM_GETTIMEOFDAY` (28),
/// which answers with `m2l1 = tv_sec`, `m2l2 = tv_usec`. The microseconds
/// are converted to nanoseconds to fill `TimeSpec`.
pub fn clock_gettime(clock_id: i32) -> Result<TimeSpec, MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        // PM exposes a single realtime clock; the clock id is not honored.
        let _ = clock_id;
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_GETTIMEOFDAY as i32);

        match pm_call(&mut msg) {
            Ok(_) => {
                let sec = msg_i64(&msg, OFF_TIMEVAL_SEC);
                let usec = msg_i64(&msg, OFF_TIMEVAL_USEC);
                Ok(TimeSpec {
                    tv_sec: sec,
                    tv_nsec: usec * 1_000,
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
///
/// The PM server exposes a single microsecond-resolution clock (its
/// `PM_GETTIMEOFDAY` reply carries `tv_usec`), so the resolution is fixed
/// at 1 µs and no IPC is needed.
pub fn clock_getres(_clock_id: i32) -> Result<TimeSpec, MinixErr> {
    #[cfg(target_os = "none")]
    {
        Ok(TimeSpec {
            tv_sec: 0,
            tv_nsec: 1_000,
        })
    }
    #[cfg(not(target_os = "none"))]
    {
        Err(MinixErr::ENOSYS)
    }
}

/// Set the time of the given clock.
///
/// Only `CLOCK_REALTIME` is settable, and only by root — PM answers
/// `EPERM` otherwise. The request uses the C `mess_lc_pm_time` layout
/// with `now = 1` (set, not adjtime).
pub fn clock_settime(clock_id: i32, tp: &TimeSpec) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_CLOCK_SETTIME as i32);
        msg_set_i64(&mut msg, OFF_CLOCK_SEC, tp.tv_sec);
        msg_set_i32(&mut msg, OFF_CLOCK_ID, clock_id);
        msg_set_i32(&mut msg, OFF_CLOCK_NOW, 1); // now = 1: set the clock
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

// Signal operations

/// Encode a C-style 28-byte sigaction: handler u64 + mask 16 bytes + flags
/// i32. This is the exact layout PM's `do_sigaction` reads via sys_vircopy
/// (handler@0, mask@8, flags@24).
pub fn encode_action(handler: u64, mask: u128, flags: i32) -> [u8; 28] {
    let mut act = [0u8; 28];
    act[0..8].copy_from_slice(&handler.to_ne_bytes());
    act[8..24].copy_from_slice(&mask.to_ne_bytes());
    act[24..28].copy_from_slice(&flags.to_ne_bytes());
    act
}

/// Decode a C-style 28-byte sigaction into (handler, mask, flags).
///
/// Inverse of `encode_action`; used by the libc `sigaction` wrapper to
/// translate a C `struct sigaction` (handler u64@0, mask 16 bytes@8,
/// flags i32@24) into the minix-std raw-value form.
pub fn decode_action(act: &[u8; 28]) -> (u64, u128, i32) {
    let handler = u64::from_ne_bytes(act[0..8].try_into().unwrap());
    let mask = u128::from_ne_bytes(act[8..24].try_into().unwrap());
    let flags = i32::from_ne_bytes(act[24..28].try_into().unwrap());
    (handler, mask, flags)
}

/// Build a PM_SIGACTION request (m_type@4, signo@8, act@24, oact@32,
/// sigreturn trampoline@40).
pub fn build_sigaction_msg(
    signo: i32,
    act_ptr: u64,
    oact_ptr: u64,
    restorer: u64,
    msg: &mut [u8; 64],
) {
    msg_set_i32(msg, OFF_TYPE, PM_SIGACTION as i32);
    msg_set_i32(msg, OFF_SIGACT_SIG, signo);
    msg_set_u64(msg, OFF_SIGACT_ACT, act_ptr);
    msg_set_u64(msg, OFF_SIGACT_OACT, oact_ptr);
    msg_set_u64(msg, OFF_SIGACT_RESTORER, restorer);
}

/// Build a PM_KILL request (m_type@4, signo@8, pid@12).
pub fn build_kill_msg(signo: i32, pid: i32, msg: &mut [u8; 64]) {
    msg_set_i32(msg, OFF_TYPE, PM_KILL as i32);
    msg_set_i32(msg, OFF_KILL_SIG, signo);
    msg_set_i32(msg, OFF_KILL_PID, pid);
}

/// Send a signal to a process.
///
/// Message layout: m_type = PM_KILL, m1i1 = signo, m1i2 = pid (matches PM
/// `handle_kill`).
pub fn kill(pid: i32, sig: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        build_kill_msg(sig, pid, &mut msg);
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

/// Examine or change a signal action (SIGNALS.md 2.1).
///
/// `handler` is SIG_DFL (0), SIG_IGN (1), or a handler address; `mask` is
/// the new signal mask (low 128 bits); `flags` the sa_flags. The act is
/// encoded in PM's 28-byte layout and the message uses PM's real offsets
/// (m_type@4, signo@8, act@24, oact@32).
pub fn sigaction(signo: i32, handler: u64, mask: u128, flags: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let act = encode_action(handler, mask, flags);
        let mut msg = [0u8; 64];
        // PM needs the caller's sigreturn trampoline address (m2l3@40) to
        // build the sigframe for caught signals (SIGNALS.md Phase 4).
        let restorer = minix_rt::sigreturn_trampoline_addr();
        build_sigaction_msg(signo, act.as_ptr() as u64, 0, restorer, &mut msg);
        match pm_call(&mut msg) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (signo, handler, mask, flags);
        Err(MinixErr::ENOSYS)
    }
}

/// POSIX `signal()`: set the disposition of `signo` to `handler`
/// (SIG_DFL / SIG_IGN / a handler address).
pub fn signal(signo: i32, handler: u64) -> Result<(), MinixErr> {
    sigaction(signo, handler, 0, 0)
}

/// Ignore a signal: set its disposition to SIG_IGN.
///
/// Delegates to the general `sigaction`, which encodes the act in PM's
/// 28-byte layout and uses PM's real message offsets. The act lives on the
/// caller's stack; PM reads it via cross-address-space SYS_VIRCOPY, which
/// resolves the caller's endpoint correctly (the minix_rt::SELF fix).
pub fn sig_ignore(sig: i32) -> Result<(), MinixErr> {
    sigaction(sig, SIG_IGN, 0, 0)
}

/// Examine and change the signal mask.
///
/// Message layout: m_type = PM_SIGPROCMASK, m1i1 = how, m2l1 = pointer to a
/// 16-byte mask (matches PM `do_sigprocmask`).
pub fn sigprocmask(how: i32, set: u64) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut mask = [0u8; 16];
        mask[0..8].copy_from_slice(&set.to_ne_bytes());
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_SIGPROCMASK as i32);
        msg_set_i32(&mut msg, OFF_SIGMASK_HOW, how);
        msg_set_u64(&mut msg, OFF_SIGMASK_SET, mask.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_SIGMASK_OLD, 0);
        match pm_call(&mut msg) {
            Ok(_) => Ok(()),
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

// Tests

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
    fn test_sig_dispositions() {
        assert_eq!(SIG_DFL, 0);
        assert_eq!(SIG_IGN, 1);
    }

    #[test]
    fn test_sigaction_message_layout() {
        // The PM_SIGACTION request via the production builders: m_type@4,
        // signo@8, act pointer@24, oact@32, sigreturn trampoline@40 —
        // matching PM's dispatch and do_sigaction offsets. The act encodes
        // SIG_IGN (handler=1) at byte 0 of the 28-byte PM sigaction layout.
        let act = encode_action(SIG_IGN, 0, 0);
        let mut msg = [0u8; 64];
        build_sigaction_msg(
            SIGINT,
            act.as_ptr() as u64,
            0,
            0x1234_5678_9ABC_DEF0,
            &mut msg,
        );

        assert_eq!(msg_i32(&msg, OFF_TYPE), PM_SIGACTION as i32);
        assert_eq!(msg_i32(&msg, OFF_SIGACT_SIG), SIGINT);
        assert_eq!(msg_u64(&msg, OFF_SIGACT_ACT), act.as_ptr() as u64);
        assert_eq!(msg_u64(&msg, OFF_SIGACT_OACT), 0);
        assert_eq!(msg_u64(&msg, OFF_SIGACT_RESTORER), 0x1234_5678_9ABC_DEF0);
        assert_eq!(u64::from_ne_bytes(act[0..8].try_into().unwrap()), SIG_IGN);
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
    fn test_encode_decode_action_roundtrip() {
        let act = encode_action(0x0102_0304_0506_0708, 0x0807_0605_0403_0201, 0x04);
        let (handler, mask, flags) = decode_action(&act);
        assert_eq!(handler, 0x0102_0304_0506_0708);
        assert_eq!(mask, 0x0807_0605_0403_0201);
        assert_eq!(flags, 0x04);
    }

    #[test]
    fn test_encode_action_layout() {
        // PM's 28-byte sigaction: handler u64@0, mask 16 bytes@8, flags
        // i32@24. A 64-bit mask lands in the low half of the 16-byte field.
        let act = encode_action(0x1122_3344_5566_7788, 0xDEAD_BEEF_CAFE_BABE, 0x08);
        assert_eq!(act.len(), 28);
        assert_eq!(
            u64::from_ne_bytes(act[0..8].try_into().unwrap()),
            0x1122_3344_5566_7788
        );
        assert_eq!(
            u64::from_ne_bytes(act[8..16].try_into().unwrap()),
            0xDEAD_BEEF_CAFE_BABE
        );
        assert_eq!(u64::from_ne_bytes(act[16..24].try_into().unwrap()), 0);
        assert_eq!(i32::from_ne_bytes(act[24..28].try_into().unwrap()), 0x08);
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
        // clock_gettime routes through PM_GETTIMEOFDAY (0x01C): PM has no
        // PM_CLOCK_GETTIME (0x022) handler, so the reply comes from
        // do_time (m2l1 = tv_sec @ 24, m2l2 = tv_usec @ 32).
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_GETTIMEOFDAY as i32);

        assert_eq!(msg_i32(&msg, OFF_TYPE), 0x01C);
    }

    #[test]
    fn test_clock_settime_message_format() {
        // mess_lc_pm_time layout: sec @ 8, clk_id @ 16, now @ 20, nsec @ 24.
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_CLOCK_SETTIME as i32);
        msg_set_i64(&mut msg, OFF_CLOCK_SEC, 42);
        msg_set_i32(&mut msg, OFF_CLOCK_ID, CLOCK_REALTIME);
        msg_set_i32(&mut msg, OFF_CLOCK_NOW, 1);
        msg_set_i64(&mut msg, OFF_CLOCK_NSEC, 500);

        assert_eq!(msg_i32(&msg, OFF_TYPE), 0x023);
        assert_eq!(msg_i64(&msg, OFF_CLOCK_SEC), 42);
        assert_eq!(msg_i32(&msg, OFF_CLOCK_ID), 0);
        assert_eq!(msg_i32(&msg, OFF_CLOCK_NOW), 1);
        assert_eq!(msg_i64(&msg, OFF_CLOCK_NSEC), 500);
    }

    #[test]
    fn test_kill_message_format() {
        // PM handle_kill reads m1i1 = signo@8, m1i2 = pid@12.
        let mut msg = [0u8; 64];
        build_kill_msg(SIGTERM, 123, &mut msg);

        assert_eq!(msg_i32(&msg, OFF_TYPE), PM_KILL as i32);
        assert_eq!(msg_i32(&msg, OFF_KILL_SIG), 15);
        assert_eq!(msg_i32(&msg, OFF_KILL_PID), 123);
    }

    #[test]
    fn test_sigprocmask_message_format() {
        // PM do_sigprocmask reads m1i1 = how@8, m2l1 = set ptr@24.
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_SIGPROCMASK as i32);
        msg_set_i32(&mut msg, OFF_SIGMASK_HOW, SIG_SETMASK);
        msg_set_u64(&mut msg, OFF_SIGMASK_SET, 0xFFFF);

        assert_eq!(msg_i32(&msg, OFF_TYPE), PM_SIGPROCMASK as i32);
        assert_eq!(msg_i32(&msg, OFF_SIGMASK_HOW), SIG_SETMASK);
        assert_eq!(msg_u64(&msg, OFF_SIGMASK_SET), 0xFFFF);
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

    #[test]
    fn test_sigaction_returns_enosys_on_host() {
        let r = sigaction(SIGINT, SIG_IGN, 0, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_signal_returns_enosys_on_host() {
        let r = signal(SIGINT, SIG_IGN);
        assert!(r.is_err());
    }

    type SignalHandler = fn(i32, u64, u128, i32) -> Result<(), MinixErr>;

    #[test]
    fn test_sigaction_signature() {
        fn _check(f: SignalHandler) {
            let _ = f;
        }
        _check(sigaction);
    }
}
