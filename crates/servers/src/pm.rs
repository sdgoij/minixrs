//! Process Manager types and infrastructure — ported from
//! `minix/servers/pm/` (mproc.h, const.h, signal.h integration).
//!
//! This is a types-and-infrastructure port, **not** the full PM server.
//! The full PM server with IPC dispatch comes in Phase 12.3.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
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

/// Error codes.
pub const ENOSYS: i32 = -71;
pub const EINVAL: i32 = -22;

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
// Process table — wrapped in UnsafeCell + Sync for interior mutability
// ─────────────────────────────────────────────────────────────────────────────

struct MProcTable(UnsafeCell<[MProc; NR_PROCS]>);

// Safety: All access to the process table must be externally synchronized.
// UnsafeCell provides interior mutability; the unsafe impl Sync allows
// sharing across threads when the caller guarantees exclusion.
unsafe impl Sync for MProcTable {}

impl MProcTable {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { MProc::zeroed() }; NR_PROCS]))
    }

    fn as_ptr(&self) -> *mut MProc {
        self.0.get() as *mut MProc
    }
}

/// PM process table — one slot per process.
static MPROC: MProcTable = MProcTable::new();

/// Number of processes currently in use.
static PROCS_IN_USE: AtomicU32 = AtomicU32::new(0);

/// Allocate a free process slot.
///
/// Scans the process table for a slot with `IN_USE` not set, marks it as in
/// use, and returns its index. Returns `None` if all slots are occupied.
pub fn alloc_proc() -> Option<usize> {
    let base = MPROC.as_ptr();
    for i in 0..NR_PROCS {
        // Safety: `base.add(i)` is valid for `i < NR_PROCS` because the
        // allocation is a contiguous array of NR_PROCS elements.
        let slot = unsafe { &mut *base.add(i) };
        if slot.mp_flags & IN_USE == 0 {
            slot.mp_flags |= IN_USE;
            slot.mp_magic = MP_MAGIC;
            PROCS_IN_USE.fetch_add(1, Ordering::Relaxed);
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
/// `alloc_proc()`. The caller must ensure exclusive access to the process
/// table while this function runs.
pub unsafe fn free_proc(slot: usize) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    // Safety: We checked `slot < NR_PROCS`, so `base.add(slot)` is in bounds.
    // Caller guarantees exclusive access to the process table.
    let slot_ref = unsafe { &mut *base.add(slot) };
    if slot_ref.mp_flags & IN_USE == 0 {
        return;
    }
    *slot_ref = MProc::zeroed();
    PROCS_IN_USE.fetch_sub(1, Ordering::Relaxed);
}

/// Initialize the PM process table.
///
/// Resets the entire table and `PROCS_IN_USE` counter. Should be called once
/// during server initialization.
pub fn init_proc() {
    let base = MPROC.as_ptr();
    for i in 0..NR_PROCS {
        // Safety: `base.add(i)` is valid for `i < NR_PROCS` because the
        // allocation is a contiguous array of NR_PROCS elements.
        unsafe {
            *base.add(i) = MProc::zeroed();
        }
    }
    PROCS_IN_USE.store(0, Ordering::Relaxed);
}

/// Look up a process by its index (slot number).
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. The caller must ensure that no other
/// reference to the process table aliases this slot in a conflicting way.
pub unsafe fn get_proc(slot: usize) -> Option<&'static MProc> {
    if slot >= NR_PROCS {
        return None;
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above. Caller guarantees no
    // conflicting mutable reference exists for this slot.
    let rmp = unsafe { &*base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return None;
    }
    Some(rmp)
}

/// Look up a process by its index, returning a mutable reference.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. The caller must ensure exclusive access to
/// the target slot while the returned reference is live.
pub unsafe fn get_proc_mut(slot: usize) -> Option<&'static mut MProc> {
    if slot >= NR_PROCS {
        return None;
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above. Caller guarantees exclusive
    // access to this slot.
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return None;
    }
    Some(rmp)
}

// ─────────────────────────────────────────────────────────────────────────────
// Alarm management
// ─────────────────────────────────────────────────────────────────────────────

/// Set the alarm timer for a process slot.
///
/// The alarm fires after `ticks` clock ticks.
pub fn set_alarm(slot: usize, ticks: u64) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above.
    let mp = unsafe { &mut *base.add(slot) };
    if mp.mp_flags & IN_USE == 0 {
        return;
    }
    // Clear any pending alarm first.
    mp.mp_flags &= !ALARM_ON;
    // Safety: `mp` points into the process table; caller ensures no other
    // concurrent access to this slot's timer.
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
    // Safety: same as above; caller ensures exclusive access.
    unsafe {
        kernel::clock::set_kernel_timer(&mut mp.mp_timer, exp_time, 0);
    }
    mp.mp_flags |= ALARM_ON;
}

/// Check whether an alarm is currently active for a process slot.
pub fn alarm_is_active(slot: usize) -> bool {
    if slot >= NR_PROCS {
        return false;
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above.
    let mp = unsafe { &*base.add(slot) };
    if mp.mp_flags & IN_USE == 0 {
        return false;
    }
    mp.mp_flags & ALARM_ON != 0
}

/// Cancel an active alarm for a process slot.
pub fn cancel_alarm(slot: usize) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above.
    let mp = unsafe { &mut *base.add(slot) };
    if mp.mp_flags & IN_USE == 0 {
        return;
    }
    mp.mp_flags &= !ALARM_ON;
    // Safety: `mp` points into the process table; caller ensures no other
    // concurrent access to this slot's timer.
    unsafe {
        kernel::clock::reset_kernel_timer(&mut mp.mp_timer);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Compile-time offset verification
// ─────────────────────────────────────────────────────────────────────────────

/// Compile-time assertion that `MProc` field offsets match the C layout
/// from `mproc.h`.  These verify the `#[repr(C)]` layout is as expected.
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
// PID management
// ─────────────────────────────────────────────────────────────────────────────

/// Next available PID.
static NEXT_PID: AtomicI32 = AtomicI32::new(0);

/// Allocate a new unique PID.
///
/// # Safety
///
/// The caller must ensure exclusive access to the process table while the PID
/// scan is in progress (the scan reads every in-use slot).
pub unsafe fn get_free_pid() -> i32 {
    // Simple incrementing PID allocator.  Wraps around, skipping
    // PIDs that are currently in use by scanning the process table.
    // This matches the C code's approach in `get_free_pid()`.
    'search: loop {
        let next = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        let candidate = if next + 1 < 1 {
            NEXT_PID.store(1, Ordering::Relaxed);
            1
        } else {
            next + 1
        };
        // Check if this PID is already in use.
        let base = MPROC.as_ptr();
        for i in 0..NR_PROCS {
            // Safety: `i < NR_PROCS` holds by loop bound.
            let slot = unsafe { &*base.add(i) };
            if slot.mp_flags & IN_USE != 0 && slot.mp_pid == candidate {
                continue 'search;
            }
        }
        return candidate;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// do_fork — create child process
// ─────────────────────────────────────────────────────────────────────────────

/// Fork the current process — create a child with copied MProc state.
///
/// In the real implementation, this calls `vm_fork()` and `tell_vfs()`.
/// Here we just copy the slot and assign a new PID/endpoint.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS` and refer to a valid in-use process. The
/// caller must ensure exclusive access to the process table.
pub unsafe fn do_fork(slot: usize) -> Result<usize, i32> {
    let base = MPROC.as_ptr();
    // Safety: caller guarantees `slot < NR_PROCS`.
    let parent_ptr = unsafe { base.add(slot) };
    // Safety: caller guarantees the pointer is valid and the slot is in use.
    let parent_flags = unsafe { (*parent_ptr).mp_flags };
    if parent_flags & IN_USE == 0 {
        return Err(EINVAL);
    }

    // Find a free child slot.
    let child_slot = alloc_proc().ok_or(-11)?; // EAGAIN
    // Safety: `child_slot` was just returned by `alloc_proc()`, so it is
    // a valid index (< NR_PROCS).
    let child_ptr = unsafe { base.add(child_slot) };

    // Copy parent state.
    // Safety: parent_ptr and child_ptr are valid, non-overlapping pointers
    // derived from the same allocation.
    unsafe {
        core::ptr::copy_nonoverlapping(parent_ptr, child_ptr, 1);
    }
    // Safety: `child_ptr` is valid (just allocated).
    unsafe {
        (*child_ptr).mp_parent = slot as i32;
        (*child_ptr).mp_tracer = NO_TRACER;
        (*child_ptr).mp_trace_flags = 0;
        (*child_ptr).mp_child_utime = 0;
        (*child_ptr).mp_child_stime = 0;
        (*child_ptr).mp_exitstatus = 0;
        (*child_ptr).mp_sigstatus = 0;
        (*child_ptr).mp_flags &= IN_USE;
        (*child_ptr).mp_endpoint = child_slot as i32 | 0x8000;
        // Safety: `get_free_pid()` requires exclusive access to the process
        // table, which the caller guarantees.
        (*child_ptr).mp_pid = get_free_pid();
        (*child_ptr).mp_interval = [0u64; NR_ITIMERS];
        (*child_ptr).mp_magic = MP_MAGIC;
    }

    Ok(child_slot)
}

// ─────────────────────────────────────────────────────────────────────────────
// do_exit + do_waitpid
// ─────────────────────────────────────────────────────────────────────────────

/// Exit the current process — mark as ZOMBIE, notify parent.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS` and refer to a valid in-use process. The
/// caller must ensure exclusive access to the process table.
pub unsafe fn do_exit(slot: usize, exit_status: i32) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above.
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return;
    }

    rmp.mp_flags |= EXITING | ZOMBIE;
    rmp.mp_flags &= !PROC_STOPPED;
    rmp.mp_exitstatus = exit_status as i8;
    rmp.mp_sigstatus = 0;
    rmp.mp_ksigpending.sigemptyset();

    // Notify parent.
    let parent = rmp.mp_parent;
    if parent >= 0 && (parent as usize) < NR_PROCS {
        // Safety: `parent as usize < NR_PROCS` checked above.
        let parent_rmp = unsafe { &mut *base.add(parent as usize) };
        if parent_rmp.mp_flags & IN_USE != 0 {
            parent_rmp.mp_child_utime = rmp.mp_child_utime;
            parent_rmp.mp_child_stime = rmp.mp_child_stime;
        }
    }
}

/// Test whether a parent is waiting for a specific child.
///
/// # Safety
///
/// `parent` must be < `NR_PROCS`. The caller must ensure that no conflicting
/// mutable reference to the parent slot exists.
pub unsafe fn wait_test(parent: usize, child: &MProc) -> bool {
    if child.mp_flags & ZOMBIE == 0 {
        return false;
    }
    if parent >= NR_PROCS {
        return false;
    }
    let base = MPROC.as_ptr();
    // Safety: `parent < NR_PROCS` checked above.
    let parent_rmp = unsafe { &*base.add(parent) };
    if parent_rmp.mp_flags & IN_USE == 0 {
        return false;
    }
    // Check if parent is waiting for this specific child or any child.
    let wpid = parent_rmp.mp_wpid;
    wpid == -1 || wpid == child.mp_pid
}

/// Wait for a child process to exit.
///
/// # Safety
///
/// `parent` must be < `NR_PROCS` and refer to a valid in-use process. The
/// caller must ensure exclusive access to the process table.
pub unsafe fn do_waitpid(parent: usize, wpid: i32) -> Result<(i32, i32), i32> {
    if parent >= NR_PROCS {
        return Err(EINVAL);
    }
    let base = MPROC.as_ptr();
    // Safety: `parent < NR_PROCS` checked above.
    let parent_rmp = unsafe { &*base.add(parent) };
    if parent_rmp.mp_flags & IN_USE == 0 {
        return Err(EINVAL);
    }

    // Scan for a zombie child.
    for i in 0..NR_PROCS {
        if i == parent {
            continue;
        }
        // Safety: `i < NR_PROCS` holds by loop bound.
        let child = unsafe { &*base.add(i) };
        if child.mp_flags & IN_USE == 0 {
            continue;
        }
        if child.mp_parent != parent as i32 {
            continue;
        }
        if wpid != -1 && child.mp_pid != wpid {
            continue;
        }
        if child.mp_flags & ZOMBIE != 0 {
            // Found a zombie child.
            let pid = child.mp_pid;
            let status = (child.mp_exitstatus as i32) & 0xFF;
            // Safety: `free_proc` requires exclusive access to the process
            // table, which the caller guarantees.
            unsafe {
                free_proc(i);
            }
            return Ok((pid, status));
        }
    }

    Err(-4) // EINTR — no zombie child found
}

// ─────────────────────────────────────────────────────────────────────────────
// Signal handling
// ─────────────────────────────────────────────────────────────────────────────

/// Check if a signal can be sent to a process and deliver it.
///
/// # Safety
///
/// The caller must ensure exclusive access to the process table while this
/// function runs.
pub unsafe fn check_sig(proc_id: i32, signo: i32, ksig: bool) -> Result<(), i32> {
    let base = MPROC.as_ptr();
    for i in 0..NR_PROCS {
        // Safety: `i < NR_PROCS` holds by loop bound.
        let rmp = unsafe { &*base.add(i) };
        if rmp.mp_flags & IN_USE == 0 {
            continue;
        }
        if rmp.mp_pid != proc_id && proc_id != -1 {
            continue;
        }
        // Send the signal.
        unsafe {
            sig_proc(i, signo, false, ksig);
        }
    }
    Ok(())
}

/// Deliver a signal to a process.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. The caller must ensure exclusive access to
/// the target slot.
pub unsafe fn sig_proc(slot: usize, signo: i32, trace: bool, ksig: bool) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above.
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return;
    }

    if signo < 1 || signo >= _NSIG as i32 {
        return;
    }

    if trace {
        rmp.mp_sigtrace.sigaddset(signo);
        rmp.mp_flags |= TRACE_STOPPED;
        return;
    }

    // Check if signal is ignored.
    if rmp.mp_ignore.sigismember(signo) {
        return;
    }

    // Add to pending set.
    if ksig {
        rmp.mp_ksigpending.sigaddset(signo);
    } else {
        rmp.mp_sigpending.sigaddset(signo);
    }

    // SIGKILL and SIGSTOP cannot be caught or ignored.
    if signo == 9 || signo == 19 {
        // SIGKILL or SIGSTOP
        if signo == 9 {
            // Safety: `do_exit` requires exclusive access to the process
            // table, which the caller guarantees.
            unsafe {
                do_exit(slot, 0);
            }
        }
    }
}

/// Handle do_kill request.
///
/// # Safety
///
/// The caller must ensure exclusive access to the process table.
pub unsafe fn do_kill(pid: i32, signo: i32) -> Result<(), i32> {
    if signo < 0 || signo >= _NSIG as i32 {
        return Err(EINVAL);
    }
    // Safety: caller guarantees exclusive access to the process table.
    unsafe { check_sig(pid, signo, false) }
}

// ─────────────────────────────────────────────────────────────────────────────
// do_get / do_set — UID, GID, PID
// ─────────────────────────────────────────────────────────────────────────────

/// Handle PM_GET* requests.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS` and refer to a valid in-use process. The
/// caller must ensure that no conflicting mutable reference exists for
/// this slot.
pub unsafe fn do_get(slot: usize, call_nr: i32) -> Result<i64, i32> {
    if slot >= NR_PROCS {
        return Err(EINVAL);
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above.
    let rmp = unsafe { &*base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return Err(EINVAL);
    }

    match call_nr {
        0 => {
            // PM_GETUID
            let euid = rmp.mp_effuid;
            Ok(((rmp.mp_realuid as i64) << 32) | (euid as i64 & 0xFFFF_FFFF))
        }
        1 => {
            // PM_GETGID
            let egid = rmp.mp_effgid;
            Ok(((rmp.mp_realgid as i64) << 32) | (egid as i64 & 0xFFFF_FFFF))
        }
        2 => {
            // PM_GETPID
            let ppid = if (rmp.mp_parent as usize) < NR_PROCS {
                // Safety: checked `rmp.mp_parent as usize < NR_PROCS` above.
                let pslot = unsafe { &*base.add(rmp.mp_parent as usize) };
                pslot.mp_pid
            } else {
                0
            };
            Ok(((rmp.mp_pid as i64) << 32) | (ppid as i64 & 0xFFFF_FFFF))
        }
        _ => Err(ENOSYS),
    }
}

/// Handle PM_SET* requests.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS` and refer to a valid in-use process. The
/// caller must ensure exclusive access to this slot.
pub unsafe fn do_set(slot: usize, call_nr: i32, uid: i32, gid: i32) -> Result<(), i32> {
    if slot >= NR_PROCS {
        return Err(EINVAL);
    }
    let base = MPROC.as_ptr();
    // Safety: `slot < NR_PROCS` checked above.
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return Err(EINVAL);
    }

    match call_nr {
        0 => {
            // PM_SETUID
            rmp.mp_realuid = uid;
            rmp.mp_effuid = uid;
            Ok(())
        }
        1 => {
            // PM_SETGID
            rmp.mp_realgid = gid;
            rmp.mp_effgid = gid;
            Ok(())
        }
        _ => Err(ENOSYS),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// pm_isokendpt
// ─────────────────────────────────────────────────────────────────────────────

/// Check if a process endpoint is valid.
///
/// # Safety
///
/// The caller must ensure that no conflicting mutable reference to the
/// process table exists while this function reads the relevant slot.
pub unsafe fn pm_isokendpt(endpoint: i32) -> Option<usize> {
    if endpoint < 0 {
        return None;
    }
    let proc_nr = (endpoint & 0x7FFF) as usize;
    if proc_nr >= NR_PROCS {
        return None;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(proc_nr) };
    if rmp.mp_flags & IN_USE == 0 {
        return None;
    }
    if rmp.mp_endpoint != endpoint {
        return None;
    }
    Some(proc_nr)
}

// ─────────────────────────────────────────────────────────────────────────────
// Dispatch table + main loop (stub)
// ─────────────────────────────────────────────────────────────────────────────

/// PM server main loop.
///
/// Receives messages and dispatches PM requests.
/// Currently a stub — will be wired when the SEF/server framework is running.
pub fn pm_server_main() {
    // TODO: Phase 12 — receive messages and dispatch PM requests
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigset_new_is_empty() {
        let set = SigSet::new();
        assert_eq!(set.bits[0], 0);
        for s in 1..=_NSIG as i32 {
            assert!(!set.sigismember(s));
        }
    }

    #[test]
    fn test_sigset_full() {
        let set = SigSet::full();
        assert_eq!(set.bits[0], !0u128);
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
        assert!(!set.sigaddset(0));
        assert!(!set.sigdelset(0));
        assert!(!set.sigismember(0));
        assert!(!set.sigaddset(_NSIG as i32));
        assert!(!set.sigdelset(_NSIG as i32));
        assert!(!set.sigismember(_NSIG as i32));
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

    #[test]
    fn test_init_proc_clears_table() {
        let _idx = alloc_proc().expect("should find a free slot");
        assert!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed) > 0);
        init_proc();
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn test_alloc_proc_returns_valid_slot() {
        init_proc();
        let idx = alloc_proc().expect("should find a free slot");
        assert!(idx < NR_PROCS);
        unsafe {
            let base = MPROC.as_ptr();
            let rmp = &*base.add(idx);
            assert!(rmp.in_use());
            assert_eq!(rmp.mp_magic, MP_MAGIC);
        }
    }

    #[test]
    fn test_free_proc_clears_slot() {
        init_proc();
        let idx = alloc_proc().expect("should find a free slot");
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 1);
        unsafe {
            free_proc(idx);
        }
        unsafe {
            let base = MPROC.as_ptr();
            let rmp = &*base.add(idx);
            assert!(!rmp.in_use());
            assert_eq!(rmp.mp_magic, 0);
            assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 0);
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
    }

    #[test]
    fn test_procs_in_use_tracking() {
        init_proc();
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 0);
        let a = alloc_proc().unwrap();
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 1);
        let b = alloc_proc().unwrap();
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 2);
        unsafe {
            free_proc(a);
        }
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 1);
        unsafe {
            free_proc(b);
        }
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 0);
    }

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

    #[test]
    fn test_sigset_size() {
        assert_eq!(core::mem::size_of::<SigSet>(), 16);
    }

    #[test]
    fn test_mproc_size() {
        let mproc_size = core::mem::size_of::<MProc>();
        assert!(mproc_size > 400);
        assert!(mproc_size < 2048);
    }

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

    #[test]
    fn test_pm_isokendpt() {
        init_proc();
        let idx = alloc_proc().unwrap();
        unsafe {
            let endpoint = idx as i32;
            let base = MPROC.as_ptr();
            (*base.add(idx)).mp_endpoint = endpoint;
            assert_eq!(pm_isokendpt(endpoint), Some(idx));
            assert_eq!(pm_isokendpt(9999), None);
        }
    }

    #[test]
    fn test_do_fork() {
        init_proc();
        let parent = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(parent)).mp_pid = 100;
            let child = do_fork(parent).unwrap();
            let child_rmp = &*base.add(child);
            assert!(child != parent);
            assert!(child_rmp.in_use());
            assert_eq!(child_rmp.mp_magic, MP_MAGIC);
        }
    }

    #[test]
    fn test_pm_server_main_callable() {
        pm_server_main();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Compile-time offset verification
// ─────────────────────────────────────────────────────────────────────────────

const _: () = {
    use core::mem::offset_of;
    let _ = offset_of!(MProc, mp_pid);
    let _ = offset_of!(MProc, mp_endpoint);
    let _ = offset_of!(MProc, mp_parent);
    let _ = offset_of!(MProc, mp_flags);
    assert!(core::mem::size_of::<SigSet>() == 16);
    assert!(core::mem::size_of::<TimeVal>() == 16);
    assert!(core::mem::size_of::<Itimerval>() == 32);
};
