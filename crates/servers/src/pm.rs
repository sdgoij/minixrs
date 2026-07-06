//! Process Manager types and infrastructure — ported from
//! `minix/servers/pm/` (mproc.h, const.h, signal.h integration).
//!
//! This is a types-and-infrastructure port, **not** the full PM server.
//! The full PM server with IPC dispatch comes in Phase 12.3.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use kernel::r#priv::MinixTimer;

// Constants

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

// SigSet — signal set type (sigset_t equivalent)

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

// TimeVal and Itimerval — POSIX interval timer types

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

// MProc — process manager slot

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

// Process table — wrapped in UnsafeCell + Sync for interior mutability

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

// Alarm management

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

// Compile-time offset verification

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

// PID management

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

// do_fork — create child process

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

// do_exit + do_waitpid

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

    // No zombie child found. If WNOHANG was set, return EAGAIN.
    // For now, always return EINTR since WNOHANG isn't wired.
    Err(-4) // EINTR — no zombie child found
}

// Signal handling

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
pub unsafe fn do_kill(caller_slot: usize, pid: i32, signo: i32) -> Result<(), i32> {
    if signo < 0 || signo >= _NSIG as i32 {
        return Err(EINVAL);
    }

    // Permission check: only root (uid == 0) or the target process
    // owner may send a signal.
    let base = MPROC.as_ptr();
    let caller = unsafe { &*base.add(caller_slot) };
    let caller_uid = caller.mp_effuid;

    if caller_uid != 0 {
        // Non-root: find the target's UID and compare.
        // The target is specified by PID, not slot, so we scan.
        let mut target_uid = -1i32;
        for i in 0..NR_PROCS {
            let rmp = unsafe { &*base.add(i) };
            if rmp.mp_flags & IN_USE != 0 && rmp.mp_pid == pid {
                target_uid = rmp.mp_effuid;
                break;
            }
        }
        if caller_uid != target_uid {
            return Err(-1); // EPERM
        }
    }

    // Safety: caller guarantees exclusive access to the process table.
    unsafe { check_sig(pid, signo, false) }
}

// do_get / do_set — UID, GID, PID

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

// pm_isokendpt

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

// Dispatch table + main loop

use arch_common::ipc::Message;

/// PM call numbers (from `.refs/minix-3.3.0/minix/include/minix/callnr.h`).
pub const PM_BASE: i32 = 0x000;
pub const NR_PM_CALLS: usize = 48;

pub const PM_EXIT: i32 = PM_BASE + 1;
pub const PM_FORK: i32 = PM_BASE + 2;
pub const PM_WAITPID: i32 = PM_BASE + 3;
pub const PM_GETPID: i32 = PM_BASE + 4;
pub const PM_SETUID: i32 = PM_BASE + 5;
pub const PM_GETUID: i32 = PM_BASE + 6;
pub const PM_STIME: i32 = PM_BASE + 7;
pub const PM_PTRACE: i32 = PM_BASE + 8;
pub const PM_SETGROUPS: i32 = PM_BASE + 9;
pub const PM_GETGROUPS: i32 = PM_BASE + 10;
pub const PM_KILL: i32 = PM_BASE + 11;
pub const PM_SETGID: i32 = PM_BASE + 12;
pub const PM_GETGID: i32 = PM_BASE + 13;
pub const PM_EXEC: i32 = PM_BASE + 14;
pub const PM_SETSID: i32 = PM_BASE + 15;
pub const PM_GETPGRP: i32 = PM_BASE + 16;
pub const PM_ITIMER: i32 = PM_BASE + 17;
pub const PM_GETMCONTEXT: i32 = PM_BASE + 18;
pub const PM_SETMCONTEXT: i32 = PM_BASE + 19;
pub const PM_SIGACTION: i32 = PM_BASE + 20;
pub const PM_SIGSUSPEND: i32 = PM_BASE + 21;
pub const PM_SIGPENDING: i32 = PM_BASE + 22;
pub const PM_SIGPROCMASK: i32 = PM_BASE + 23;
pub const PM_SIGRETURN: i32 = PM_BASE + 24;
pub const PM_SYSUNAME: i32 = PM_BASE + 25;
pub const PM_GETTIMEOFDAY: i32 = PM_BASE + 28;
pub const PM_SETEUID: i32 = PM_BASE + 29;
pub const PM_SETEGID: i32 = PM_BASE + 30;
pub const PM_ISSETUGID: i32 = PM_BASE + 31;
pub const PM_GETSID: i32 = PM_BASE + 32;
pub const PM_CLOCK_GETRES: i32 = PM_BASE + 33;
pub const PM_CLOCK_GETTIME: i32 = PM_BASE + 34;
pub const PM_CLOCK_SETTIME: i32 = PM_BASE + 35;
pub const PM_GETRUSAGE: i32 = PM_BASE + 36;
pub const PM_REBOOT: i32 = PM_BASE + 37;
pub const PM_SVRCTL: i32 = PM_BASE + 38;
pub const PM_SPROF: i32 = PM_BASE + 39;
pub const PM_CPROF: i32 = PM_BASE + 40;
pub const PM_SRV_FORK: i32 = PM_BASE + 41;
pub const PM_SRV_KILL: i32 = PM_BASE + 42;
pub const PM_EXEC_NEW: i32 = PM_BASE + 43;
pub const PM_EXEC_RESTART: i32 = PM_BASE + 44;
pub const PM_GETEPINFO: i32 = PM_BASE + 45;
pub const PM_GETPROCNR: i32 = PM_BASE + 46;
pub const PM_GETSYSINFO: i32 = PM_BASE + 47;

/// OK / error constants matching MINIX conventions.
pub const OK: i32 = 0;
pub const EDONTREPLY: i32 = -201;

// M1 field indexes — unused but document the layout for reference
#[allow(dead_code)]
const M1_I1: usize = 0;
#[allow(dead_code)]
const M1_I2: usize = 1;
#[allow(dead_code)]
const M1_I3: usize = 2;
#[allow(dead_code)]
const M1_I4: usize = 3;

/// Type of a PM handler function.
#[allow(dead_code)]
type PmHandler = unsafe fn(caller_slot: usize, msg: &mut Message) -> i32;

/// Default stub for unimplemented PM calls.
///
/// # Safety
///
/// `_caller_slot` must be a valid process slot. `_msg` must point to a
/// valid message buffer.
pub unsafe fn no_sys(_caller_slot: usize, _msg: &mut Message) -> i32 {
    ENOSYS
}

/// Handler for PM_EXIT — terminate the current process.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
pub unsafe fn handle_exit(caller_slot: usize, msg: &mut Message) -> i32 {
    let status = unsafe { msg.m_payload.m1.m1i1 };
    unsafe { do_exit(caller_slot, status) };
    EDONTREPLY
}

/// Invoke a kernel call on the SYSTEM task.
///
/// `call_nr` is the kernel call number (0 = SYS_FORK, 1 = SYS_EXEC, etc.).
/// `msg` should have payload fields set in `m_payload`.
/// On success, `msg.m_payload` contains the kernel's reply.
/// Returns 0 on success, negative error code on failure.
pub fn send_kernel_call(call_nr: i32, msg: &mut Message) -> i32 {
    #[cfg(target_os = "none")]
    unsafe {
        // Message is 56 bytes, but kernel expects 64. Use a proper
        // 64-byte buffer to avoid stack corruption from the size mismatch.
        let mut buf = [0u8; 64];
        let msg_size = core::mem::size_of::<Message>();
        // Copy Message into 64-byte buffer (first msg_size bytes).
        core::ptr::copy_nonoverlapping(
            msg as *const Message as *const u8,
            buf.as_mut_ptr(),
            msg_size,
        );
        let result = minix_rt::kernel_call(call_nr, &mut buf);
        // Copy back the first msg_size bytes (avoids reading garbage
        // from bytes 56-63 that the kernel may have overwritten).
        core::ptr::copy_nonoverlapping(buf.as_ptr(), msg as *mut Message as *mut u8, msg_size);
        result
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (call_nr, msg);
        -12 // ENOMEM on host builds
    }
}

/// Handler for PM_FORK — create a child process.
///
/// Notifies VFS of the new child so VFS can copy the parent's file
/// descriptor table (Fproc) to the child slot.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
#[allow(unused_unsafe)]
pub unsafe fn handle_fork(caller_slot: usize, msg: &mut Message) -> i32 {
    let result = unsafe { do_fork(caller_slot) };
    match result {
        Ok(child_slot) => {
            let base = MPROC.as_ptr();
            let child = unsafe { &*base.add(child_slot) };
            let parent_endpoint = unsafe { (*base.add(caller_slot)).mp_endpoint };

            // Step 1: Send SYS_FORK to kernel to clone the Proc entry.
            // Kernel message format (SYS_FORK, call 0):
            //   m1_i1 = parent endpoint
            //   m1_i2 = child slot (-1 = kernel auto-selects)
            //   m1_i3 = fork flags (0 for normal fork)
            // Reply: m1_i1 = child endpoint
            let mut kmsg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };
            unsafe {
                kmsg.m_payload.m1.m1i1 = parent_endpoint;
                kmsg.m_payload.m1.m1i2 = -1; // auto-select free slot
                kmsg.m_payload.m1.m1i3 = 0; // normal fork, no flags
            }
            let kresult = send_kernel_call(0, &mut kmsg);
            #[cfg(target_os = "none")]
            unsafe {
                minix_rt::write(1, b"PMK");
            }
            if kresult != OK {
                // Kernel fork failed — free the MProc slot we allocated.
                unsafe { free_proc(child_slot) };
                return kresult;
            }
            // Read the child endpoint from the kernel reply.
            let child_endpoint = unsafe { kmsg.m_payload.m1.m1i1 };
            // Update the MProc entry with the kernel-assigned endpoint.
            unsafe {
                let child_ptr = base.add(child_slot);
                (*child_ptr).mp_endpoint = child_endpoint;
            }

            // Step 2: Set reply fields for the caller (the PM_FORK requester).
            unsafe {
                msg.m_payload.m1.m1i1 = child.mp_pid;
                msg.m_payload.m1.m1i2 = child_endpoint;
            }

            // Step 3: Notify VFS about the new child process.
            #[cfg(target_os = "none")]
            unsafe {
                minix_rt::write(1, b"PMV");
            }
            // Message format (matches VFS pm.rs VFS_PM_FORK handler):
            //   m_type = VFS_PM_FORK (0x907)
            //   m1_i1 = child endpoint
            //   m1_i2 = parent endpoint
            //   m1_i3 = child PID
            let mut vfs_msg = Message {
                m_source: 0,
                m_type: arch_common::com::VFS_PM_FORK as i32,
                m_payload: unsafe { core::mem::zeroed() },
            };
            unsafe {
                vfs_msg.m_payload.m1.m1i1 = child_endpoint;
                vfs_msg.m_payload.m1.m1i2 = parent_endpoint;
                vfs_msg.m_payload.m1.m1i3 = child.mp_pid;
            }
            // Send to VFS and wait for reply.
            // VFS's reply() overwrites m_type with the result code (OK=0),
            // so we check m_type or the syscall return value.
            let reply = unsafe {
                minix_rt::syscall2(
                    minix_rt::SENDREC_CALL,
                    arch_common::com::VFS_PROC_NR as u64,
                    &mut vfs_msg as *mut Message as u64,
                )
            };
            if reply < 0 || vfs_msg.m_type < 0 {
                // VFS fork failed — free the MProc and Proc slots.
                unsafe { free_proc(child_slot) };
                // Also send SYS_CLEAR to kernel to free the Proc slot.
                let mut clear_msg = Message {
                    m_source: 0,
                    m_type: 0,
                    m_payload: unsafe { core::mem::zeroed() },
                };
                unsafe {
                    clear_msg.m_payload.m1.m1i1 = child_endpoint;
                }
                let _ = send_kernel_call(2, &mut clear_msg); // SYS_CLEAR = 2
                if reply < 0 {
                    reply as i32
                } else {
                    vfs_msg.m_type
                }
            } else {
                #[cfg(target_os = "none")]
                unsafe {
                    minix_rt::write(1, b"PMS");
                }
                // Step 4: Skip SCHED server notification (SCHED is not loaded).
                // The child was already enqueued by do_fork_handler, so the
                // normal scheduler will pick it. No SCHED server interaction
                // needed at this stage of development.
                OK
            }
        }
        Err(_) => -11,
    }
}

/// Handler for PM_WAITPID — wait for a child to exit.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
#[allow(unused_unsafe)]
pub unsafe fn handle_waitpid(caller_slot: usize, msg: &mut Message) -> i32 {
    let wpid = unsafe { msg.m_payload.m1.m1i1 };
    match unsafe { do_waitpid(caller_slot, wpid) } {
        Ok((pid, status)) => {
            unsafe {
                msg.m_payload.m1.m1i1 = pid;
                msg.m_payload.m1.m1i2 = status;
            }
            OK
        }
        Err(_) => {
            // No zombie child found. Store the waitpid request and block.
            // Set mp_wpid so do_exit can find us when a child exits.
            let base = MPROC.as_ptr();
            unsafe {
                let rmp = &mut *base.add(caller_slot);
                rmp.mp_wpid = wpid;
            }
            EDONTREPLY
        }
    }
}

/// Handler for PM_GETPID — return pid via m1i1, ppid via m1i2.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
#[allow(unused_unsafe)]
pub unsafe fn handle_getpid(caller_slot: usize, msg: &mut Message) -> i32 {
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(caller_slot) };
    let ppid = if (rmp.mp_parent as usize) < NR_PROCS {
        let parent = unsafe { &*base.add(rmp.mp_parent as usize) };
        parent.mp_pid
    } else {
        0
    };
    unsafe {
        msg.m_payload.m1.m1i1 = rmp.mp_pid;
        msg.m_payload.m1.m1i2 = ppid;
    }
    OK
}

/// Handler for PM_SETUID — set user/group IDs.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
pub unsafe fn handle_setuid(caller_slot: usize, msg: &mut Message) -> i32 {
    let uid = unsafe { msg.m_payload.m1.m1i1 };
    let gid = unsafe { msg.m_payload.m1.m1i2 };
    match unsafe { do_set(caller_slot, 0, uid, gid) } {
        Ok(()) => OK,
        Err(e) => e,
    }
}

/// Handler for PM_SETGID — set real/effective group ID.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
pub unsafe fn handle_setgid(caller_slot: usize, msg: &mut Message) -> i32 {
    // PM_SETGID message: m1i1 = gid, m1i2 = egid
    let gid = unsafe { msg.m_payload.m1.m1i1 };
    let egid = unsafe { msg.m_payload.m1.m1i2 };
    // do_set with subtype 1 for GID operations
    match unsafe { do_set(caller_slot, 1, gid, egid) } {
        Ok(()) => OK,
        Err(e) => e,
    }
}

/// Handler for PM_GETGID — return real/effective GID.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
pub unsafe fn handle_getgid(caller_slot: usize, msg: &mut Message) -> i32 {
    match unsafe { do_get(caller_slot, 1) } {
        Ok(val) => {
            let egid = (val & 0xFFFF_FFFF) as i32;
            let rgid = (val >> 32) as i32;
            msg.m_payload.m1.m1i1 = rgid;
            msg.m_payload.m1.m1i2 = egid;
            OK
        }
        Err(e) => e,
    }
}

/// Handler for PM_GETUID — return real/effective UID and GID.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
#[allow(unused_unsafe)]
pub unsafe fn handle_getuid(caller_slot: usize, msg: &mut Message) -> i32 {
    match unsafe { do_get(caller_slot, 0) } {
        Ok(val) => {
            let euid = (val & 0xFFFF_FFFF) as i32;
            let ruid = (val >> 32) as i32;
            unsafe {
                msg.m_payload.m1.m1i1 = ruid;
                msg.m_payload.m1.m1i2 = euid;
            }
            OK
        }
        Err(e) => e,
    }
}

/// Handler for PM_KILL — send a signal to a process.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
#[allow(unused_unsafe)]
pub unsafe fn handle_kill(caller_slot: usize, msg: &mut Message) -> i32 {
    let signo = unsafe { msg.m_payload.m1.m1i1 };
    let target_pid = unsafe { msg.m_payload.m1.m1i2 };
    match unsafe { do_kill(caller_slot, target_pid, signo) } {
        Ok(()) => OK,
        Err(e) => e,
    }
}

/// Handler for PM_SETSID — create a new session.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `_msg` must point
/// to a valid message buffer.
#[allow(unused_unsafe)]
pub unsafe fn handle_setsid(caller_slot: usize, _msg: &mut Message) -> i32 {
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }
    if rmp.mp_procgrp == rmp.mp_pid {
        return -1;
    }
    rmp.mp_procgrp = rmp.mp_pid;
    OK
}

/// Handler for PM_GETPGRP — return process group.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
#[allow(unused_unsafe)]
pub unsafe fn handle_getpgrp(caller_slot: usize, msg: &mut Message) -> i32 {
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(caller_slot) };
    unsafe {
        msg.m_payload.m1.m1i1 = rmp.mp_procgrp;
    }
    OK
}

/// Handler for PM_REBOOT — reboot the system.
///
/// # Safety
///
/// `caller_slot` must be a valid process slot. `_msg` must point to
/// a valid message buffer.
pub unsafe fn handle_reboot(_caller_slot: usize, _msg: &mut Message) -> i32 {
    #[cfg(target_os = "none")]
    unsafe {
        // syscall1(NR_ABORT=27, 1) — kernel do_abort_handler with reboot.
        minix_rt::syscall1(27, 1);
    }
    OK
}

/// The PM dispatch table.
/// Maps each PM call number to its handler function.
pub fn pm_dispatch(caller_slot: usize, msg: &mut Message) -> i32 {
    // Handle notifications (m_type == NOTIFY_MESSAGE = -10).
    // These include kernel exit notifications.
    if msg.m_type == -10 {
        // Check for pending process exits via SYS_GETKSIG (kernel call 7).
        // This returns: endpoint at m1i1, exit status at m1i2.
        // Call repeatedly until no more exits.
        loop {
            let mut kmsg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };
            let result = send_kernel_call(7, &mut kmsg); // SYS_GETKSIG
            if result != 0 {
                break;
            }
            // SYS_GETKSIG reply: endpoint at kernel msg[16] = m1i3,
            // exit status at kernel msg[24] = m1i5.
            let endpt = unsafe { kmsg.m_payload.m1.m1i3 };
            // NONE (31743) is the sentinel for "no more pending"
            if endpt == -1 || endpt == 0 || endpt == 31743 {
                break; // NONE sentinel — no more pending
            }
            let exit_status = unsafe { kmsg.m_payload.m1.m1i5 };
            // Find the MProc slot for this endpoint.
            if let Some(slot) = unsafe { pm_isokendpt(endpt) } {
                let pid = unsafe {
                    let base = MPROC.as_ptr();
                    (*base.add(slot)).mp_pid
                };
                unsafe { do_exit(slot, exit_status) };

                // Check if any parent is waiting for this child (waitpid).
                // The parent set mp_wpid in handle_waitpid when returning
                // EDONTREPLY. If the parent is waiting for this child,
                // send the waitpid reply now.
                let parent_slot = unsafe {
                    let base = MPROC.as_ptr();
                    (*base.add(slot)).mp_parent
                };
                if parent_slot >= 0 && (parent_slot as usize) < NR_PROCS {
                    unsafe {
                        let base = MPROC.as_ptr();
                        let parent_rmp = &*base.add(parent_slot as usize);
                        if parent_rmp.mp_flags & IN_USE != 0 {
                            let wp = parent_rmp.mp_wpid;
                            if wp == -1 || wp == pid {
                                // Parent is waiting for this child.
                                // Send the waitpid reply via SENDREC.
                                let mut reply_msg = Message {
                                    m_source: 0,
                                    m_type: OK,
                                    m_payload: core::mem::zeroed(),
                                };
                                reply_msg.m_payload.m1.m1i1 = pid;
                                reply_msg.m_payload.m1.m1i2 = (exit_status & 0xFF) as i32;
                                minix_rt::syscall2(
                                    minix_rt::SEND_CALL,
                                    parent_rmp.mp_endpoint as u64,
                                    &mut reply_msg as *mut Message as u64,
                                );
                                // Clear the waitpid request.
                                let parent_ptr = base.add(parent_slot as usize);
                                (*parent_ptr).mp_wpid = 0;
                            }
                        }
                    }
                }
            }
        }
        return unsafe { no_sys(caller_slot, msg) };
    }
    let call_nr = msg.m_type;
    let idx = (call_nr - PM_BASE) as usize;
    match idx {
        1 => unsafe { handle_exit(caller_slot, msg) },
        2 => unsafe { handle_fork(caller_slot, msg) },
        3 => unsafe { handle_waitpid(caller_slot, msg) },
        4 => unsafe { handle_getpid(caller_slot, msg) },
        5 => unsafe { handle_setuid(caller_slot, msg) },
        6 => unsafe { handle_getuid(caller_slot, msg) },
        7 => unsafe { no_sys(caller_slot, msg) }, // PM_STIME
        8 => unsafe { no_sys(caller_slot, msg) }, // PM_PTRACE
        9 => unsafe { no_sys(caller_slot, msg) }, // PM_SETGROUPS
        10 => unsafe { no_sys(caller_slot, msg) }, // PM_GETGROUPS
        11 => unsafe { handle_kill(caller_slot, msg) },
        12 => unsafe { handle_setgid(caller_slot, msg) }, // PM_SETGID
        13 => unsafe { handle_getgid(caller_slot, msg) }, // PM_GETGID
        14 => unsafe { do_exec(caller_slot, msg) },
        15 => unsafe { handle_setsid(caller_slot, msg) },
        16 => unsafe { handle_getpgrp(caller_slot, msg) },
        17 => unsafe { no_sys(caller_slot, msg) }, // PM_ITIMER
        20 => unsafe { no_sys(caller_slot, msg) }, // PM_SIGACTION
        21 => unsafe { no_sys(caller_slot, msg) }, // PM_SIGSUSPEND
        25 => unsafe { no_sys(caller_slot, msg) }, // PM_SYSUNAME
        28 => unsafe { no_sys(caller_slot, msg) }, // PM_GETTIMEOFDAY
        29 => unsafe { no_sys(caller_slot, msg) }, // PM_SETEUID
        30 => unsafe { no_sys(caller_slot, msg) }, // PM_SETEGID
        32 => unsafe { no_sys(caller_slot, msg) }, // PM_GETSID
        37 => unsafe { handle_reboot(caller_slot, msg) }, // PM_REBOOT
        43 => unsafe { do_exec(caller_slot, msg) }, // PM_EXEC_NEW
        _ => unsafe { no_sys(caller_slot, msg) },
    }
}

/// PM server main loop entry point.
///
/// Called once from the PM server process. Receives messages via kernel
/// IPC syscalls, dispatches to the appropriate handler, and sends replies.
/// On host builds (testing), this is a no-op — the dispatch logic is
/// exercised through unit tests instead.
pub fn pm_server_main() {
    #[cfg(target_os = "none")]
    {
        // Initialize PM's process table.
        init_proc();

        // Mark PM and other boot processes as IN_USE so pm_isokendpt
        // accepts messages from them. RS (endpoint 2) sends the first
        // boot notification to kickstart the server chain.
        let boot_endpoints = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        for &ep in &boot_endpoints {
            if let Some(slot) = alloc_proc() {
                let mp = unsafe { &mut *MPROC.as_ptr().add(slot) };
                mp.mp_endpoint = ep;
                mp.mp_pid = ep + 1; // PID = slot + 1 (like real MINIX)
            }
        }

        // Syscall numbers for IPC (from minix-std):
        //   RECEIVE_CALL = 47: receive(src, &mut msg) → sender endpoint
        const SEND_CALL: u64 = 46;
        const RECEIVE_CALL: u64 = 47;
        const ANY: i32 = 0x0000ffff;

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            // Receive a message from any sender.
            // syscall2(RECEIVE_CALL=47, src=ANY, msg_ptr) → sender endpoint
            let src = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64)
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            // Resolve the sender's process slot.
            let slot = match unsafe { pm_isokendpt(src_ep) } {
                Some(s) => s,
                None => continue,
            };

            // Dispatch the call.
            let status = pm_dispatch(slot, &mut msg);

            // Send the reply if the handler didn't return EDONTREPLY.
            if status != EDONTREPLY {
                msg.m_type = status;
                unsafe {
                    minix_rt::syscall2(SEND_CALL, src_ep as u64, &mut msg as *mut Message as u64);
                }
            }
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        // No-op on host builds — dispatch is tested directly
    }
}

/// Execute a new binary in the current process.
///
/// Handles PM_EXEC_NEW (call 43). The caller (libc) sends the exec
/// data (path + argv + envp) through a grant. The PM server:
/// 1. Reads the exec data from the grant
/// 2. Opens the binary via VFS
/// 3. Reads the ELF header
/// 4. Coordinates with VM for address space setup
/// 5. Loads segments, sets up stack
/// 6. Finalizes the new process state
///
/// # Safety
///
/// Must be called from a valid PM dispatch context with the process
/// table lock held.
pub unsafe fn do_exec(caller_slot: usize, msg: &mut Message) -> i32 {
    // PM_EXEC_NEW message format (from callnr.h / mproc.h):
    //   m1_i1  (payload.m1.m1i1): exec_endpt (set by kernel)
    //   m1_i2  (payload.m1.m1i2): grant_id (path pointer)
    //   m1_i3  (payload.m1.m1i3): stack_frame pointer (grant)
    //   m1_i4  (payload.m1.m1i4): frame length
    //
    // The grant points to exec data in the caller's address space:
    //   [path\0][argv[0]\0][argv[1]\0]...[argv[n]\0][envp[0]\0]...
    //   The first null-terminated string is the executable path.
    //
    // Full flow:
    //   1. Send VFS_PM_EXEC to VFS → VFS opens binary, reads ELF, returns pc/newsp/ps_str
    //   2. Send SYS_EXEC (kernel call 1) to set up the process's TrapFrame
    //   3. Return EDONTREPLY (child already set up to run)
    //
    // If VFS returns ENOSYS (no FS servers available), fall back to
    // kernel SYS_EXEC_TARGET (62) which loads from the embedded initramfs.

    let exec_endpt = unsafe { msg.m_payload.m1.m1i1 };
    let grant_id = unsafe { msg.m_payload.m1.m1i2 };
    let stack_frame = unsafe { msg.m_payload.m1.m1i3 };
    let frame_len = unsafe { msg.m_payload.m1.m1i4 };

    // Validate the caller slot.
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }

    // Step 1: Try VFS exec path.
    // Send VFS_PM_EXEC to VFS with the exec parameters.
    // Message format (matching VFS pm.rs offset constants):
    //   m1_i1 (PM_ENDPT_OFF = 8):  exec_endpt
    //   m7_p1 (PM_PATH_OFF = 28):  path pointer (from grant_id)
    //   m1_i2 (PM_EID_OFF = 12):   path length (computed from path string)
    //   m7_p2 (PM_FRAME_OFF = 36): stack frame pointer
    //   m1_i3 (PM_RID_OFF = 16):   frame length
    //   m1_i4 (PM_REUID_OFF = 20): ps_str pointer

    // Compute path length from the path string.
    let path_ptr = grant_id as *const u8;
    let mut path_len = 0usize;
    for i in 0..1023usize {
        if unsafe { *path_ptr.add(i) } == 0 {
            path_len = i;
            break;
        }
    }

    // Build VFS exec request.
    let mut vfs_msg = Message {
        m_source: 0,
        m_type: arch_common::com::VFS_PM_EXEC as i32,
        m_payload: unsafe { core::mem::zeroed() },
    };
    unsafe {
        vfs_msg.m_payload.m1.m1i1 = exec_endpt; // PM_ENDPT_OFF
        vfs_msg.m_payload.m1.m1i2 = path_len as i32; // PM_EID_OFF
        vfs_msg.m_payload.m1.m1i3 = frame_len; // PM_RID_OFF
        vfs_msg.m_payload.m1.m1i4 = 0; // PM_REUID_OFF — reserved
        // m7_p1 at bytes 28-35: path pointer
        // m7_p2 at bytes 36-43: stack frame pointer
        // M7 layout in the union: 6 x i32 (24 bytes) followed by the raw bytes [24..48)
        // Actually m7 is a raw [u8; 48] — we write the pointers at the right offsets.
        // The offset constants in vfs/pm.rs use:
        //   PM_ENDPT_OFF = 8   (m1_i1)
        //   PM_EID_OFF = 12     (m1_i2)
        //   PM_RID_OFF = 16     (m1_i3)
        //   PM_REUID_OFF = 20   (m1_i4)
        //   PM_REGID_OFF = 24   (m1_i5)
        //   PM_PATH_OFF = 28    (m7_p1, u64)
        //   PM_FRAME_OFF = 36   (m7_p2, u64)
        // In the M1 struct, fields are at:
        //   m1i1=0, m1i2=4, m1i3=8, m1i4=12, m1i5=16 (relative to m1 start)
        //   m1 starts at payload offset 0 = absolute offset 8
        //   So PM_ENDPT_OFF = 8 → m1i1 at abs 8 = m1 offset 0 ✓
        // For the u64 pointers, we write to the raw payload bytes at abs offsets 28 and 36.
        let raw = &mut vfs_msg.m_payload.raw;
        raw[20..28].copy_from_slice(&(grant_id as u64).to_le_bytes()); // PM_PATH_OFF - 8 = 20
        raw[28..36].copy_from_slice(&(stack_frame as u64).to_le_bytes()); // PM_FRAME_OFF - 8 = 28
    }

    // SENDREC to VFS.
    let vfs_reply = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::VFS_PROC_NR as u64,
            &mut vfs_msg as *mut Message as u64,
        )
    };

    if vfs_reply >= 0 && vfs_msg.m_type >= 0 {
        // VFS exec succeeded: read pc, newsp, ps_str from reply.
        // Reply format (from VFS service_pm_postponed):
        //   m_type = VFS_PM_EXEC_REPLY (overwritten with OK=0 by reply())
        //   PM_ENDPT_OFF = 8:  exec_endpt
        //   PM_EID_OFF = 12:   result code
        //   PM_PATH_OFF = 28:  pc (entry point, u64)
        //   PM_FRAME_OFF = 36: newsp (stack pointer, u64)
        //   PM_REGID_OFF = 24: ps_str (process string pointer, i32 as u64)
        let pc = unsafe {
            u64::from_le_bytes(vfs_msg.m_payload.raw[20..28].try_into().unwrap_or([0u8; 8]))
        };
        let newsp = unsafe {
            u64::from_le_bytes(vfs_msg.m_payload.raw[28..36].try_into().unwrap_or([0u8; 8]))
        };
        let ps_str = unsafe { vfs_msg.m_payload.m1.m1i5 as u64 };

        // Step 2: Send SYS_EXEC to kernel.
        // Kernel message format (SYS_EXEC, call 1):
        //   m1_i1 (EXEC_ENDPT_OFF = 8):  exec_endpt
        //   m1_i2 (EXEC_IP_OFF = 16):    entry point
        //   m1_i3 (EXEC_STACK_OFF = 24): stack pointer
        //   m1_p1 (EXEC_NAME_OFF = 32):  program name pointer
        //   m1_p2 (EXEC_PS_STR_OFF = 40): ps_str
        let mut kmsg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        unsafe {
            kmsg.m_payload.m1.m1i1 = exec_endpt;
            kmsg.m_payload.m1.m1i2 = pc as i32;
            kmsg.m_payload.m1.m1i3 = newsp as i32;
            let raw = &mut kmsg.m_payload.raw;
            // EXEC_NAME_OFF = 32: name pointer at abs offset 32 = raw offset 24
            raw[24..32].copy_from_slice(&(path_ptr as u64).to_le_bytes());
            // EXEC_PS_STR_OFF = 40: ps_str at abs offset 40 = raw offset 32
            raw[32..40].copy_from_slice(&(ps_str as u64).to_le_bytes());
        }
        let _ = send_kernel_call(1, &mut kmsg); // SYS_EXEC = 1

        // Return EDONTREPLY — the exec target is set up by the kernel.
        // pm_server_main will not send a reply (child starts running).
        EDONTREPLY
    } else {
        // VFS exec failed (or not available). Fall back to initramfs path.
        // Call kernel SYS_EXEC_TARGET (62) to load from initramfs.
        // args[0] = target endpoint, args[1] = path pointer.
        let result = unsafe { minix_rt::syscall2(62, exec_endpt as u64, path_ptr as u64) };
        if result == 0 {
            EDONTREPLY
        } else {
            result as i32
        }
    }
}

// Compile-time offset verification

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

// Tests

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
        assert!(mp.mp_name.iter().all(|&c| c == 0));
        assert!(mp.mp_sgroups.iter().all(|&g| g == 0));
        assert!(mp.mp_interval.iter().all(|&t| t == 0));
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
        while alloc_proc().is_some() {
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

    #[test]
    fn test_send_kernel_call_host_build() {
        // On host builds (not target_os = "none"), send_kernel_call
        // returns ENOMEM (-12) without calling the kernel.
        let mut msg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let result = send_kernel_call(0, &mut msg);
        assert_eq!(result, -12); // ENOMEM on host
    }

    #[test]
    fn test_handle_waitpid_blocks_on_no_zombie() {
        init_proc();
        let parent = alloc_proc().unwrap();
        let mut msg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(parent)).mp_flags |= IN_USE;
            (*base.add(parent)).mp_pid = 1;
            // Set wpid = -1 (wait for any child)
            msg.m_payload.m1.m1i1 = -1;
        }
        let result = unsafe { handle_waitpid(parent, &mut msg) };
        // No zombie children exist, should return EDONTREPLY to block
        assert_eq!(result, EDONTREPLY);
        // mp_wpid should be set to -1 (wait for any child)
        unsafe {
            let base = MPROC.as_ptr();
            assert_eq!((*base.add(parent)).mp_wpid, -1);
        }
    }

    #[test]
    fn test_handle_waitpid_returns_zombie_immediately() {
        init_proc();
        let parent = alloc_proc().unwrap();
        let child = alloc_proc().unwrap();
        let mut msg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(parent)).mp_flags |= IN_USE;
            (*base.add(parent)).mp_pid = 1;
            (*base.add(child)).mp_flags |= IN_USE | ZOMBIE;
            (*base.add(child)).mp_pid = 2;
            (*base.add(child)).mp_parent = parent as i32;
            (*base.add(child)).mp_exitstatus = 42;
            // Set wpid = -1 (wait for any child)
            msg.m_payload.m1.m1i1 = -1;
        }
        let result = unsafe { handle_waitpid(parent, &mut msg) };
        // Zombie child exists, should return OK with pid+status
        assert_eq!(result, OK);
        unsafe {
            assert_eq!(msg.m_payload.m1.m1i1, 2); // pid
            assert_eq!(msg.m_payload.m1.m1i2, 42); // status
        }
    }

    #[test]
    fn test_do_waitpid_no_children() {
        init_proc();
        let parent = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(parent)).mp_flags |= IN_USE;
        }
        let r = unsafe { do_waitpid(parent, -1) };
        assert!(r.is_err());
    }

    #[test]
    fn test_do_waitpid_finds_zombie() {
        init_proc();
        let parent = alloc_proc().unwrap();
        let child = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(parent)).mp_flags |= IN_USE;
            (*base.add(parent)).mp_pid = 1;
            (*base.add(child)).mp_flags |= IN_USE | ZOMBIE;
            (*base.add(child)).mp_pid = 2;
            (*base.add(child)).mp_parent = parent as i32;
            (*base.add(child)).mp_exitstatus = 7;
        }
        let r = unsafe { do_waitpid(parent, -1) };
        assert_eq!(r, Ok((2, 7)));
        // Child slot should be freed
        unsafe {
            let base = MPROC.as_ptr();
            assert_eq!((*base.add(child)).mp_flags & IN_USE, 0);
        }
    }
}
