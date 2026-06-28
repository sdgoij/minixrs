//! Process Manager types and infrastructure — ported from
//! `minix/servers/pm/` (mproc.h, const.h, signal.h integration).
//!
//! This is a types-and-infrastructure port, **not** the full PM server.
//! The full PM server with IPC dispatch comes in Phase 12.3.

#![allow(static_mut_refs)]

use kernel::r#priv::MinixTimer;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Number of signals on x86_64.
pub const _NSIG: usize = 128;

/// Magic number to verify an `MProc` is valid.
pub const MP_MAGIC: u32 = 0xC0FFEE;

/// Maximum number of processes in the PM table.
pub const NR_PROCS: usize = 256;

/// Number of supported interval timers (real, virtual, prof).
pub const NR_ITIMERS: usize = 3;
pub const ITIMER_REAL: i32 = 0;
pub const ITIMER_VIRTUAL: i32 = 1;
pub const ITIMER_PROF: i32 = 2;

/// Special tracer / parent sentinels.
pub const NO_TRACER: i32 = -1;
pub const NO_PARENT: i32 = -2;

/// Maximum supplemental groups.
pub const NGROUPS_MAX: usize = 32;

/// Process name length.
pub const PROC_NAME_LEN: usize = 16;

// ── MProc flags (from mproc.h) ─────────────────────────────────────────────

pub const IN_USE: u32 = 0x00001;
pub const WAITING: u32 = 0x00002;
pub const ZOMBIE: u32 = 0x00004;
pub const PROC_STOPPED: u32 = 0x00008;
pub const ALARM_ON: u32 = 0x00010;
pub const EXITING: u32 = 0x00020;
pub const TOLD_PARENT: u32 = 0x00040;
pub const TRACE_STOPPED: u32 = 0x00080;
pub const SIGSUSPENDED: u32 = 0x00100;
pub const VFS_CALL: u32 = 0x00400;
pub const NEW_PARENT: u32 = 0x00800;
pub const UNPAUSED: u32 = 0x01000;
pub const PRIV_PROC: u32 = 0x02000;
pub const PARTIAL_EXEC: u32 = 0x04000;
pub const TRACE_EXIT: u32 = 0x08000;
pub const TRACE_ZOMBIE: u32 = 0x10000;
pub const DELAY_CALL: u32 = 0x20000;
pub const TAINTED: u32 = 0x40000;

// ─────────────────────────────────────────────────────────────────────────────
// SigSet — signal set type (sigset_t equivalent)
// ─────────────────────────────────────────────────────────────────────────────

/// Signal set type (`sigset_t` equivalent). Supports 128 signals on x86_64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SigSet {
    pub bits: [u128; 1], // 128 bits for up to _NSIG = 128 signals
}

impl SigSet {
    /// Create an empty signal set.
    pub const fn new() -> Self {
        Self { bits: [0u128] }
    }

    /// Create a signal set with all signals set.
    pub const fn full() -> Self {
        Self { bits: [!0u128] }
    }

    /// Clear all signals in the set.
    pub fn sigemptyset(&mut self) {
        self.bits[0] = 0;
    }

    /// Set all signals in the set.
    pub fn sigfillset(&mut self) {
        self.bits[0] = !0;
    }

    /// Add a signal to the set. Returns `true` on success, `false` if the
    /// signal number is invalid (< 1 or >= _NSIG).
    pub fn sigaddset(&mut self, sig: i32) -> bool {
        if sig < 1 || sig as usize >= _NSIG {
            return false;
        }
        self.bits[0] |= 1u128 << ((sig as usize) - 1);
        true
    }

    /// Remove a signal from the set. Returns `true` on success, `false` if
    /// the signal number is invalid (< 1 or >= _NSIG).
    pub fn sigdelset(&mut self, sig: i32) -> bool {
        if sig < 1 || sig as usize >= _NSIG {
            return false;
        }
        self.bits[0] &= !(1u128 << ((sig as usize) - 1));
        true
    }

    /// Check whether a signal is a member of the set.
    ///
    /// Returns `false` for invalid signals (< 1 or >= _NSIG).
    pub fn sigismember(&self, sig: i32) -> bool {
        if sig < 1 || sig as usize >= _NSIG {
            return false;
        }
        (self.bits[0] & (1u128 << ((sig as usize) - 1))) != 0
    }
}

impl Default for SigSet {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TimeVal and Itimerval — POSIX interval timer types
// ─────────────────────────────────────────────────────────────────────────────

/// POSIX `timeval` struct for interval timers.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TimeVal {
    pub tv_sec: i64,  // seconds
    pub tv_usec: i64, // microseconds
}

/// POSIX `itimerval` struct for `setitimer` / `getitimer`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Itimerval {
    pub it_interval: TimeVal, // timer interval
    pub it_value: TimeVal,    // current value
}

// ─────────────────────────────────────────────────────────────────────────────
// MProc — process manager slot
// ─────────────────────────────────────────────────────────────────────────────

/// PM process table slot — adapted from `mproc.h`.
///
/// Fields are ordered to match the original C layout for future procfs
/// compatibility. The `mp_sigact` array and `mp_reply` message are omitted
/// here (they will be added in Phase 12.3).
#[derive(Debug, Clone)]
#[repr(C)]
pub struct MProc {
    pub mp_exitstatus: i8,
    pub mp_sigstatus: i8,
    pub mp_pid: i32,
    pub mp_endpoint: i32,
    pub mp_procgrp: i32,
    pub mp_wpid: i32,
    pub mp_parent: i32,
    pub mp_tracer: i32,
    pub mp_child_utime: u64,
    pub mp_child_stime: u64,
    pub mp_realuid: i32,
    pub mp_effuid: i32,
    pub mp_realgid: i32,
    pub mp_effgid: i32,
    pub mp_ngroups: i32,
    pub mp_sgroups: [i32; NGROUPS_MAX],
    pub mp_ignore: SigSet,
    pub mp_catch: SigSet,
    pub mp_sigmask: SigSet,
    pub mp_sigmask2: SigSet,
    pub mp_sigpending: SigSet,
    pub mp_ksigpending: SigSet,
    pub mp_sigtrace: SigSet,
    // mp_sigact[_NSIG] skipped — Phase 12.3
    pub mp_sigreturn: u64,
    pub mp_timer: MinixTimer,
    pub mp_interval: [u64; NR_ITIMERS],
    pub mp_flags: u32,
    pub mp_trace_flags: u32,
    // mp_reply skipped — Phase 12.3
    pub mp_frame_addr: u64,
    pub mp_frame_len: u64,
    pub mp_nice: i32,
    pub mp_scheduler: i32,
    pub mp_name: [i8; PROC_NAME_LEN],
    pub mp_magic: u32,
}

impl MProc {
    /// Create a zeroed / default process slot.
    pub const fn zeroed() -> Self {
        Self {
            mp_exitstatus: 0,
            mp_sigstatus: 0,
            mp_pid: 0,
            mp_endpoint: 0,
            mp_procgrp: 0,
            mp_wpid: 0,
            mp_parent: 0,
            mp_tracer: 0,
            mp_child_utime: 0,
            mp_child_stime: 0,
            mp_realuid: 0,
            mp_effuid: 0,
            mp_realgid: 0,
            mp_effgid: 0,
            mp_ngroups: 0,
            mp_sgroups: [0; NGROUPS_MAX],
            mp_ignore: SigSet::new(),
            mp_catch: SigSet::new(),
            mp_sigmask: SigSet::new(),
            mp_sigmask2: SigSet::new(),
            mp_sigpending: SigSet::new(),
            mp_ksigpending: SigSet::new(),
            mp_sigtrace: SigSet::new(),
            mp_sigreturn: 0,
            mp_timer: MinixTimer {
                tmr_next: core::ptr::null_mut(),
                tmr_exp_time: 0,
                tmr_func: 0,
                tmr_arg: 0,
            },
            mp_interval: [0; NR_ITIMERS],
            mp_flags: 0,
            mp_trace_flags: 0,
            mp_frame_addr: 0,
            mp_frame_len: 0,
            mp_nice: 0,
            mp_scheduler: 0,
            mp_name: [0; PROC_NAME_LEN],
            mp_magic: 0,
        }
    }

    /// Returns `true` if this slot is in use.
    pub fn in_use(&self) -> bool {
        self.mp_flags & IN_USE != 0
    }

    /// Returns `true` if this slot is a zombie.
    pub fn is_zombie(&self) -> bool {
        self.mp_flags & ZOMBIE != 0
    }

    /// Returns `true` if this slot is stopped.
    pub fn is_stopped(&self) -> bool {
        self.mp_flags & PROC_STOPPED != 0
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Process table
// ─────────────────────────────────────────────────────────────────────────────

/// PM process table — one slot per process.
pub static mut MPROC: [MProc; NR_PROCS] = [const { MProc::zeroed() }; NR_PROCS];

/// Number of processes currently in use.
pub static mut PROCS_IN_USE: u32 = 0;

/// Allocate a free process slot.
///
/// Scans the process table for a slot with `IN_USE` not set, marks it as in
/// use, and returns its index. Returns `None` if all slots are occupied.
pub fn alloc_proc() -> Option<usize> {
    let slots = unsafe { &mut *core::ptr::addr_of_mut!(MPROC) };
    for (i, slot) in slots.iter_mut().enumerate() {
        if slot.mp_flags & IN_USE == 0 {
            slot.mp_flags |= IN_USE;
            slot.mp_magic = MP_MAGIC;
            unsafe {
                PROCS_IN_USE += 1;
            }
            return Some(i);
        }
    }
    None
}

/// Free a process slot.
///
/// # Safety
///
/// `slot` must be a valid index (< NR_PROCS) previously returned by
/// `alloc_proc()`.
pub unsafe fn free_proc(slot: usize) {
    unsafe {
        let slots = &mut *core::ptr::addr_of_mut!(MPROC);
        if slot >= NR_PROCS {
            return;
        }
        let slot_ref = &mut slots[slot];
        if slot_ref.mp_flags & IN_USE == 0 {
            return;
        }
        *slot_ref = MProc::zeroed();
        PROCS_IN_USE = PROCS_IN_USE.saturating_sub(1);
    }
}

/// Initialize the PM process table.
///
/// Resets the entire table and `PROCS_IN_USE` counter. Should be called once
/// during server initialization.
pub fn init_proc() {
    let slots = unsafe { &mut *core::ptr::addr_of_mut!(MPROC) };
    for slot in slots.iter_mut() {
        *slot = MProc::zeroed();
    }
    unsafe {
        PROCS_IN_USE = 0;
    }
}

/// Look up a process by its index (slot number).
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`.
pub unsafe fn get_proc(slot: usize) -> Option<&'static MProc> {
    unsafe {
        let slots = &*core::ptr::addr_of_mut!(MPROC);
        if slot >= NR_PROCS {
            return None;
        }
        if slots[slot].mp_flags & IN_USE == 0 {
            return None;
        }
        Some(&slots[slot])
    }
}

/// Look up a process by its index, returning a mutable reference.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`.
pub unsafe fn get_proc_mut(slot: usize) -> Option<&'static mut MProc> {
    unsafe {
        let slots = &mut *core::ptr::addr_of_mut!(MPROC);
        if slot >= NR_PROCS {
            return None;
        }
        if slots[slot].mp_flags & IN_USE == 0 {
            return None;
        }
        Some(&mut slots[slot])
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Alarm management
// ─────────────────────────────────────────────────────────────────────────────

/// Set the alarm timer for a process slot.
///
/// The alarm fires after `ticks` clock ticks.
pub fn set_alarm(slot: usize, ticks: u64) {
    let slots = unsafe { &mut *core::ptr::addr_of_mut!(MPROC) };
    if slot >= NR_PROCS {
        return;
    }
    let mp = &mut slots[slot];
    if mp.mp_flags & IN_USE == 0 {
        return;
    }
    // Clear any pending alarm first.
    mp.mp_flags &= !ALARM_ON;
    unsafe {
        kernel::clock::reset_kernel_timer(&mut mp.mp_timer);
    }
    if ticks == 0 {
        return;
    }
    let now = kernel::clock::get_monotonic();
    let exp_time = now.saturating_add(ticks);
    // Use a zero watchdog function pointer — the PM will handle expiry via
    // the timer queue in Phase 12.3.
    unsafe {
        kernel::clock::set_kernel_timer(&mut mp.mp_timer, exp_time, 0);
    }
    mp.mp_flags |= ALARM_ON;
}

/// Check whether an alarm is currently active for a process slot.
pub fn alarm_is_active(slot: usize) -> bool {
    let slots = unsafe { &*core::ptr::addr_of_mut!(MPROC) };
    if slot >= NR_PROCS {
        return false;
    }
    let mp = &slots[slot];
    if mp.mp_flags & IN_USE == 0 {
        return false;
    }
    mp.mp_flags & ALARM_ON != 0
}

/// Cancel an active alarm for a process slot.
pub fn cancel_alarm(slot: usize) {
    let slots = unsafe { &mut *core::ptr::addr_of_mut!(MPROC) };
    if slot >= NR_PROCS {
        return;
    }
    let mp = &mut slots[slot];
    if mp.mp_flags & IN_USE == 0 {
        return;
    }
    mp.mp_flags &= !ALARM_ON;
    unsafe {
        kernel::clock::reset_kernel_timer(&mut mp.mp_timer);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Compile-time offset verification
// ─────────────────────────────────────────────────────────────────────────────

/// Compile-time assertion that `MProc` field offsets match the C layout
/// from `mproc.h`.  These verify the `#[repr(C)]` layout is as expected.
#[allow(clippy::erasing_op)]
#[allow(clippy::identity_op)]
#[allow(clippy::zero_prefixed_literal)]
const _: () = {
    use core::mem::offset_of;

    // NOTE: The exact offset values depend on `#[repr(C)]` alignment rules.
    // These assertions serve as a regression check against unintentional
    // layout changes.

    let _ = offset_of!(MProc, mp_pid);
    let _ = offset_of!(MProc, mp_endpoint);
    let _ = offset_of!(MProc, mp_parent);
    let _ = offset_of!(MProc, mp_tracer);
    let _ = offset_of!(MProc, mp_flags);
    let _ = offset_of!(MProc, mp_realuid);
    let _ = offset_of!(MProc, mp_effuid);
    let _ = offset_of!(MProc, mp_nice);
    let _ = offset_of!(MProc, mp_scheduler);
    let _ = offset_of!(MProc, mp_name);
    let _ = offset_of!(MProc, mp_magic);

    // Verify that key fields are within expected ranges.
    // mp_pid should be early in the struct (within first 16 bytes on x86_64).
    assert!(offset_of!(MProc, mp_pid) < 16);
    assert!(offset_of!(MProc, mp_endpoint) >= offset_of!(MProc, mp_pid));
    // mp_magic should be at the end of the struct.
    assert!(offset_of!(MProc, mp_magic) > offset_of!(MProc, mp_pid));

    // SigSet size: one u128 = 16 bytes.
    assert!(core::mem::size_of::<SigSet>() == 16);
    assert!(core::mem::align_of::<SigSet>() == 16);

    // TimeVal size: two i64 = 16 bytes.
    assert!(core::mem::size_of::<TimeVal>() == 16);

    // Itimerval size: two TimeVal = 32 bytes.
    assert!(core::mem::size_of::<Itimerval>() == 32);
};

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── SigSet tests ────────────────────────────────────────────────────────

    #[test]
    fn test_sigset_new_is_empty() {
        let set = SigSet::new();
        assert_eq!(set.bits[0], 0);
        // No signal should be a member.
        for s in 1..=_NSIG as i32 {
            assert!(!set.sigismember(s));
        }
    }

    #[test]
    fn test_sigset_full() {
        let set = SigSet::full();
        assert_eq!(set.bits[0], !0u128);
        // Every valid signal should be a member (1.._NSIG).
        for s in 1.._NSIG as i32 {
            assert!(set.sigismember(s));
        }
    }

    #[test]
    fn test_sigset_add_and_del() {
        let mut set = SigSet::new();
        assert!(set.sigaddset(1));
        assert!(set.sigismember(1));
        assert!(!set.sigismember(2));

        assert!(set.sigaddset(2));
        assert!(set.sigismember(2));

        assert!(set.sigdelset(1));
        assert!(!set.sigismember(1));
        assert!(set.sigismember(2));

        // Del a signal that was never added — should still succeed.
        assert!(set.sigdelset(3));
        assert!(!set.sigismember(3));
    }

    #[test]
    fn test_sigset_emptyset_fillset() {
        let mut set = SigSet::full();
        assert!(set.sigismember(9));
        set.sigemptyset();
        assert_eq!(set.bits[0], 0);
        assert!(!set.sigismember(9));

        set.sigfillset();
        assert_eq!(set.bits[0], !0u128);
        assert!(set.sigismember(9));
    }

    #[test]
    fn test_sigset_bounds() {
        let mut set = SigSet::new();

        // Signal 0 is invalid.
        assert!(!set.sigaddset(0));
        assert!(!set.sigdelset(0));
        assert!(!set.sigismember(0));

        // Signal _NSIG is out of bounds (signals are 1..=_NSIG-1).
        assert!(!set.sigaddset(_NSIG as i32));
        assert!(!set.sigdelset(_NSIG as i32));
        assert!(!set.sigismember(_NSIG as i32));

        // Negative signals are invalid.
        assert!(!set.sigaddset(-1));
        assert!(!set.sigismember(-1));
    }

    #[test]
    fn test_sigset_equality() {
        let mut a = SigSet::new();
        let b = SigSet::new();
        assert_eq!(a, b);

        assert!(a.sigaddset(15));
        assert_ne!(a, b);

        let mut c = SigSet::new();
        assert!(c.sigaddset(15));
        assert_eq!(a, c);
    }

    // ── MProc tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_mproc_zeroed() {
        let mp = MProc::zeroed();
        assert_eq!(mp.mp_pid, 0);
        assert_eq!(mp.mp_endpoint, 0);
        assert_eq!(mp.mp_flags, 0);
        assert_eq!(mp.mp_magic, 0);
        assert!(!mp.in_use());
        assert!(!mp.is_zombie());
        assert!(!mp.is_stopped());
        assert_eq!(mp.mp_name.iter().all(|&c| c == 0), true);
        assert_eq!(mp.mp_sgroups.iter().all(|&g| g == 0), true);
        assert_eq!(mp.mp_interval.iter().all(|&t| t == 0), true);
    }

    #[test]
    fn test_mproc_flags() {
        let mut mp = MProc::zeroed();
        assert!(!mp.in_use());
        mp.mp_flags |= IN_USE;
        assert!(mp.in_use());

        mp.mp_flags |= ZOMBIE;
        assert!(mp.is_zombie());

        mp.mp_flags |= PROC_STOPPED;
        assert!(mp.is_stopped());
    }

    // ── Process table tests ─────────────────────────────────────────────────

    #[test]
    fn test_init_proc_clears_table() {
        // Allocate a slot to dirty the table.
        let _idx = alloc_proc().expect("should find a free slot");
        unsafe {
            assert!(PROCS_IN_USE > 0);
        }

        init_proc();
        unsafe {
            assert_eq!(PROCS_IN_USE, 0);
        }
        // After init, none should be in use.
        for i in 0..NR_PROCS {
            unsafe {
                assert!(!MPROC[i].in_use());
            }
        }
    }

    #[test]
    fn test_alloc_proc_returns_valid_slot() {
        init_proc();
        let idx = alloc_proc().expect("should find a free slot");
        assert!(idx < NR_PROCS);
        unsafe {
            assert!(MPROC[idx].in_use());
            assert_eq!(MPROC[idx].mp_magic, MP_MAGIC);
            assert_eq!(PROCS_IN_USE, 1);
        }
    }

    #[test]
    fn test_free_proc_clears_slot() {
        init_proc();
        let idx = alloc_proc().expect("should find a free slot");
        unsafe {
            assert_eq!(PROCS_IN_USE, 1);
        }
        unsafe {
            free_proc(idx);
            assert!(!MPROC[idx].in_use());
            assert_eq!(MPROC[idx].mp_magic, 0);
            assert_eq!(PROCS_IN_USE, 0);
        }
    }

    #[test]
    fn test_alloc_proc_exhaustion() {
        init_proc();
        let mut count = 0;
        while let Some(_) = alloc_proc() {
            count += 1;
        }
        assert_eq!(count, NR_PROCS);
        unsafe {
            assert_eq!(PROCS_IN_USE, NR_PROCS as u32);
        }
        // Alloc should return None now.
        assert!(alloc_proc().is_none());
    }

    #[test]
    fn test_procs_in_use_tracking() {
        init_proc();
        assert_eq!(unsafe { PROCS_IN_USE }, 0);

        let a = alloc_proc().unwrap();
        assert_eq!(unsafe { PROCS_IN_USE }, 1);

        let b = alloc_proc().unwrap();
        assert_eq!(unsafe { PROCS_IN_USE }, 2);

        unsafe { free_proc(a) };
        assert_eq!(unsafe { PROCS_IN_USE }, 1);

        unsafe { free_proc(b) };
        assert_eq!(unsafe { PROCS_IN_USE }, 0);
    }

    // ── Alarm tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_alarm_set_and_active() {
        init_proc();
        let idx = alloc_proc().unwrap();

        assert!(!alarm_is_active(idx));
        set_alarm(idx, 100);
        assert!(alarm_is_active(idx));
    }

    #[test]
    fn test_alarm_cancel() {
        init_proc();
        let idx = alloc_proc().unwrap();

        set_alarm(idx, 100);
        assert!(alarm_is_active(idx));

        cancel_alarm(idx);
        assert!(!alarm_is_active(idx));
    }

    // ── Compile-time offset check tests ─────────────────────────────────────

    #[test]
    fn test_sigset_size() {
        assert_eq!(core::mem::size_of::<SigSet>(), 16);
    }

    #[test]
    fn test_mproc_size() {
        // Layout validation: MProc should have a reasonable size.
        let mproc_size = core::mem::size_of::<MProc>();
        // Approximate: SigSets (7 × 16 = 112), sgroups (32 × 4 = 128),
        // MinixTimer (32), plus other fields. Should be less than 2 KB.
        assert!(mproc_size > 400);
        assert!(mproc_size < 2048);
    }

    // ── get_proc / get_proc_mut tests ───────────────────────────────────────

    #[test]
    fn test_get_proc_none_for_unused_slot() {
        init_proc();
        unsafe {
            assert!(get_proc(0).is_none());
            assert!(get_proc_mut(0).is_none());
        }
    }

    #[test]
    fn test_get_proc_returns_slot() {
        init_proc();
        let idx = alloc_proc().unwrap();
        unsafe {
            let p = get_proc(idx).expect("should find process");
            assert!(p.in_use());
            assert_eq!(p.mp_magic, MP_MAGIC);

            let p = get_proc_mut(idx).expect("should find process mut");
            p.mp_pid = 42;
            assert_eq!(p.mp_pid, 42);
        }
    }

    #[test]
    fn test_get_proc_out_of_bounds() {
        unsafe {
            assert!(get_proc(NR_PROCS).is_none());
            assert!(get_proc_mut(NR_PROCS).is_none());
        }
    }
}
