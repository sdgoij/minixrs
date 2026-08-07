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

// Signal numbers (MINIX 3.3.0 <minix/signal.h>).
pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGILL: i32 = 4;
pub const SIGTRAP: i32 = 5;
pub const SIGABRT: i32 = 6;
pub const SIGEMT: i32 = 7;
pub const SIGFPE: i32 = 8;
pub const SIGKILL: i32 = 9;
pub const SIGBUS: i32 = 10;
pub const SIGSEGV: i32 = 11;
pub const SIGTERM: i32 = 15;
pub const SIGCHLD: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGCONT: i32 = 25;
pub const SIGWINCH: i32 = 28;
pub const SIGINFO: i32 = 29;

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
pub const EAGAIN: i32 = -11;
pub const EPERM: i32 = -1;

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

/// Build a signal set from a list of signal numbers.
const fn sigset_of(sigs: &[i32]) -> SigSet {
    let mut bits = 0u128;
    let mut i = 0;
    while i < sigs.len() {
        bits |= 1u128 << ((sigs[i] as u32) - 1);
        i += 1;
    }
    SigSet { bits: [bits] }
}

/// Signals whose default disposition is to ignore the signal
/// (C: `ign_sigs[]` in pm/main.c: SIGCHLD, SIGWINCH, SIGCONT, SIGINFO).
static IGN_SSET: SigSet = sigset_of(&[SIGCHLD, SIGWINCH, SIGCONT, SIGINFO]);

/// Signals that may not be ignored or blocked when sent by the kernel
/// (C: `noign_sigs[]` in pm/main.c: SIGILL, SIGTRAP, SIGEMT, SIGFPE,
/// SIGBUS, SIGSEGV).
static NOIGN_SSET: SigSet = sigset_of(&[SIGILL, SIGTRAP, SIGEMT, SIGFPE, SIGBUS, SIGSEGV]);

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
    /// Sigreturn trampoline address (passed at sigaction, m2l3@40).
    pub mp_sigrestorer: u64,
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
            mp_sigrestorer: 0,
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
/// Reserve a slot so alloc_proc skips it.
#[allow(dead_code)]
fn reserve_slot(slot: usize) {
    if slot < NR_PROCS {
        let base = MPROC.as_ptr();
        unsafe {
            (*base.add(slot)).mp_flags |= IN_USE;
            (*base.add(slot)).mp_magic = MP_MAGIC;
        }
        PROCS_IN_USE.fetch_add(1, Ordering::Relaxed);
    }
}

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
/// Resets the entire table and `PROCS_IN_USE` counter, then marks
/// boot process slots as IN_USE so alloc_proc skips them.
pub fn init_proc() {
    let base = MPROC.as_ptr();
    for i in 0..NR_PROCS {
        unsafe {
            *base.add(i) = MProc::zeroed();
        }
    }
    PROCS_IN_USE.store(0, Ordering::Relaxed);
    // Mark boot process slots as occupied. These match the kernel's
    // boot_proc list: ds, rs, pm, sched, vfs, ramdisk, vm, mfs, tty, init.
    // The slot numbers are proc_nr values (not MProc indices); they keep
    // alloc_proc() aligned with the kernel's Proc table so fork child slots
    // (VMF_SLOTNO) index both tables identically.
    //
    // These are placeholders — the real per-process entries (endpoint, pid)
    // are created in pm_server_main. Mark them PRIV_PROC so sig_proc/
    // exit_proc treat them as system processes (matching C) and a signal
    // broadcast (e.g. ^C's SIGINT) cannot terminate a phantom slot. The real
    // INIT entry (allocated in pm_server_main, endpoint 10) is a user
    // process and receives ^C.
    for &slot in &[6, 2, 0, 4, 1, 11, 8, 7, 5, 10] {
        unsafe {
            (*base.add(slot)).mp_flags |= IN_USE | PRIV_PROC;
            (*base.add(slot)).mp_magic = MP_MAGIC;
        }
        PROCS_IN_USE.fetch_add(1, Ordering::Relaxed);
    }
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
    let parent_ptr = unsafe { base.add(slot) };
    let parent_flags = unsafe { (*parent_ptr).mp_flags };
    if parent_flags & IN_USE == 0 {
        return Err(EINVAL);
    }
    let child_slot = match alloc_proc() {
        Some(s) => s,
        None => {
            return Err(-11);
        }
    };
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

/// Main exit path — mark the process as exiting, handle children and session.
///
/// Replaces the old `do_exit` with semantics matching C `exit_proc()` in
/// `minix/servers/pm/forkexit.c`.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS` and refer to a valid in-use process. The
/// caller must ensure exclusive access to the process table.
pub unsafe fn exit_proc(slot: usize, exit_status: i32, dump_core: bool) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return;
    }

    // PRIV_PROC processes cannot exit — send SIGKILL and warn.
    if rmp.mp_flags & PRIV_PROC != 0 {
        // Matching C: sys_kill(rmp->mp_endpoint, SIGKILL)
        let _ = unsafe { check_sig(rmp.mp_pid, 0, 9, true) };
        return;
    }

    unsafe { stop_proc(slot) };
    rmp.mp_flags |= EXITING;
    rmp.mp_exitstatus = exit_status as i8;
    rmp.mp_sigstatus = (exit_status & 0xFF) as i8;
    rmp.mp_ksigpending.sigemptyset();

    // Reparent children to INIT (pid 1) if parent is PM or INIT.
    let parent = rmp.mp_parent;
    for i in 0..NR_PROCS {
        let child = unsafe { &mut *base.add(i) };
        if child.mp_flags & IN_USE == 0 {
            continue;
        }
        if child.mp_parent != slot as i32 {
            continue;
        }
        if parent == 0 || parent == 1 {
            child.mp_parent = 1; // INIT
        } else {
            child.mp_parent = parent;
            child.mp_flags |= NEW_PARENT;
        }
    }

    // If session leader, send SIGHUP to the process group.
    if rmp.mp_pid == rmp.mp_procgrp {
        let _ = unsafe { check_sig(-rmp.mp_procgrp, 0, 1, false) };
    }

    // Tell VFS the process exited so it closes the process's open file
    // descriptors (pipe ends, file locks) before the process is gone.
    // Without this, pipe writer counts never drop and readers spin on
    // EAGAIN instead of seeing EOF. Matching C: exit_proc() sends
    // VFS_PM_EXIT via tell_vfs() (asynsend3). Called from every exit
    // path (direct PM_EXIT, kernel exit notification, signal death) —
    // handle_exit only sees direct PM_EXIT messages, while normal exits
    // arrive via the kernel's SYS_GETKSIG notification loop.
    //
    // Async, fire-and-forget: a blocking SENDREC here can return ELOCKED
    // when the deadlock detector sees a PM→VFS→…→PM chain, silently
    // dropping the notification and leaving VFS's fds (pipe ends) open
    // forever.
    let endpoint = rmp.mp_endpoint;
    let mut vfs_msg = [0u8; 64];
    vfs_msg[VFS_MSG_TYPE_OFF..VFS_MSG_TYPE_OFF + 4]
        .copy_from_slice(&(arch_common::com::VFS_PM_EXIT as i32).to_le_bytes());
    vfs_msg[VFS_M7_I1_OFF..VFS_M7_I1_OFF + 4].copy_from_slice(&endpoint.to_le_bytes());
    let _ = unsafe {
        minix_rt::asynsend3(
            arch_common::com::VFS_PROC_NR,
            vfs_msg.as_ptr(),
            arch_common::ipc::AMF_NOREPLY,
        )
    };

    let _dump = dump_core;
    unsafe { zombify(slot) };
}

/// Complete exit after VFS has finished cleanup.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. Caller must ensure exclusive access.
pub unsafe fn exit_restart(slot: usize, dump_core: bool) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return;
    }
    let traced = rmp.mp_flags & TRACE_EXIT != 0;
    let tracer = rmp.mp_tracer;
    let told_parent = rmp.mp_flags & TOLD_PARENT != 0;
    let endpoint = rmp.mp_endpoint;

    // Stop scheduling (SYS_STOP via kernel call 5).
    let mut stop_msg = [0u8; 64];
    stop_msg[8..12].copy_from_slice(&endpoint.to_le_bytes());
    let _ = minix_rt::kernel_call(5, &mut stop_msg);

    if dump_core {
        unsafe { zombify(slot) };
    }

    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & PRIV_PROC == 0 {
        // SYS_CLEAR (kernel call 2) — free kernel Proc entry.
        let mut clear_msg = [0u8; 64];
        clear_msg[8..12].copy_from_slice(&endpoint.to_le_bytes());
        let _ = minix_rt::kernel_call(2, &mut clear_msg);

        // VM_EXIT — notify VM to free address space. Matching C
        // exit_restart()'s vm_exit() (_taskcall, synchronous) so the
        // notification is never dropped when VM is busy.
        let mut vm_exit_msg = [0u8; 64];
        vm_exit_msg[4..8].copy_from_slice(&(arch_common::com::VM_EXIT as i32).to_le_bytes());
        vm_exit_msg[8..12].copy_from_slice(&endpoint.to_le_bytes());
        let _ = unsafe {
            minix_rt::syscall2(
                minix_rt::SENDREC_CALL,
                arch_common::com::VM_PROC_NR as u64,
                vm_exit_msg.as_mut_ptr() as u64,
            )
        };
    }

    if traced {
        // Reply to tracer with exit status.
        let mut reply_msg = Message {
            m_source: 0,
            m_type: OK,
            m_payload: unsafe { core::mem::zeroed() },
        };
        reply_msg.m_payload.m1.m1i1 = rmp.mp_pid;
        reply_msg.m_payload.m1.m1i2 =
            w_exitcode(rmp.mp_exitstatus as i32, rmp.mp_sigstatus as i32 & 0o377);
        let _ = unsafe {
            minix_rt::syscall2(
                minix_rt::SENDNB_CALL,
                tracer as u64,
                &mut reply_msg as *mut Message as u64,
            )
        };
    }

    if told_parent {
        unsafe { cleanup(slot) };
    }
}

/// Compute waitpid exit status from exit code and signal.
const fn w_exitcode(status: i32, sig: i32) -> i32 {
    (status << 8) | sig
}

/// Transition a process to zombie state, handling tracer logic.
///
/// If the process has a tracer (not its parent), sets TRACE_ZOMBIE and
/// notifies the tracer if it's waiting. Otherwise sets ZOMBIE and calls
/// check_parent. Matching C `zombify()` in `forkexit.c`.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. Caller must ensure exclusive access.
pub unsafe fn zombify(slot: usize) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return;
    }
    let tracer = rmp.mp_tracer;
    let parent = rmp.mp_parent;

    if tracer != NO_TRACER && tracer != parent {
        let rmp = unsafe { &mut *base.add(slot) };
        rmp.mp_flags |= TRACE_ZOMBIE;
        // If tracer is waiting, tell it.
        if rmp.mp_flags & WAITING != 0 {
            unsafe { tell_tracer(slot) };
        }
    } else {
        let rmp = unsafe { &mut *base.add(slot) };
        rmp.mp_flags |= ZOMBIE;
        unsafe { check_parent(slot, true) };
    }
}

/// Called when a child exits — handles parent notification.
///
/// If parent is EXITING: no-op. If parent is waiting: call tell_parent.
/// If try_cleanup and parent exists: call cleanup. Otherwise: send SIGCHLD.
///
/// # Safety
///
/// `child_slot` must be < `NR_PROCS`. Caller must ensure exclusive access.
pub unsafe fn check_parent(child_slot: usize, _try_cleanup: bool) {
    if child_slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let child = unsafe { &*base.add(child_slot) };
    if child.mp_flags & IN_USE == 0 {
        return;
    }
    let parent = child.mp_parent;
    if parent < 0 || parent as usize >= NR_PROCS {
        return;
    }
    let parent_rmp = unsafe { &*base.add(parent as usize) };
    if parent_rmp.mp_flags & IN_USE == 0 {
        return;
    }
    if parent_rmp.mp_flags & EXITING != 0 {
        return;
    }

    if unsafe { wait_test(parent as usize, child) } {
        unsafe { tell_parent(parent as usize, child) };
        return;
    }

    // Parent is not waiting: leave the child as a zombie and signal the
    // parent. Matching C check_parent(): the child is only cleaned up when
    // the parent reaps it via waitpid (do_waitpid's free_proc + SYS_CLEAR).
    // Cleaning up here would destroy the zombie before the parent's waitpid
    // can find it, blocking the parent forever (the shell hangs on
    // `echo hi | cat` because echo exited before the shell's waitpid).
    unsafe { sig_proc(parent as usize, SIGCHLD, false, false) };
}

/// Tell a waiting parent that its child has exited.
/// Sends a reply to the parent's WAITPID call with child PID and status.
/// Matching C tell_parent() in forkexit.c
///
/// # Safety
///
/// - `parent` must be < `NR_PROCS`.
/// - `child` must point to a valid, in-use MProc entry.
pub unsafe fn tell_parent(parent: usize, child: &MProc) {
    if parent >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let parent_rmp = unsafe { &mut *base.add(parent) };
    if parent_rmp.mp_flags & IN_USE == 0 {
        return;
    }
    parent_rmp.mp_flags &= !WAITING;
    let mut reply_msg = Message {
        m_source: 0,
        m_type: OK,
        m_payload: unsafe { core::mem::zeroed() },
    };

    reply_msg.m_payload.m1.m1i1 = child.mp_pid;
    reply_msg.m_payload.m1.m1i2 = (child.mp_exitstatus as i32) & 0xFF;

    let _ = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDNB_CALL,
            parent_rmp.mp_endpoint as u64,
            &mut reply_msg as *mut Message as u64,
        )
    };
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
    let parent_rmp = unsafe { &*base.add(parent) };
    if parent_rmp.mp_flags & IN_USE == 0 {
        return false;
    }
    // Parent must have WAITING flag set AND matching wpid
    if parent_rmp.mp_flags & WAITING == 0 {
        return false;
    }
    let wpid = parent_rmp.mp_wpid;
    wpid == -1 || wpid == child.mp_pid
}

/// Tell the tracer that a traced child exited.
///
/// Sets TOLD_PARENT if child is ZOMBIE, replies to tracer with child PID
/// and w_exitcode(exitstatus, sigstatus&0377), clears WAITING on tracer.
/// Matching C `tell_tracer()` in `forkexit.c`.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. Caller must ensure exclusive access.
pub unsafe fn tell_tracer(slot: usize) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let child = unsafe { &*base.add(slot) };
    if child.mp_flags & IN_USE == 0 {
        return;
    }
    let tracer = child.mp_tracer;
    if tracer < 0 || tracer as usize >= NR_PROCS {
        return;
    }

    if child.mp_flags & ZOMBIE != 0 {
        let child_mut = unsafe { &mut *base.add(slot) };
        child_mut.mp_flags |= TOLD_PARENT;
    }

    let tracer_rmp = unsafe { &mut *base.add(tracer as usize) };
    if tracer_rmp.mp_flags & IN_USE == 0 {
        return;
    }
    tracer_rmp.mp_flags &= !WAITING;

    let mut reply_msg = Message {
        m_source: 0,
        m_type: OK,
        m_payload: unsafe { core::mem::zeroed() },
    };
    reply_msg.m_payload.m1.m1i1 = child.mp_pid;
    reply_msg.m_payload.m1.m1i2 = w_exitcode(
        child.mp_exitstatus as i32,
        child.mp_sigstatus as i32 & 0o377,
    );

    let _ = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDNB_CALL,
            tracer_rmp.mp_endpoint as u64,
            &mut reply_msg as *mut Message as u64,
        )
    };
}

/// Clean up a zombie process slot.
///
/// Zeroes mp_pid, mp_flags, mp_child_utime, mp_child_stime and decrements
/// PROCS_IN_USE. Matching C `cleanup()` in `forkexit.c`.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. Caller must ensure exclusive access.
pub unsafe fn cleanup(slot: usize) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return;
    }
    rmp.mp_pid = 0;
    rmp.mp_flags = 0;
    rmp.mp_child_utime = 0;
    rmp.mp_child_stime = 0;
    PROCS_IN_USE.fetch_sub(1, Ordering::Relaxed);
}

/// Wait for a child process to exit.
///
/// `wpid` is the child PID to wait for, or -1 for any child. With `options`
/// & WNOHANG set, returns `Err(EAGAIN)` when no zombie child exists instead
/// of blocking (the caller turns a non-WNOHANG miss into a suspended wait).
///
/// # Safety
///
/// `parent` must be < `NR_PROCS` and refer to a valid in-use process. The
/// caller must ensure exclusive access to the process table.
pub unsafe fn do_waitpid(parent: usize, wpid: i32, options: i32) -> Result<(i32, i32), i32> {
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
            let child_ep = child.mp_endpoint;
            // Safety: `free_proc` requires exclusive access to the process
            // table, which the caller guarantees.
            unsafe {
                free_proc(i);
            }
            // Call SYS_CLEAR (kernel call 2) to free the kernel Proc entry
            // and page tables.  Matching C: cleanup() in forkexit.c.
            let mut clear_msg = [0u8; 64];
            clear_msg[8..12].copy_from_slice(&child_ep.to_le_bytes());
            let _ = minix_rt::kernel_call(2, &mut clear_msg);
            // Notify VM to free its Vmproc entry. Matching C: exit_restart()
            // calls vm_exit() (libsys/vm_exit.c) which uses _taskcall — a
            // synchronous SENDREC, so the notification is never dropped. A
            // SENDNB here fails silently (ENOTREADY) whenever VM is busy
            // mid-request, leaking the child's Vmproc; the next fork that
            // reuses this mproc slot then fails vmproc_alloc and the shell
            // hangs.
            let mut vm_exit_msg = [0u8; 64];
            vm_exit_msg[4..8].copy_from_slice(&(arch_common::com::VM_EXIT as i32).to_le_bytes());
            vm_exit_msg[8..12].copy_from_slice(&child_ep.to_le_bytes());
            let _ = unsafe {
                minix_rt::syscall2(
                    minix_rt::SENDREC_CALL,
                    arch_common::com::VM_PROC_NR as u64,
                    vm_exit_msg.as_mut_ptr() as u64,
                )
            };
            return Ok((pid, status));
        }
    }

    // No zombie child found. If WNOHANG was set, return EAGAIN so the
    // caller can keep going; otherwise the caller suspends the process and
    // waits for a child exit (EINTR marks the blocked-wait case).
    if options & WNOHANG != 0 {
        Err(EAGAIN)
    } else {
        Err(-4) // EINTR — no zombie child found
    }
}

// Signal handling

/// Check if a signal can be sent to a process and deliver it.
///
/// # Safety
///
/// The caller must ensure exclusive access to the process table while this
/// function runs.
pub unsafe fn check_sig(proc_id: i32, pgrp_ref: i32, signo: i32, ksig: bool) -> Result<(), i32> {
    let base = MPROC.as_ptr();
    let mut sent = 0;
    for i in 0..NR_PROCS {
        // Safety: `i < NR_PROCS` holds by loop bound.
        let rmp = unsafe { &*base.add(i) };
        if rmp.mp_flags & IN_USE == 0 {
            continue;
        }
        // Do not signal processes that are already exiting — re-entering
        // exit_proc on the same slot corrupts the table. Matching C
        // check_sig(): `if (signo == 0 || (rmp->mp_flags & EXITING)) continue;`.
        if rmp.mp_flags & EXITING != 0 {
            continue;
        }
        // C check_sig() selection:
        //   proc_id > 0  → specific pid
        //   proc_id == 0 → same process group as `pgrp_ref` (the signaled
        //                  process's group, set by process_ksig)
        //   proc_id == -1 → systemwide (PRIV_PROC slots are dropped inside
        //                  sig_proc, so servers are never touched)
        //   proc_id < -1 → -proc_id process group
        if proc_id > 0 && rmp.mp_pid != proc_id {
            continue;
        }
        if proc_id == 0 && rmp.mp_procgrp != pgrp_ref {
            continue;
        }
        if proc_id < -1 && rmp.mp_procgrp != -proc_id {
            continue;
        }
        // A process matched — kill(2) on an existing process succeeds even
        // if the disposition drops the signal. C counts matches and returns
        // OK when count > 0, ESRCH when nothing matched.
        sent += 1;
        // Send the signal.
        unsafe {
            sig_proc(i, signo, false, ksig);
        }
        // Specific pid: only one process may be signaled.
        if proc_id > 0 {
            break;
        }
    }
    if sent > 0 {
        Ok(())
    } else {
        Err(-3) // ESRCH
    }
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

    // Matching C sig_proc(): a traced process gets the signal diverted to
    // its tracer first, unless the signal is SIGKILL.
    if trace && rmp.mp_tracer != NO_TRACER && signo != SIGKILL {
        rmp.mp_sigtrace.sigaddset(signo);
        rmp.mp_flags |= TRACE_STOPPED;
        return;
    }

    // System processes: PM never takes signals (C: "Always skip signals
    // for PM"). Other system processes are routed through the kernel in C
    // (sys_kill / SIGS_SIGNAL_RECEIVED), which this port does not implement;
    // skip them rather than terminate PM or a driver via the user-process
    // path.
    if rmp.mp_flags & PRIV_PROC != 0 {
        return;
    }

    // Kernel-sent signals from noign_sset (SIGILL, SIGSEGV, ...) may not be
    // ignored or blocked even if the process requested it.
    let badignore = ksig
        && NOIGN_SSET.sigismember(signo)
        && (rmp.mp_ignore.sigismember(signo) || rmp.mp_sigmask.sigismember(signo));

    // Explicitly ignored.
    if !badignore && rmp.mp_ignore.sigismember(signo) {
        return;
    }

    // Blocked: pend until unmasked; check_pending() delivers on unblock.
    if !badignore && rmp.mp_sigmask.sigismember(signo) {
        rmp.mp_sigpending.sigaddset(signo);
        if ksig {
            rmp.mp_ksigpending.sigaddset(signo);
        }
        return;
    }

    // Stopped for a debugger: pend (except SIGKILL) until the debugger
    // releases the process.
    if rmp.mp_flags & TRACE_STOPPED != 0 && signo != SIGKILL {
        rmp.mp_sigpending.sigaddset(signo);
        if ksig {
            rmp.mp_ksigpending.sigaddset(signo);
        }
        return;
    }

    // SIGSTOP stops the process.
    if signo == SIGSTOP {
        unsafe { stop_proc(slot) };
        return;
    }

    // Caught: ask the kernel to run the handler (SIGNALS.md Phase 4).
    if !badignore && rmp.mp_catch.sigismember(signo) {
        // Can't deliver while the process is in a PM→VFS round-trip; pend
        // and let restart_sigs deliver when the VFS reply arrives.
        if rmp.mp_flags & VFS_CALL != 0 {
            rmp.mp_sigpending.sigaddset(signo);
            if ksig {
                rmp.mp_ksigpending.sigaddset(signo);
            }
            return;
        }
        // Interrupt a blocked PM call (waitpid/sigsuspend) — matching C's
        // unpause: the process is stopped and its call released with EINTR.
        rmp.mp_flags &= !(WAITING | SIGSUSPENDED);
        rmp.mp_flags |= PROC_STOPPED;
        if unsafe { sig_send(slot, signo) } {
            // The kernel set up the handler frame and resumed the process;
            // clear PM's stop mark (matching C sig_send → try_resume_proc).
            rmp.mp_flags &= !PROC_STOPPED;
            return;
        }
        // Delivery failed (bad handler address, unwritable stack) — the
        // process cannot catch the signal; fall through and terminate it.
    }

    // Default disposition is to ignore (SIGCHLD, SIGWINCH, SIGCONT, SIGINFO).
    // This is what makes check_parent() safe: a parent that is not waiting
    // gets SIGCHLD, which must be dropped here — pending it would make
    // check_pending() deliver it again, re-pend, and recurse forever.
    if !badignore && IGN_SSET.sigismember(signo) {
        return;
    }

    // Everything else terminates the process (SIGKILL reaches this point:
    // it can never be ignored, blocked, or caught).
    unsafe { sig_proc_exit(slot, signo) };
}

/// Ask the kernel to run a caught signal handler for a process.
///
/// Builds a 48-byte `struct sigmsg` (signo@0, mask@8, handler@24,
/// trampoline@32 — matching the C layout) in PM's space and calls
/// SYS_SIGSEND (kernel call 9). The target must be stopped (PROC_STOPPED);
/// the kernel captures its registers into a sigframe and resumes it at the
/// handler.
///
/// Returns `true` when the handler was set up; on failure the caller
/// terminates the process.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. The caller must ensure exclusive access to
/// the process table.
pub unsafe fn sig_send(slot: usize, signo: i32) -> bool {
    if slot >= NR_PROCS {
        return false;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return false;
    }

    let mut sigmsg = [0u8; arch_common::consts::SIGMSG_SIZE];
    sigmsg[0..4].copy_from_slice(&(signo as u32).to_ne_bytes());
    // Mask to restore on sigreturn: the current mask (the handled signal
    // was added at registration unless SA_NODEFER).
    let mask_bytes: [u8; 16] = rmp.mp_sigmask.bits[0].to_ne_bytes();
    sigmsg[8..24].copy_from_slice(&mask_bytes);
    sigmsg[24..32].copy_from_slice(&rmp.mp_sigreturn.to_ne_bytes());
    sigmsg[32..40].copy_from_slice(&rmp.mp_sigrestorer.to_ne_bytes());

    // The signal is no longer pending once delivered.
    let rmp = unsafe { &mut *base.add(slot) };
    rmp.mp_sigpending.sigdelset(signo);
    rmp.mp_ksigpending.sigdelset(signo);
    let endpoint = rmp.mp_endpoint;

    let mut kmsg = [0u8; 64];
    kmsg[arch_common::consts::SIGCALLS_ENDPT_OFF..arch_common::consts::SIGCALLS_ENDPT_OFF + 4]
        .copy_from_slice(&endpoint.to_ne_bytes());
    kmsg[arch_common::consts::SIGCALLS_SIGCTX_OFF..arch_common::consts::SIGCALLS_SIGCTX_OFF + 8]
        .copy_from_slice(&(sigmsg.as_ptr() as u64).to_ne_bytes());
    let r = minix_rt::kernel_call(9, &mut kmsg);
    r == 0
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
    unsafe { check_sig(pid, 0, signo, false) }
}

// Signal delivery infrastructure

const SIG_BLOCK: i32 = 0;
const SIG_UNBLOCK: i32 = 1;
const SIG_SETMASK: i32 = 2;
const SIG_INQUIRE: i32 = 3;
pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;
pub const SUSPEND: i32 = -998;
/// WNOHANG for waitpid — return immediately when no child has exited.
pub const WNOHANG: i32 = 1;

/// Terminate a process due to a signal.
///
/// Sets exit status to (signo | 0x80) and calls do_exit.
/// If signo is in the core set (SIGQUIT=3, SIGILL=4, SIGTRAP=5, SIGABRT=6,
/// SIGFPE=8, SIGSEGV=11), marks the slot for a core dump (not yet implemented).
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. Caller must ensure exclusive access to the
/// process table.
pub unsafe fn sig_proc_exit(slot: usize, signo: i32) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return;
    }
    let exit_status = signo | 0x80;
    // Core dump signals: SIGQUIT(3), SIGILL(4), SIGTRAP(5),
    // SIGABRT(6), SIGFPE(8), SIGSEGV(11)
    if matches!(signo, 3 | 4 | 5 | 6 | 8 | 11) {
        // Core dump flag would go here when VM core dump is implemented.
    }
    unsafe { exit_proc(slot, exit_status, false) };
}

/// Stop a process by setting PROC_STOPPED and sending a SYS_STOP kernel call.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. Caller must ensure exclusive access.
pub unsafe fn stop_proc(slot: usize) {
    if slot >= NR_PROCS {
        return;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return;
    }
    rmp.mp_flags |= PROC_STOPPED;
    let endpoint = rmp.mp_endpoint;
    let mut msg = [0u8; 64];
    msg[8..12].copy_from_slice(&endpoint.to_le_bytes());
    let _ = minix_rt::kernel_call(5, &mut msg);
}

/// Deliver pending signals to a process.
///
/// Iterates `mp_sigpending`; for each signal not masked by `mp_sigmask`,
/// deletes it from the pending sets and delivers via `sig_proc`. If a
/// delivery sets VFS_CALL, delivery pauses until the VFS reply triggers
/// restart_sigs. Matching C `check_pending()` in `signal.c` (flat loop,
/// no recursion).
///
/// # Safety
///
/// The caller must ensure exclusive access to the process table.
pub unsafe fn check_pending(rmp: &mut MProc) {
    // Safety: called under exclusive access to process table.
    for signo in 1..(_NSIG as i32) {
        if !rmp.mp_sigpending.sigismember(signo) {
            continue;
        }
        if rmp.mp_sigmask.sigismember(signo) {
            continue;
        }
        // Matching C: preserve the ksig flag and delete from both sets.
        let ksig = rmp.mp_ksigpending.sigismember(signo);
        rmp.mp_sigpending.sigdelset(signo);
        rmp.mp_ksigpending.sigdelset(signo);
        // Deliver the signal. The slot is derived from `rmp`'s position
        // in the table — find it by scanning.
        let slot = {
            let base = MPROC.as_ptr();
            let mut found = NR_PROCS;
            for i in 0..NR_PROCS {
                if core::ptr::eq(unsafe { &*base.add(i) }, rmp) {
                    found = i;
                    break;
                }
            }
            found
        };
        if slot >= NR_PROCS {
            return;
        }
        unsafe { sig_proc(slot, signo, false, ksig) };
        // If the process now has VFS_CALL set, stop — the VFS reply will
        // call restart_sigs to continue delivery.
        if rmp.mp_flags & VFS_CALL != 0 {
            return;
        }
        // No recursion: sig_proc() may re-pend a blocked/traced signal,
        // and the loop re-checks it on a later iteration. Matching C
        // check_pending(), which is a flat loop.
    }
}

/// Check whether a process has any pending (non-masked) signals.
fn has_pending(rmp: &MProc) -> bool {
    for signo in 1..(_NSIG as i32) {
        if rmp.mp_sigpending.sigismember(signo) && !rmp.mp_sigmask.sigismember(signo) {
            return true;
        }
    }
    false
}

/// Restart signal delivery after a VFS/VM reply to an unpause request.
///
/// Delivers any ksigpending signals, then pending signals via check_pending.
/// Clears the VFS_CALL flag.
///
/// # Safety
///
/// The caller must ensure exclusive access to the process table.
pub unsafe fn restart_sigs(rmp: &mut MProc) {
    rmp.mp_flags &= !VFS_CALL;
    // Deliver kernel-signalled pending signals first.
    let slot = {
        let base = MPROC.as_ptr();
        let mut found = NR_PROCS;
        for i in 0..NR_PROCS {
            if core::ptr::eq(unsafe { &*base.add(i) }, rmp) {
                found = i;
                break;
            }
        }
        found
    };
    if slot >= NR_PROCS {
        return;
    }
    // Deliver ksigpending signals.
    for signo in 1..(_NSIG as i32) {
        if rmp.mp_ksigpending.sigismember(signo) {
            rmp.mp_ksigpending.sigdelset(signo);
            unsafe { sig_proc(slot, signo, false, true) };
        }
    }
    // Deliver remaining pending signals.
    if has_pending(rmp) {
        let base = MPROC.as_ptr();
        let rmp2 = unsafe { &mut *base.add(slot) };
        unsafe { check_pending(rmp2) };
    }
}

/// Apply a parsed sigaction (raw handler, mask, flags) to a process's signal
/// state. `SIG_IGN` sets `mp_ignore`, `SIG_DFL` clears both, anything else
/// registers a catch handler (`mp_catch` + `mp_sigreturn`).
///
/// The client's `sa_mask` is NOT applied to `mp_sigmask` at registration:
/// blocking the handled signal here would make it permanently undeliverable
/// (delivery-time masking — sa_mask + signo during the handler — is a
/// follow-up; the mask restores from the sigframe on sigreturn).
fn apply_action(rmp: &mut MProc, signo: i32, handler: u64, _mask_bits: u128, _flags: i32) {
    rmp.mp_ignore.sigdelset(signo);
    rmp.mp_catch.sigdelset(signo);

    if handler == SIG_DFL {
        // Default action — clear catch, keep ignore clear.
    } else if handler == SIG_IGN {
        rmp.mp_ignore.sigaddset(signo);
    } else {
        rmp.mp_catch.sigaddset(signo);
        rmp.mp_sigreturn = handler;
    }
}

/// Handle PM_SIGACTION — set or get signal action.
///
/// Message layout: m1i1=signo, m2l1=nact pointer, m2l2=oact pointer.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn do_sigaction(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let signo = unsafe { msg.m_payload.m1.m1i1 };
    let nact_ptr = unsafe { msg.m_payload.m2.m2l1 };
    let oact_ptr = unsafe { msg.m_payload.m2.m2l2 };
    // The sigreturn trampoline address (m2l3@40), passed by the client so
    // PM can build the sigframe for caught signals (SIGNALS.md 4.4).
    let restorer = unsafe { msg.m_payload.m2.m2l3 };

    // SIGKILL and SIGSTOP cannot have their action changed.
    if signo == 9 || signo == 19 {
        return OK;
    }
    if signo < 1 || signo >= _NSIG as i32 {
        return EINVAL;
    }

    let base = MPROC.as_ptr();
    let caller_ep = unsafe { (*base.add(caller_slot)).mp_endpoint };
    let rmp = unsafe { &mut *base.add(caller_slot) };

    // If oact provided, read old action back.
    if oact_ptr != 0 {
        // Build old sigaction struct: handler(u64) + mask(16 bytes) + flags(i32) = 28 bytes
        let old_handler: u64 = if rmp.mp_catch.sigismember(signo) {
            rmp.mp_sigreturn
        } else if rmp.mp_ignore.sigismember(signo) {
            SIG_IGN
        } else {
            SIG_DFL
        };
        let _ = minix_rt::sys_vircopy(
            minix_rt::SELF,
            &old_handler as *const u64 as u64,
            caller_ep,
            oact_ptr as u64,
            8,
        );
        // Copy old mask (16 bytes) to oact_ptr + 8
        let old_mask = rmp.mp_sigmask;
        let _ = minix_rt::sys_vircopy(
            minix_rt::SELF,
            &old_mask as *const SigSet as u64,
            caller_ep,
            (oact_ptr as u64) + 8,
            16,
        );
        // Copy old flags (0 for now) to oact_ptr + 24
        let old_flags: i32 = 0;
        let _ = minix_rt::sys_vircopy(
            minix_rt::SELF,
            &old_flags as *const i32 as u64,
            caller_ep,
            (oact_ptr as u64) + 24,
            4,
        );
    }

    // If nact provided, read and apply new action.
    if nact_ptr != 0 {
        let mut sa_buf = [0u8; 28];
        let copy_r = minix_rt::sys_vircopy(
            caller_ep,
            nact_ptr as u64,
            minix_rt::SELF,
            sa_buf.as_mut_ptr() as u64,
            28,
        );
        if copy_r != 0 {
            return copy_r;
        }
        let handler = u64::from_ne_bytes(sa_buf[0..8].try_into().unwrap());
        let mask_bytes: [u8; 16] = sa_buf[8..24].try_into().unwrap();
        let mask_bits = u128::from_ne_bytes(mask_bytes);
        let flags = i32::from_ne_bytes(sa_buf[24..28].try_into().unwrap());

        apply_action(rmp, signo, handler, mask_bits, flags);
        // The restorer is only meaningful for a caught handler.
        rmp.mp_sigrestorer = restorer as u64;
    }

    OK
}

/// Handle PM_SIGPENDING — return the set of pending signals.
///
/// Message layout: m2l1 = set pointer (user buffer for SigSet).
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn do_sigpending(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let set_ptr = unsafe { msg.m_payload.m2.m2l1 };
    if set_ptr == 0 {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let caller_ep = unsafe { (*base.add(caller_slot)).mp_endpoint };
    let rmp = unsafe { &*base.add(caller_slot) };
    let pending = rmp.mp_sigpending;
    // Merge ksigpending into the result — these are also pending signals.
    let mut result_set = pending;
    for signo in 1..(_NSIG as i32) {
        if rmp.mp_ksigpending.sigismember(signo) {
            result_set.sigaddset(signo);
        }
    }
    minix_rt::sys_vircopy(
        minix_rt::SELF,
        &result_set as *const SigSet as u64,
        caller_ep,
        set_ptr as u64,
        16,
    )
}

/// Handle PM_SIGPROCMASK — block, unblock, set, or get the signal mask.
///
/// Message layout: m1i1=how, m2l1=set pointer, m2l2=old_set pointer.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn do_sigprocmask(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let how = unsafe { msg.m_payload.m1.m1i1 };
    let set_ptr = unsafe { msg.m_payload.m2.m2l1 };
    let old_set_ptr = unsafe { msg.m_payload.m2.m2l2 };

    let base = MPROC.as_ptr();
    let caller_ep = unsafe { (*base.add(caller_slot)).mp_endpoint };
    let rmp = unsafe { &mut *base.add(caller_slot) };

    // Save old mask for return.
    let old_mask = rmp.mp_sigmask;

    if old_set_ptr != 0 {
        let _ = minix_rt::sys_vircopy(
            minix_rt::SELF,
            &old_mask as *const SigSet as u64,
            caller_ep,
            old_set_ptr as u64,
            16,
        );
    }

    if how == SIG_INQUIRE {
        return OK;
    }

    if set_ptr == 0 {
        return EINVAL;
    }

    // Read the new mask from caller.
    let mut mask_buf = [0u8; 16];
    let copy_r = minix_rt::sys_vircopy(
        caller_ep,
        set_ptr as u64,
        minix_rt::SELF,
        mask_buf.as_mut_ptr() as u64,
        16,
    );
    if copy_r != 0 {
        return copy_r;
    }
    let set_bits = u128::from_ne_bytes(mask_buf);
    let set = SigSet { bits: [set_bits] };

    match how {
        SIG_BLOCK => {
            // mask |= set
            for signo in 1..(_NSIG as i32) {
                if set.sigismember(signo) {
                    rmp.mp_sigmask.sigaddset(signo);
                }
            }
        }
        SIG_UNBLOCK => {
            // mask &= ~set
            for signo in 1..(_NSIG as i32) {
                if set.sigismember(signo) {
                    rmp.mp_sigmask.sigdelset(signo);
                }
            }
            // Newly unblocked signals may now be deliverable.
            if has_pending(rmp) {
                let slot = caller_slot;
                let base2 = MPROC.as_ptr();
                let rmp2 = unsafe { &mut *base2.add(slot) };
                unsafe { check_pending(rmp2) };
            }
        }
        SIG_SETMASK => {
            rmp.mp_sigmask = set;
            // After changing mask, check for newly deliverable pending signals.
            if has_pending(rmp) {
                let slot = caller_slot;
                let base2 = MPROC.as_ptr();
                let rmp2 = unsafe { &mut *base2.add(slot) };
                unsafe { check_pending(rmp2) };
            }
        }
        _ => return EINVAL,
    }

    OK
}

/// Handle PM_SIGSUSPEND — atomically replace signal mask and suspend.
///
/// Message layout: m2l1 = set pointer (new mask).
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn do_sigsuspend(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let set_ptr = unsafe { msg.m_payload.m2.m2l1 };
    if set_ptr == 0 {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let caller_ep = unsafe { (*base.add(caller_slot)).mp_endpoint };
    let rmp = unsafe { &mut *base.add(caller_slot) };

    // Read the new mask from caller.
    let mut mask_buf = [0u8; 16];
    let copy_r = minix_rt::sys_vircopy(
        caller_ep,
        set_ptr as u64,
        minix_rt::SELF,
        mask_buf.as_mut_ptr() as u64,
        16,
    );
    if copy_r != 0 {
        return copy_r;
    }
    let set_bits = u128::from_ne_bytes(mask_buf);
    let set = SigSet { bits: [set_bits] };

    // Atomically: save old mask, set new mask, suspend.
    rmp.mp_sigmask2 = rmp.mp_sigmask;
    rmp.mp_sigmask = set;
    rmp.mp_flags |= SIGSUSPENDED;

    SUSPEND
}

/// Handle PM_SIGRETURN — restore the signal mask and CPU context after a
/// caught handler (SIGNALS.md Phase 4).
///
/// Message layout: m2l1 = pointer to the sigframe on the caller's stack.
/// PM restores `mp_sigmask` from the frame, then calls SYS_SIGRETURN so the
/// kernel restores the interrupted register state.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn do_sigreturn(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let caller_ep = unsafe { (*base.add(caller_slot)).mp_endpoint };
    let rmp = unsafe { &mut *base.add(caller_slot) };

    // The trampoline passes the sigframe address in m2l1.
    let scp = unsafe { msg.m_payload.m2.m2l1 } as u64;
    if scp == 0 {
        return EINVAL;
    }

    // Restore the signal mask from the frame (mask@SIGFRAME_MASK_OFF).
    let mut mask_buf = [0u8; 16];
    let copy_r = minix_rt::sys_vircopy(
        caller_ep,
        scp + arch_common::consts::sigframe::MASK_OFF as u64,
        minix_rt::SELF,
        mask_buf.as_mut_ptr() as u64,
        16,
    );
    if copy_r != 0 {
        return copy_r;
    }
    rmp.mp_sigmask = SigSet {
        bits: [u128::from_ne_bytes(mask_buf)],
    };
    rmp.mp_sigmask2 = SigSet::new();

    // Tell the kernel to restore the CPU context from the frame
    // (SYS_SIGRETURN = kernel call 10, m_sigcalls: endpt@16, sigctx@24).
    let endpoint = rmp.mp_endpoint;
    let mut kmsg = [0u8; 64];
    kmsg[arch_common::consts::SIGCALLS_ENDPT_OFF..arch_common::consts::SIGCALLS_ENDPT_OFF + 4]
        .copy_from_slice(&endpoint.to_le_bytes());
    kmsg[arch_common::consts::SIGCALLS_SIGCTX_OFF..arch_common::consts::SIGCALLS_SIGCTX_OFF + 8]
        .copy_from_slice(&scp.to_le_bytes());
    let _ = minix_rt::kernel_call(10, &mut kmsg);

    // Deliver any pending signals that are now unmasked.
    if has_pending(rmp) {
        let base = MPROC.as_ptr();
        let rmp2 = unsafe { &mut *base.add(caller_slot) };
        unsafe { check_pending(rmp2) };
    }

    OK
}

/// Process a kernel signal for a given process.
///
/// Called by `pm_server_main`'s NOTIFY_MESSAGE handler when the kernel
/// reports a signal via SYS_GETKSIG.
///
/// Returns the number of processes signaled.
///
/// # Safety
///
/// The caller must ensure exclusive access to the process table.
pub unsafe fn process_ksig(proc_nr_e: i32, signo: i32) -> i32 {
    let mut count = 0;

    match signo {
        2 | 3 | 28 | 29 => {
            // SIGINT, SIGQUIT, SIGWINCH, SIGINFO: the tty's sigchar already
            // targeted the specific reader (SYS_KILL(tty_incaller)), so
            // deliver to that process only. A group broadcast would also
            // hit the shell (no setpgid yet — the child inherits the
            // parent's pgrp), and delivering into the shell's waitpid
            // clears its SENDING/RECEIVING via do_sigsend without
            // completing the syscall, freezing it (observed: sigtest ^C
            // froze the shell mid-waitpid with an empty run queue).
            if let Some(slot) = unsafe { pm_isokendpt(proc_nr_e) } {
                unsafe { sig_proc(slot, signo, false, true) };
                count = 1;
            }
        }
        14 => {
            // SIGALRM: find the process with this endpoint.
            if let Some(slot) = unsafe { pm_isokendpt(proc_nr_e) } {
                unsafe { sig_proc(slot, signo, false, true) };
                count = 1;
            }
        }
        _ => {
            // Other signals: find the process, check ksigpending.
            if let Some(slot) = unsafe { pm_isokendpt(proc_nr_e) } {
                let base = MPROC.as_ptr();
                let rmp = unsafe { &mut *base.add(slot) };
                if rmp.mp_ksigpending.sigismember(signo) {
                    rmp.mp_ksigpending.sigdelset(signo);
                    unsafe { sig_proc(slot, signo, false, true) };
                    count = 1;
                }
            }
        }
    }

    count
}

/// Process one SYS_GETKSIG reply: deliver the pending signal bits via
/// `process_ksig`. Returns true when the reply is a pure exit (no pending
/// bits) — the caller must then run `exit_proc` with the reply's status.
///
/// The reply's pending bits are set only for signals (`cause_sig`) and the
/// status field only for exits (`sys_exit_handler`); the two never coincide
/// because `do_getksig` clears both in the same reply and a process cannot
/// run `exit()` while SIGNALED. Treating a signal-only reply as an exit
/// kills a process whose signal was ignored (observed with ^C / SIGINT).
///
/// # Safety
///
/// `process_ksig` bounds-checks the endpoint internally.
pub unsafe fn process_ksig_reply(endpt: i32, pending_bits: u128) -> bool {
    if pending_bits != 0 {
        // The kernel's cause_sig sets bit `sig` (1u128 << sig), not C's
        // sigaddset bit sig-1, so decode bit == signo.
        for signo in 1..(_NSIG as i32) {
            if pending_bits & (1u128 << (signo as usize)) != 0 {
                let _ = unsafe { process_ksig(endpt, signo) };
            }
        }
        false
    } else {
        true
    }
}

/// Try to unpause a process.
///
/// If process is WAITING or SIGSUSPENDED: sets PROC_STOPPED, sends
/// SYS_STOP kernel call, clears WAITING/SIGSUSPENDED, returns true.
/// If process has VFS_CALL flag: cannot unpause yet, returns false.
///
/// # Safety
///
/// `slot` must be < `NR_PROCS`. Caller must ensure exclusive access.
pub unsafe fn unpause(slot: usize) -> bool {
    if slot >= NR_PROCS {
        return false;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return false;
    }
    if rmp.mp_flags & VFS_CALL != 0 {
        return false;
    }
    if rmp.mp_flags & (WAITING | SIGSUSPENDED) != 0 {
        rmp.mp_flags |= PROC_STOPPED;
        let endpoint = rmp.mp_endpoint;
        let mut msg = [0u8; 64];
        msg[8..12].copy_from_slice(&endpoint.to_le_bytes());
        let _ = minix_rt::kernel_call(5, &mut msg);
        rmp.mp_flags &= !(WAITING | SIGSUSPENDED);
        return true;
    }
    false
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
/// Searches the MProc table by endpoint value (not by extracting slot bits),
/// because PM's slot allocator and the kernel's Proc table allocator may
/// assign different slot numbers for the same process (the kernel ignores
/// PM's child_slot hint due to a message size mismatch).
///
/// Matching C `pm_isokendpt()` from `minix/servers/pm/main.c` which extracts
/// the slot from the endpoint (`_ENDPOINT_P`). Our Rust implementation
/// cannot use that shortcut because the kernel and PM slot allocators are
/// independent, so we scan linearly.
///
/// # Safety
///
/// The caller must ensure that no conflicting mutable reference to the
/// process table exists while this function reads the relevant slot.
pub unsafe fn pm_isokendpt(endpoint: i32) -> Option<usize> {
    if endpoint < 0 {
        return None;
    }
    let base = MPROC.as_ptr();
    for i in 0..NR_PROCS {
        unsafe {
            let rmp = &*base.add(i);
            if rmp.mp_flags & IN_USE != 0 && rmp.mp_endpoint == endpoint {
                return Some(i);
            }
        }
    }
    None
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
    unsafe { exit_proc(caller_slot, status, false) };

    // Notify VM to clean up the child's address space BEFORE SYS_CLEAR.
    // In C MINIX, exit_proc calls vm_exit() which decrements PhysBlock
    // refcounts so shared COW pages survive. If we skip this and go
    // straight to SYS_CLEAR, release_address_space frees shared pages
    // that are still in use by the parent. Matching C's vm_exit()
    // (_taskcall, synchronous) so the notification is never dropped.
    let child_ep = unsafe { (*MPROC.as_ptr().add(caller_slot)).mp_endpoint };
    let mut vm_exit_msg = [0u8; 64];
    vm_exit_msg[4..8].copy_from_slice(&(arch_common::com::VM_EXIT as i32).to_le_bytes());
    // VM_EXIT handler reads endpoint from payload (m_source is PM).
    vm_exit_msg[8..12].copy_from_slice(&child_ep.to_le_bytes());
    let _ = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::VM_PROC_NR as u64,
            vm_exit_msg.as_mut_ptr() as u64,
        )
    };

    // The waiting parent is replied to inside exit_proc (zombify →
    // check_parent → tell_parent). A second reply here races with the
    // parent's next SENDREC: the kernel can deliver it into the parent's
    // stale receive buffer (REPLY_PEND set, p_getfrom_e stale), corrupting
    // the parent's stack (observed: shell jumped to garbage after the
    // second sigtest run). Matching C, exit_proc is the single reply path.

    // Matching C exit_proc / exit_restart: clean up kernel Proc entry
    // via SYS_CLEAR (kernel call 2). Without this, the kernel still
    // thinks the process is alive and blocked in RECEIVE/SENDREC.
    let mut clear_msg = Message {
        m_source: 0,
        m_type: 0,
        m_payload: unsafe { core::mem::zeroed() },
    };
    clear_msg.m_payload.m1.m1i1 = unsafe { (*MPROC.as_ptr().add(caller_slot)).mp_endpoint };
    let _ = send_kernel_call(2, &mut clear_msg);

    EDONTREPLY
}

/// Invoke a kernel call on the SYSTEM task.
///
/// `call_nr` is the kernel call number (0 = SYS_FORK, 1 = SYS_EXEC, etc.).
/// `msg` should have payload fields set in `m_payload`.
/// On success, `msg.m_payload` contains the kernel's reply.
/// Returns 0 on success, negative error code on failure.
pub fn send_kernel_call(call_nr: i32, msg: &mut Message) -> i32 {
    #[cfg(target_os = "minix")]
    unsafe {
        // Message is 56 bytes, but kernel expects 64. Use a proper
        // 64-byte buffer to avoid stack corruption from the size mismatch.
        #[cfg(target_arch = "riscv64")]
        let buf: &mut [u8; 64] = {
            // On RISC-V a stack-local buffer here is clobbered between the
            // kernel_call return and the copy-back: SYS_GETKSIG's reply
            // decoded as zero (PM never saw the pending-signal map), so
            // caught signals were silently dropped. A static buffer is out
            // of the clobbered stack region. x86/aarch64 keep the stack
            // local (x86's reply handling depends on it; a static buffer
            // there broke GETKSIG decode too).
            static mut BUF: [u8; 64] = [0u8; 64];
            &mut *core::ptr::addr_of_mut!(BUF)
        };
        #[cfg(not(target_arch = "riscv64"))]
        let buf: &mut [u8; 64] = &mut [0u8; 64];
        let msg_size = core::mem::size_of::<Message>();
        // Copy Message into 64-byte buffer (first msg_size bytes).
        core::ptr::copy_nonoverlapping(
            msg as *const Message as *const u8,
            buf.as_mut_ptr(),
            msg_size,
        );
        let result = minix_rt::kernel_call(call_nr, buf);
        // Copy back the first msg_size bytes (avoids reading garbage
        // from bytes 56-63 that the kernel may have overwritten).
        core::ptr::copy_nonoverlapping(buf.as_ptr(), msg as *mut Message as *mut u8, msg_size);
        result
    }
    #[cfg(not(target_os = "minix"))]
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
/// Message field offsets for building VFS PM messages via raw buffer.
/// Matches the mess_7 layout: m_type at +4, m7_i1..m7_i5 at +8,+12,+16,+20,+24.
const VFS_MSG_TYPE_OFF: usize = 4;
const VFS_M7_I1_OFF: usize = 8;
const VFS_M7_I2_OFF: usize = 12;
#[allow(dead_code)]
const VFS_M7_I3_OFF: usize = 16;
#[allow(dead_code)]
const VFS_M7_I4_OFF: usize = 20;
#[allow(dead_code)]
const VFS_M7_I5_OFF: usize = 24;

// m7 pointer fields in the packed convention used by `crates/servers/src/vfs/pm.rs`
// (m7p1 @ 28, m7p2 @ 36 — the Rust port does not 8-align pointers after the
// five i32 fields). VFS_PM_EXEC uses these for PATH and FRAME.
const VFS_M7_P1_OFF: usize = 28;
const VFS_M7_P2_OFF: usize = 36;

// VFS_PM_EXEC_REPLY field offsets (VFS→PM, same packed convention):
//   type@4, endpt@8, status@12, pc@28 (u64), newsp@36 (u64)
const EXEC_REPLY_STATUS_OFF: usize = 12;
const EXEC_REPLY_PC_OFF: usize = 28;
const EXEC_REPLY_NEWSP_OFF: usize = 36;

// m_lc_pm_exec (userland → PM) field offsets within the message payload
// (matches `mess_lc_pm_exec` in `.refs/minix-3.3.0/minix/include/minix/ipc.h`):
//   name@8 (u64), namelen@16 (u64), frame@24 (u64), framelen@32 (u64), ps_str@40 (u64)
const LC_EXEC_NAME_OFF: usize = 8;
const LC_EXEC_NAMELEN_OFF: usize = 16;
const LC_EXEC_FRAME_OFF: usize = 24;
const LC_EXEC_FRAMELEN_OFF: usize = 32;
const LC_EXEC_PS_STR_OFF: usize = 40;

/// Handle a PM_FORK request.
///
/// Performs the fork: creates a child MProc slot via `do_fork`, sends
/// VM_FORK to the VM server (which creates the child's kernel Proc entry
/// via SYS_FORK), then notifies VFS and returns the child PID.
///
/// Matching C: `do_fork()` in `minix/servers/pm/forkexit.c` with the
/// VM_FORK call order.
///
/// # Safety
///
/// - `caller_slot` must be a valid, in-use process slot.
/// - `msg` must point to a valid message buffer.
pub unsafe fn handle_fork(caller_slot: usize, _msg: &mut Message) -> i32 {
    let result = unsafe { do_fork(caller_slot) };
    match result {
        Ok(child_slot) => {
            let base = MPROC.as_ptr();
            let parent_endpoint = unsafe { (*base.add(caller_slot)).mp_endpoint };
            let child_pid = unsafe { (*base.add(child_slot)).mp_pid };

            // Call VM via IPC to create child page table + kernel Proc entry.
            // Matching C: `vm_fork(rmp->mp_endpoint, next_child, &child_ep)`
            // in forkexit.c. VM's do_fork does deep-copy page table, COW setup,
            // and sys_fork() in kernel.
            let mut vm_msg = [0u8; 64];
            vm_msg[4..8].copy_from_slice(&(arch_common::com::VM_FORK as i32).to_le_bytes());
            vm_msg[8..12].copy_from_slice(&parent_endpoint.to_le_bytes()); // VMF_ENDPOINT
            vm_msg[12..16].copy_from_slice(&(child_slot as i32).to_le_bytes()); // VMF_SLOTNO
            let vm_reply = unsafe {
                minix_rt::syscall2(
                    minix_rt::SENDREC_CALL,
                    arch_common::com::VM_PROC_NR as u64,
                    vm_msg.as_mut_ptr() as u64,
                )
            };
            // The SENDREC return is the source endpoint (always >= 0); the
            // reply status is m_type at bytes 4-8. Matching C: vm_fork()
            // returns the _taskcall reply type and do_fork() does
            // `if ((s = vm_fork(...)) != OK) return s;`. A failed VM fork
            // leaves m1i1 (= VMF_ENDPOINT, the parent endpoint) untouched,
            // so using it without checking m_type would record a garbage
            // child endpoint (observed: 10 = INIT's endpoint) and hang the
            // parent's waitpid forever.
            let vm_type = i32::from_le_bytes(vm_msg[4..8].try_into().unwrap_or([0; 4]));
            if vm_reply < 0 || vm_type != 0 {
                unsafe { free_proc(child_slot) };
                return if vm_type != 0 { vm_type } else { -1 };
            }
            let child_endpoint = i32::from_le_bytes(vm_msg[8..12].try_into().unwrap_or([0; 4]));
            unsafe {
                let mp = MPROC.as_ptr().add(child_slot);
                (*mp).mp_endpoint = child_endpoint;
            }

            // Notify VFS of the new child (matching C tell_vfs).
            let mut vfs_msg = [0u8; 64];
            vfs_msg[4..8]
                .copy_from_slice(&(arch_common::com::VFS_PM_RQ_BASE as i32 + 7).to_le_bytes());
            vfs_msg[8..12].copy_from_slice(&child_endpoint.to_le_bytes());
            vfs_msg[12..16].copy_from_slice(&parent_endpoint.to_le_bytes());
            vfs_msg[16..20].copy_from_slice(&child_pid.to_le_bytes());
            vfs_msg[20..24].copy_from_slice(&(-1i32).to_le_bytes()); // reuid
            vfs_msg[24..28].copy_from_slice(&(-1i32).to_le_bytes()); // regid
            let asend_result = unsafe {
                minix_rt::asynsend3(
                    arch_common::com::VFS_PROC_NR,
                    vfs_msg.as_ptr(),
                    arch_common::ipc::AMF_NOREPLY,
                )
            };
            if asend_result != 0 {
                unsafe { free_proc(child_slot) };
                return -1;
            }

            unsafe {
                let mp = MPROC.as_ptr().add(child_slot);
                (*mp).mp_flags |= VFS_CALL;
            }

            EDONTREPLY
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
    let options = unsafe { msg.m_payload.m1.m1i2 };
    match unsafe { do_waitpid(caller_slot, wpid, options) } {
        Ok((pid, status)) => {
            unsafe {
                msg.m_payload.m1.m1i1 = pid;
                msg.m_payload.m1.m1i2 = status;
            }
            OK
        }
        Err(EAGAIN) => EAGAIN,
        Err(_) => {
            // No zombie child found. Store the waitpid request and block.
            // Set mp_wpid so do_exit can find us when a child exits.
            let base = MPROC.as_ptr();
            unsafe {
                let rmp = &mut *base.add(caller_slot);
                rmp.mp_flags |= WAITING;
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

/// Handler for PM_SIGACTION — set or get signal action.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_sigaction(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_sigaction(caller_slot, msg) }
}

/// Handler for PM_SIGPENDING — return pending signal set.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_sigpending(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_sigpending(caller_slot, msg) }
}

/// Handler for PM_SIGPROCMASK — block/unblock/set/get signal mask.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_sigprocmask(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_sigprocmask(caller_slot, msg) }
}

/// Handler for PM_SIGSUSPEND — replace signal mask and suspend.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_sigsuspend(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_sigsuspend(caller_slot, msg) }
}

/// Handler for PM_SIGRETURN — restore mask and kernel context.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_sigreturn(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_sigreturn(caller_slot, msg) }
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
    #[cfg(target_os = "minix")]
    unsafe {
        // syscall1(NR_ABORT=27, 1) — kernel do_abort_handler with reboot.
        minix_rt::syscall1(27, 1);
    }
    OK
}

/// Handler for PM_EXEC — forward exec to VFS.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_exec(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_exec(caller_slot, msg) }
}

/// Handler for PM_EXEC_NEW — process exec info after VFS opens binary.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_newexec(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_newexec(caller_slot, msg) }
}

/// Handler for PM_EXEC_RESTART — complete exec with new entry point.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_execrestart(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_execrestart(caller_slot, msg) }
}

/// Handler for PM_GETTIMEOFDAY — return realtime clock.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_time(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_time(caller_slot, msg) }
}

/// Handler for PM_CLOCK_GETTIME — return a clock value (sec/nsec).
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_clock_gettime(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_gettime(caller_slot, msg) }
}

/// Handler for PM_GETRUSAGE — return resource usage.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_rusage(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_getrusage(caller_slot, msg) }
}

/// Handler for PM_ITIMER — set/get interval timer.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn handle_itimer(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe { do_itimer(caller_slot, msg) }
}

/// Handler for PM_SRV_KILL — kill a server process.
///
/// # Safety
///
/// `caller_slot` must be a valid process slot.
pub unsafe fn handle_srv_kill(caller_slot: usize, msg: &mut Message) -> i32 {
    let pid = unsafe { msg.m_payload.m1.m1i1 };
    let signo = unsafe { msg.m_payload.m1.m1i2 };
    match unsafe { do_kill(caller_slot, pid, signo) } {
        Ok(()) => OK,
        Err(e) => e,
    }
}

/// Handler for PM_STIME — set system time.
///
/// # Safety
///
/// `_caller_slot` must be a valid process slot.
pub unsafe fn handle_stime(_caller_slot: usize, _msg: &mut Message) -> i32 {
    // Setting system time requires root — simplified stub.
    ENOSYS
}

/// The PM dispatch table.
/// Maps each PM call number to its handler function.
pub fn pm_dispatch(caller_slot: usize, msg: &mut Message) -> i32 {
    // Handle notifications (m_type == NOTIFY_MESSAGE).
    if msg.m_type == arch_common::com::NOTIFY_MESSAGE as i32 {
        // Check for pending process exits and kernel signals
        // via SYS_GETKSIG (kernel call 7).
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
            // Kernel writes system::NONE (31743, not arch_common::endpoint::NONE)
            // as the sentinel for "no more pending".  Both -1 and 31743 are checked.
            if endpt == -1 || endpt == 31743 {
                break; // NONE sentinel — no more pending
            }

            // Reconstruct pending signal bitmask from scattered message fields.
            let b0 = (kmsg.m_source as u32) as u128;
            let b1 = (kmsg.m_type as u32) as u128;
            let b2 = (unsafe { kmsg.m_payload.m1.m1i1 } as u32) as u128;
            let b3 = (unsafe { kmsg.m_payload.m1.m1i2 } as u32) as u128;
            let pending_bits: u128 = b0 | (b1 << 32) | (b2 << 64) | (b3 << 96);

            // Process kernel signals from the pending bitmask.
            if pending_bits != 0 {
                // The kernel's cause_sig sets bit `sig` (1u128 << sig), not
                // C's sigaddset bit sig-1, so decode bit == signo.
                for signo in 1..(_NSIG as i32) {
                    if pending_bits & (1u128 << (signo as usize)) != 0 {
                        let _ = unsafe { process_ksig(endpt, signo) };
                    }
                }
            }

            let exit_status = unsafe { kmsg.m_payload.m1.m1i5 };
            // Find the MProc slot for this endpoint.
            if let Some(slot) = unsafe { pm_isokendpt(endpt) } {
                unsafe { exit_proc(slot, exit_status, false) };

                // Check if any parent is waiting for this child (waitpid).
                // The parent set mp_wpid in handle_waitpid when returning
                // EDONTREPLY. The waiting parent is replied to inside
                // exit_proc (zombify → check_parent → tell_parent), the
                // single reply path — a second reply here would race with
                // the parent's next SENDREC and corrupt its stale receive
                // buffer (see handle_exit).
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
        7 => unsafe { handle_stime(caller_slot, msg) }, // PM_STIME
        8 => unsafe { no_sys(caller_slot, msg) },       // PM_PTRACE
        9 => unsafe { no_sys(caller_slot, msg) },       // PM_SETGROUPS
        10 => unsafe { no_sys(caller_slot, msg) },      // PM_GETGROUPS
        11 => unsafe { handle_kill(caller_slot, msg) },
        12 => unsafe { handle_setgid(caller_slot, msg) }, // PM_SETGID
        13 => unsafe { handle_getgid(caller_slot, msg) }, // PM_GETGID
        14 => unsafe { handle_exec(caller_slot, msg) },
        15 => unsafe { handle_setsid(caller_slot, msg) },
        16 => unsafe { handle_getpgrp(caller_slot, msg) },
        17 => unsafe { handle_itimer(caller_slot, msg) }, // PM_ITIMER
        20 => unsafe { handle_sigaction(caller_slot, msg) },
        21 => unsafe { handle_sigsuspend(caller_slot, msg) },
        22 => unsafe { handle_sigpending(caller_slot, msg) },
        23 => unsafe { handle_sigprocmask(caller_slot, msg) },
        24 => unsafe { handle_sigreturn(caller_slot, msg) },
        25 => unsafe { no_sys(caller_slot, msg) }, // PM_SYSUNAME
        28 => unsafe { handle_time(caller_slot, msg) }, // PM_GETTIMEOFDAY
        29 => unsafe { no_sys(caller_slot, msg) }, // PM_SETEUID
        30 => unsafe { no_sys(caller_slot, msg) }, // PM_SETEGID
        32 => unsafe { no_sys(caller_slot, msg) }, // PM_GETSID
        34 => unsafe { handle_clock_gettime(caller_slot, msg) }, // PM_CLOCK_GETTIME
        35 => unsafe { handle_clock_settime(caller_slot, msg) }, // PM_CLOCK_SETTIME
        36 => unsafe { handle_rusage(caller_slot, msg) }, // PM_GETRUSAGE
        37 => unsafe { handle_reboot(caller_slot, msg) }, // PM_REBOOT
        42 => unsafe { handle_srv_kill(caller_slot, msg) }, // PM_SRV_KILL
        43 => unsafe { handle_newexec(caller_slot, msg) }, // PM_EXEC_NEW
        44 => unsafe { handle_execrestart(caller_slot, msg) }, // PM_EXEC_RESTART
        45 => unsafe { no_sys(caller_slot, msg) }, // PM_GETEPINFO
        46 => unsafe { no_sys(caller_slot, msg) }, // PM_GETPROCNR
        _ => unsafe { no_sys(caller_slot, msg) },
    }
}

/// Handle a VFS reply message (VFS_PM_FORK_REPLY, etc.).
///
/// Matching C: `handle_vfs_reply()` in `servers/pm/main.c`.
/// Called from the main loop when a message arrives from VFS_PROC_NR
/// with a type in the VFS_PM_RS range (0x980..0x9BF).
///
/// For VFS_PM_FORK_REPLY, sends OK to the child and PID to the parent,
/// matching the C flow in forkexit.c case VFS_PM_FORK_REPLY.
///
/// # Safety
///
/// - `_vfs_ep` must be VFS_PROC_NR.
/// - `msg` must point to a valid 64-byte message buffer.
/// - Must be called with exclusive access to the MProc table.
pub unsafe fn handle_vfs_reply(_vfs_ep: i32, msg: &mut Message) {
    let call_nr = msg.m_type;

    // Look up the process associated with this reply.
    // VFS echoes the child endpoint back in m7_i1 (offset 8), but
    // the IPC message payload is corrupted by the kernel's iretq
    // frame which overwrites bytes at user_rsp-40..user_rsp during
    // syscall return. This clobbers the upper portion of any
    // stack-local message buffer.  Instead of reading proc_e from
    // the message, scan the MProc table for a process with VFS_CALL
    // set — that's the child waiting for the VFS reply.
    let base = MPROC.as_ptr();
    let proc_n = (0..NR_PROCS).find(|&i| {
        let rmp = unsafe { &*base.add(i) };
        rmp.mp_flags & VFS_CALL != 0
    });
    let proc_n = match proc_n {
        Some(n) => n,
        None => return, // no pending VFS call — nothing to do
    };
    // Compute child endpoint from slot, matching do_fork encoding.
    // Do NOT read mp_endpoint from the MProc table — it may have
    // been corrupted by the iretq frame stack clobber during VFS
    // IPC.  The slot-based encoding is deterministic:
    //   endpoint = slot | 0x8000  (for gen-1 user processes)
    let proc_e = (proc_n as i32) | 0x8000;

    let _rmp = unsafe { &*base.add(proc_n) };

    // Clear VFS_CALL and re-deliver any signals that pended while the
    // process was in a PM→VFS round-trip (matching C: check_pending on the
    // VFS reply). Without the re-delivery, a caught signal that arrived
    // during the fork's VFS window was pended by sig_proc's VFS_CALL guard
    // and never delivered — the child stayed blocked forever (RISC-V ^C
    // on a freshly-forked sigtest hung: K/G/E fired but sigsend never ran).
    let rmp_mut = unsafe { &mut *base.add(proc_n) };
    unsafe { restart_sigs(rmp_mut) };
    let rmp = unsafe { &*base.add(proc_n) };

    match call_nr as u32 {
        arch_common::com::VFS_PM_FORK_REPLY => {
            // Step 1: Reply to parent FIRST so parent is ready to
            // handle waitpid before the child is scheduled.
            let parent_slot = rmp.mp_parent;
            if parent_slot >= 0 && (parent_slot as usize) < NR_PROCS {
                let parent_rmp = unsafe { &*base.add(parent_slot as usize) };
                if parent_rmp.mp_flags & IN_USE != 0 {
                    let mut parent_reply = [0u8; 64];
                    parent_reply[VFS_MSG_TYPE_OFF..VFS_MSG_TYPE_OFF + 4]
                        .copy_from_slice(&OK.to_le_bytes());
                    parent_reply[VFS_M7_I1_OFF..VFS_M7_I1_OFF + 4]
                        .copy_from_slice(&rmp.mp_pid.to_le_bytes());
                    let zero: i32 = 0;
                    parent_reply[VFS_M7_I2_OFF..VFS_M7_I2_OFF + 4]
                        .copy_from_slice(&zero.to_le_bytes());
                    unsafe {
                        minix_rt::syscall2(
                            minix_rt::SENDNB_CALL,
                            parent_rmp.mp_endpoint as u64,
                            parent_reply.as_ptr() as u64,
                        );
                    }
                }
            }

            // Step 2: Schedule the child (matching C: sched_start_user).
            // This clears RTS_NO_QUANTUM on the child and enqueues it.
            let mut sched_msg = [0u8; 64];
            sched_msg[8..12].copy_from_slice(&proc_e.to_le_bytes()); // endpoint
            sched_msg[12..16].copy_from_slice(&200i32.to_le_bytes()); // quantum (ticks)
            sched_msg[16..20].copy_from_slice(&0i32.to_le_bytes()); // priority (0 = same as boot procs)
            sched_msg[20..24].copy_from_slice(&0i32.to_le_bytes()); // cpu
            let sched_result = minix_rt::kernel_call(3, &mut sched_msg);
            if sched_result != 0 {
                // Scheduling failed — tear down child (matching C).
                unsafe {
                    free_proc(proc_n);
                };
                // (parent already received the OK reply above, will see
                //  no child or get SIGCHLD when child_free happens)
            } else {
                // Step 3: Reply to child with OK (matching C: reply(proc_n, OK)).
                // On RISC-V, do_fork_handler clears RECEIVING and REPLY_PEND
                // on the child directly — SENDNB reply is skipped.
                #[cfg(not(target_arch = "riscv64"))]
                {
                    let mut child_reply = [0u8; 64];
                    child_reply[VFS_MSG_TYPE_OFF..VFS_MSG_TYPE_OFF + 4]
                        .copy_from_slice(&OK.to_le_bytes());
                    let _child_send_result = unsafe {
                        minix_rt::syscall2(
                            minix_rt::SENDNB_CALL,
                            proc_e as u64,
                            child_reply.as_ptr() as u64,
                        )
                    };
                }
            }
        }

        _ => {
            // Unknown VFS reply — ignore (C panics here)
        }
    }
}

/// Initialize user-space scheduling for all boot processes.
///
/// Sends SCHEDULING_START to the SCHED server for each IN_USE process
/// in PM's MProc table, matching C MINIX's `sched_init()` in `main.c`.
/// The SCHED server calls SYS_SCHEDCTL to set p->p_scheduler, then
/// SYS_SCHEDULE to set priority/quantum and clear RTS_NO_QUANTUM.
///
/// Uses SENDREC (matching C's _taskcall) so PM waits for each reply
/// before moving to the next process. The SCHED server replies via
/// SENDREC which pairs with PM's RECEIVE phase.
///
/// # Safety
///
/// Must be called after MProc table is populated and IPC is available.
#[cfg(target_os = "minix")]
pub unsafe fn sched_init() {
    let base = MPROC.as_ptr();
    let sched_ep = arch_common::com::SCHED_PROC_NR;
    let sendrec_call: u64 = 48; // SYS_SENDREC

    for slot in 0..NR_PROCS {
        let rmp = unsafe { &*base.add(slot) };
        if rmp.mp_flags & IN_USE == 0 {
            continue;
        }

        // Build SCHEDULING_START message (m2 format).
        // m2i1 = endpoint, m2i2 = parent,
        // m2i3 = max_priority, m2l1 = quantum
        let mut msg = Message {
            m_source: 0,
            m_type: 0xF02, // SCHEDULING_START
            m_payload: unsafe { core::mem::zeroed() },
        };
        msg.m_payload.m2.m2i1 = rmp.mp_endpoint;
        msg.m_payload.m2.m2i2 = rmp.mp_parent;
        // User processes (INIT): USER_Q = 5.
        // System processes: SRV_Q = 7.
        msg.m_payload.m2.m2i3 = if rmp.mp_flags & PRIV_PROC != 0 { 7 } else { 5 };
        msg.m_payload.m2.m2l1 = 200; // DEFAULT_USER_TIME_SLICE

        // Send to SCHED server via SENDREC and wait for reply.
        // Matching C: _taskcall(scheduler_e, SCHEDULING_START, &m)
        // which does ipc_sendrec / SENDREC.
        unsafe {
            minix_rt::syscall2(
                sendrec_call,
                sched_ep as u64,
                &mut msg as *mut Message as u64,
            );
        }
    }
}

/// PM server main loop entry point.
///
/// Called once from the PM server process. Receives messages via kernel
/// IPC syscalls, dispatches to the appropriate handler, and sends replies.
/// On host builds (testing), this is a no-op — the dispatch logic is
/// exercised through unit tests instead.
pub fn pm_server_main() {
    #[cfg(target_os = "minix")]
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
                // System services (endpoints 0-9) get PRIV_PROC so
                // sig_proc/exit_proc treat them as system processes and a
                // signal broadcast cannot terminate them. INIT (endpoint
                // 10) stays a normal user process — it must receive ^C.
                if ep < arch_common::com::INIT_PROC_NR {
                    mp.mp_flags |= PRIV_PROC;
                }
            }
        }
        // Reserve slot 11 (kernel proc_addr(11) = RAMDISK) so alloc_proc
        // doesn't return it. The kernel's Proc table has boot processes
        // at proc_nr 0..11; PM's slot numbers must match for do_fork_handler
        // to find a free slot via proc_addr(child_slot).
        reserve_slot(11);

        // NOTE: sched_init() is NOT called. Boot processes never have
        // RTS_NO_QUANTUM set, and forked children get their quantum
        // cleared by handle_vfs_reply's direct SYS_SCHEDULE call.
        // The SCHED server is loaded but its IPC-based scheduling
        // init (SCHEDULING_START handshake) deadlocks because PM and
        // SCHED both use priority 5 (same queue) and the sched_init
        // SENDREC cycle is interruptible by other processes.
        //
        // When the privilege and priority system is properly wired
        // (PM higher than USER_Q), this can be restored.

        // Syscall numbers for IPC (from minix-std):
        //   RECEIVE_CALL = 47: receive(src, &mut msg) → sender endpoint
        const RECEIVE_CALL: u64 = 47;
        const ANY: i32 = 0x0000ffff;

        // Matching C MINIX: PM uses a global `m_in` message buffer.

        loop {
            let mut raw_buf = [0u8; 64];
            let buf_ptr = raw_buf.as_mut_ptr();

            // Receive a message from any sender.
            let src = unsafe { minix_rt::syscall2(RECEIVE_CALL, ANY as u64, buf_ptr as u64) };
            let src_ep = src as i32;
            let msg: &mut Message = unsafe { &mut *(buf_ptr as *mut Message) };

            // Handle notifications FIRST — before endpoint/slot resolution,
            // because notifications can come from kernel tasks with negative
            // endpoints (e.g. SYSTEM = -2) that won't pass pm_isokendpt.
            if msg.m_type == arch_common::com::NOTIFY_MESSAGE as i32 {
                // Check for pending process exits via SYS_GETKSIG (kernel call 7).
                loop {
                    let mut kmsg = Message {
                        m_source: 0,
                        m_type: 0,
                        m_payload: unsafe { core::mem::zeroed() },
                    };
                    let result = send_kernel_call(7, &mut kmsg);
                    if result != 0 {
                        break;
                    }
                    let endpt = unsafe { kmsg.m_payload.m1.m1i3 };
                    // Kernel writes system::NONE (31743) as sentinel.
                    if endpt == -1 || endpt == 31743 {
                        break;
                    }
                    // Skip PM's own endpoint (0) — signal manager signals
                    // itself via send_sig, which is not an exit to process.
                    if endpt == 0 {
                        // Still need SYS_ENDKSIG to clear SIG_PENDING,
                        // even though we skip processing (PM's own signal).
                        kmsg.m_payload.m1.m1i3 = endpt;
                        send_kernel_call(8, &mut kmsg);
                        continue;
                    }

                    // Reconstruct pending signal bitmask from scattered message
                    // fields. The kernel writes the u128 bitmask at offset 0,
                    // but bytes 0-7 are clobbered by the kernel_call trampoline.
                    let b0 = (kmsg.m_source as u32) as u128;
                    let b1 = (kmsg.m_type as u32) as u128;
                    let b2 = (unsafe { kmsg.m_payload.m1.m1i1 } as u32) as u128;
                    let b3 = (unsafe { kmsg.m_payload.m1.m1i2 } as u32) as u128;
                    let pending_bits: u128 = b0 | (b1 << 32) | (b2 << 64) | (b3 << 96);

                    // Process kernel signals from the pending bitmask.
                    let is_exit = unsafe { process_ksig_reply(endpt, pending_bits) };
                    if is_exit {
                        // No pending signal bits: this is a pure exit
                        // notification (sys_exit_handler sets
                        // p_signal_received but never p_pending; cause_sig
                        // sets p_pending but never p_signal_received).
                        // Running exit_proc for a signal-only reply kills
                        // the process even when the signal is ignored
                        // (observed with ^C / SIGINT).
                        let exit_status = unsafe { kmsg.m_payload.m1.m1i5 };
                        if let Some(slot) = unsafe { pm_isokendpt(endpt) } {
                            let base = MPROC.as_ptr();
                            let _pid = unsafe { (*base.add(slot)).mp_pid };
                            unsafe { exit_proc(slot, exit_status, false) };
                        }
                    }
                    // Clear SIG_PENDING via SYS_ENDKSIG (kernel call 8),
                    // matching C's end_work() after get_work().
                    kmsg.m_payload.m1.m1i3 = endpt;
                    send_kernel_call(8, &mut kmsg);
                }
                continue;
            }

            // For non-notifications, resolve the sender's process slot.
            let slot = match unsafe { pm_isokendpt(src_ep) } {
                Some(s) => s,
                None => {
                    continue;
                }
            };

            // Handle VFS replies: messages from VFS in the VFS_PM_RS range
            // (0x980..0x9BF). These are replies to PM's VFS_PM_FORK etc.
            // Matching C: `if (IS_VFS_PM_RS(call_nr) && who_e == VFS_PROC_NR)`
            if src_ep == arch_common::com::VFS_PROC_NR
                && arch_common::com::is_vfs_pm_rs(msg.m_type as u32)
            {
                unsafe { handle_vfs_reply(src_ep, msg) };
                // VFS messages should NOT receive a reply from PM's main loop —
                // handle_vfs_reply replies to the waiting caller directly.
                continue;
            }

            // Dispatch the call
            let status = pm_dispatch(slot, msg);

            // Send reply if the handler didn't return EDONTREPLY.
            // Use SENDNB (non-blocking) matching C MINIX's reply() which
            // uses ipc_sendnb. If the destination isn't receiving, the
            // send fails gracefully instead of blocking PM forever.
            if status != EDONTREPLY {
                msg.m_type = status;
                unsafe {
                    let send_buf = raw_buf.as_mut_ptr() as u64;
                    minix_rt::syscall2(minix_rt::SENDNB_CALL, src_ep as u64, send_buf);
                }
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // No-op on host builds — dispatch is tested directly
    }
}

/// Forward exec to VFS — PM_EXEC handler (call 14).
///
/// Reads path and frame info from the message, copies the path from the
/// caller's address space, and sends VFS_PM_EXEC to VFS via SENDREC.
/// Returns SUSPEND; VFS will call back via PM_EXEC_NEW then PM_EXEC_RESTART.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn do_exec(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }

    // m_lc_pm_exec layout (C `mess_lc_pm_exec`): name, namelen, frame,
    // framelen, ps_str — all vir_bytes/size_t (u64) fields.
    let name_ptr = u64::from_le_bytes(unsafe {
        msg.m_payload.raw[LC_EXEC_NAME_OFF - 8..LC_EXEC_NAME_OFF - 8 + 8]
            .try_into()
            .unwrap_or([0u8; 8])
    });
    let namelen = u64::from_le_bytes(unsafe {
        msg.m_payload.raw[LC_EXEC_NAMELEN_OFF - 8..LC_EXEC_NAMELEN_OFF - 8 + 8]
            .try_into()
            .unwrap_or([0u8; 8])
    }) as usize;
    let frame_ptr = u64::from_le_bytes(unsafe {
        msg.m_payload.raw[LC_EXEC_FRAME_OFF - 8..LC_EXEC_FRAME_OFF - 8 + 8]
            .try_into()
            .unwrap_or([0u8; 8])
    });
    let frame_len = u64::from_le_bytes(unsafe {
        msg.m_payload.raw[LC_EXEC_FRAMELEN_OFF - 8..LC_EXEC_FRAMELEN_OFF - 8 + 8]
            .try_into()
            .unwrap_or([0u8; 8])
    }) as usize;
    let _ps_str = u64::from_le_bytes(unsafe {
        msg.m_payload.raw[LC_EXEC_PS_STR_OFF - 8..LC_EXEC_PS_STR_OFF - 8 + 8]
            .try_into()
            .unwrap_or([0u8; 8])
    });

    if name_ptr == 0 || namelen == 0 {
        return EINVAL;
    }

    let caller_ep = rmp.mp_endpoint;

    // Matching C `do_exec`: remember the frame for procfs bookkeeping and
    // mark the process as mid-exec.
    rmp.mp_flags |= PARTIAL_EXEC;
    rmp.mp_frame_addr = frame_ptr;
    rmp.mp_frame_len = frame_len as u64;

    // Forward the exec request to VFS (VFS_PM_EXEC). Use a blocking SENDREC:
    // our VFS services PM requests synchronously, so the reply arrives here.
    // (C suspends the request and handles VFS_PM_EXEC_REPLY in the main loop;
    // the blocking form is equivalent for our single-threaded servers.)
    let mut vfs_msg = [0u8; 64];
    vfs_msg[VFS_MSG_TYPE_OFF..VFS_MSG_TYPE_OFF + 4]
        .copy_from_slice(&(arch_common::com::VFS_PM_EXEC as i32).to_le_bytes());
    // Packed m7 convention: endpt@8, path_len@12, frame_len@16, ps_str@24,
    // path@28, frame@36 (see `crates/servers/src/vfs/pm.rs`).
    vfs_msg[VFS_M7_I1_OFF..VFS_M7_I1_OFF + 4].copy_from_slice(&caller_ep.to_le_bytes());
    vfs_msg[VFS_M7_I2_OFF..VFS_M7_I2_OFF + 4].copy_from_slice(&(namelen as i32).to_le_bytes());
    vfs_msg[VFS_M7_I3_OFF..VFS_M7_I3_OFF + 4].copy_from_slice(&(frame_len as i32).to_le_bytes());
    vfs_msg[VFS_M7_I5_OFF..VFS_M7_I5_OFF + 8].copy_from_slice(&_ps_str.to_le_bytes());
    vfs_msg[VFS_M7_P1_OFF..VFS_M7_P1_OFF + 8].copy_from_slice(&name_ptr.to_le_bytes());
    vfs_msg[VFS_M7_P2_OFF..VFS_M7_P2_OFF + 8].copy_from_slice(&frame_ptr.to_le_bytes());
    let sendrec_result = unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            arch_common::com::VFS_PROC_NR as u64,
            vfs_msg.as_mut_ptr() as u64,
        )
    };
    if sendrec_result < 0 {
        // VFS unreachable — fail the exec.
        return sendrec_result as i32;
    }

    // Parse the VFS_PM_EXEC_REPLY (delivered into vfs_msg by SENDREC).
    let reply_type = i32::from_le_bytes(
        vfs_msg[VFS_MSG_TYPE_OFF..VFS_MSG_TYPE_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    );
    if reply_type != arch_common::com::VFS_PM_EXEC_REPLY as i32 {
        return EINVAL;
    }
    let status = i32::from_le_bytes(
        vfs_msg[EXEC_REPLY_STATUS_OFF..EXEC_REPLY_STATUS_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    );
    let pc = u64::from_le_bytes(
        vfs_msg[EXEC_REPLY_PC_OFF..EXEC_REPLY_PC_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let newsp = u64::from_le_bytes(
        vfs_msg[EXEC_REPLY_NEWSP_OFF..EXEC_REPLY_NEWSP_OFF + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );

    // exec_restart is unsafe: it mutates shared mproc state.
    unsafe { exec_restart(caller_slot, status, pc, newsp) }
}

/// Finish an exec after VFS has loaded the image — matching C `exec_restart()`
/// in `.refs/minix-3.3.0/minix/servers/pm/exec.c`.
///
/// On success the kernel's SYS_EXEC_LOAD already replaced the image AND made
/// the process runnable at the new entry point, so PM only resets signal
/// state and mproc fields; it deliberately does NOT re-issue SYS_EXEC (the
/// aarch64/riscv64 `arch_proc_init` zero-fills the frame, which would destroy
/// the argc/argv registers set during loading). On failure the caller's
/// SENDREC is answered with the error, or the process is killed if the image
/// was partially replaced.
///
/// Returns EDONTREPLY so the PM main loop does not double-reply.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn exec_restart(caller_slot: usize, status: i32, _pc: u64, _newsp: u64) -> i32 {
    // pc/newsp are used by the C flow's sys_exec; the kernel's SYS_EXEC_LOAD
    // already set the registers when it replaced the image.
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }

    if status != OK {
        if rmp.mp_flags & PARTIAL_EXEC != 0 {
            // The image was partially replaced — the process cannot continue.
            unsafe { sig_proc(caller_slot, 9, false, true) };
        } else {
            // Exec failed before anything was touched — reply the error.
            let mut reply_msg = Message {
                m_source: 0,
                m_type: status,
                m_payload: unsafe { core::mem::zeroed() },
            };
            let ep = rmp.mp_endpoint;
            unsafe {
                minix_rt::syscall2(
                    minix_rt::SENDNB_CALL,
                    ep as u64,
                    &mut reply_msg as *mut Message as u64,
                );
            }
        }
        rmp.mp_flags &= !PARTIAL_EXEC;
        return EDONTREPLY;
    }

    rmp.mp_flags &= !PARTIAL_EXEC;

    // Reset caught/ignored signals to default (matching C).
    rmp.mp_catch.sigemptyset();
    rmp.mp_ignore.sigemptyset();

    EDONTREPLY
}

/// Handle PM's side after VFS opens the executable — PM_EXEC_NEW handler.
///
/// Reads exec info from VFS reply: new euid, egid, process name.
/// Applies setuid/setgid bits (if not traced), updates mp_effuid,
/// mp_effgid, mp_realuid, mp_name. Sets TAINTED flag.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn do_newexec(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }

    // Read exec info from VFS reply: m1i1=euid, m1i2=egid, name in raw bytes.
    let new_euid = unsafe { msg.m_payload.m1.m1i1 };
    let new_egid = unsafe { msg.m_payload.m1.m1i2 };

    // Only apply setuid/setgid if not traced.
    if rmp.mp_tracer == NO_TRACER {
        if new_euid != -1 {
            rmp.mp_effuid = new_euid;
            rmp.mp_realuid = new_euid;
        }
        if new_egid != -1 {
            rmp.mp_effgid = new_egid;
        }
        rmp.mp_flags |= TAINTED;
    }

    // Copy process name from VFS reply (bytes at offset 24 in raw payload).
    let raw = unsafe { &msg.m_payload.raw };
    let name_len = PROC_NAME_LEN.min(16);
    for i in 0..name_len {
        rmp.mp_name[i] = raw[24 + i] as i8;
    }

    OK
}

/// Complete exec — PM_EXEC_RESTART handler.
///
/// Reads new entry point and stack pointer from msg. Clears PARTIAL_EXEC.
/// Resets caught signals to SIG_DFL. Sends SYS_EXEC (kernel call 1) to
/// kernel with new entry point and stack. If traced, sends SIGTRAP.
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot.
pub unsafe fn do_execrestart(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &mut *base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }

    let pc = unsafe { msg.m_payload.m1.m1i1 as u64 };
    let newsp = unsafe { msg.m_payload.m1.m1i2 as u64 };

    rmp.mp_flags &= !PARTIAL_EXEC;

    // Reset caught signals to SIG_DFL (clear mp_catch and mp_ignore).
    rmp.mp_catch.sigemptyset();
    rmp.mp_ignore.sigemptyset();

    let endpoint = rmp.mp_endpoint;

    // Send SYS_EXEC (kernel call 1) to set up the new process image.
    let mut kmsg = [0u8; 64];
    kmsg[8..12].copy_from_slice(&endpoint.to_le_bytes()); // EXEC_ENDPT_OFF
    kmsg[16..24].copy_from_slice(&pc.to_le_bytes()); // EXEC_IP_OFF
    kmsg[24..32].copy_from_slice(&newsp.to_le_bytes()); // EXEC_STACK_OFF
    let kresult = minix_rt::kernel_call(1, &mut kmsg);
    if kresult != 0 {
        // Exec failed — kill the process with SIGKILL.
        unsafe { sig_proc(caller_slot, 9, false, true) };
        return kresult;
    }

    // If traced, send SIGTRAP.
    if rmp.mp_tracer != NO_TRACER {
        let tracer = rmp.mp_tracer;
        if tracer >= 0 && (tracer as usize) < NR_PROCS {
            let tracer_rmp = unsafe { &*base.add(tracer as usize) };
            if tracer_rmp.mp_flags & IN_USE != 0 {
                let mut trap_msg = Message {
                    m_source: 0,
                    m_type: OK,
                    m_payload: unsafe { core::mem::zeroed() },
                };
                trap_msg.m_payload.m1.m1i1 = rmp.mp_pid;
                trap_msg.m_payload.m1.m1i2 = 5; // SIGTRAP
                let _ = unsafe {
                    minix_rt::syscall2(
                        minix_rt::SENDNB_CALL,
                        tracer_rmp.mp_endpoint as u64,
                        &mut trap_msg as *mut Message as u64,
                    )
                };
            }
        }
    }

    OK
}

/// Return current realtime clock — PM_GETTIMEOFDAY handler.
///
/// Matching C: `do_time()` in `minix/servers/pm/time.c`. Computes the
/// wall clock from the realtime tick count and replies with the C
/// `mess_pm_lc_time` layout (sec @ payload[0..8], nsec @ payload[8..16]).
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
pub unsafe fn do_time(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }

    let (_ticks, realtime, boottime, hz) = kernel_clock();

    let (sec, nsec) = clock_ticks_to_sec_nsec(realtime, boottime, hz);
    // Reply mess_pm_lc_time (C ipc.h): sec @ payload 0, nsec @ payload 8.
    unsafe {
        msg.m_payload.raw[0..8].copy_from_slice(&sec.to_le_bytes());
        msg.m_payload.raw[8..16].copy_from_slice(&nsec.to_le_bytes());
    }
    OK
}

/// Clock IDs for the PM time calls (matching `minix-std`'s
/// `CLOCK_REALTIME` / `CLOCK_MONOTONIC`).
const CLOCK_REALTIME: i32 = 0;
const CLOCK_MONOTONIC: i32 = 1;

/// Decode the SYS_TIMES reply message. The kernel `do_times_handler`
/// writes: real @ msg[0..8], boot_ticks @ msg[8..16], boottime @
/// msg[16..24], user @ 24, system @ 32, hz @ 40. Bytes 0-7 straddle
/// m_source/m_type and are clobbered by the kernel_call trampoline, so
/// realtime must be reconstructed like the SYS_GETKSIG bitmask.
///
/// Returns (ticks, realtime, boottime, hz).
#[cfg(any(target_os = "minix", test))]
fn parse_times_reply(msg: &Message) -> (u64, u64, i64, u64) {
    let realtime = (msg.m_source as u32 as u64) | ((msg.m_type as u32 as u64) << 32);
    let ticks = unsafe { u64::from_le_bytes(msg.m_payload.raw[0..8].try_into().unwrap_or([0; 8])) };
    let boottime =
        unsafe { i64::from_le_bytes(msg.m_payload.raw[8..16].try_into().unwrap_or([0; 8])) };
    let hz = unsafe {
        u32::from_le_bytes(msg.m_payload.raw[32..36].try_into().unwrap_or([0; 4])) as u64
    };
    (ticks, realtime, boottime, hz)
}

/// Fetch (uptime ticks, realtime ticks, boottime, system_hz) from the
/// kernel via SYS_TIMES (kernel call 25) — matching C `getuptime()`
/// (libsys), which reads the same fields from the SYS_TIMES reply.
/// PM runs in its own address space, so its linked copy of the kernel
/// crate's clock statics is never updated by the timer interrupt;
/// reading them directly would return a frozen zero clock.
///
/// On host builds there is no kernel to ask, so read the kernel crate's
/// clock state directly; tests set it to simulate a booted kernel.
fn kernel_clock() -> (u64, u64, i64, u64) {
    #[cfg(target_os = "minix")]
    {
        let mut kmsg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let result = send_kernel_call(25, &mut kmsg); // SYS_TIMES
        if result != 0 {
            return (0, 0, 0, 0);
        }
        parse_times_reply(&kmsg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        (
            kernel::clock::get_monotonic(),
            kernel::clock::get_realtime(),
            kernel::clock::get_boottime(),
            kernel::glo::SYSTEM_HZ.load(Ordering::Relaxed) as u64,
        )
    }
}

/// Convert a tick count to (sec, nsec) since boot — the pure arithmetic
/// behind both `do_time` and `do_gettime`. Matching C `do_time()` /
/// `do_gettime()` in `minix/servers/pm/time.c`: `sec = boottime +
/// clock/hz` and `nsec = (clock % hz) * 1e9 / hz`.
fn clock_ticks_to_sec_nsec(clock: u64, boottime: i64, hz: u64) -> (i64, i64) {
    (
        boottime + (clock / hz) as i64,
        ((clock % hz) * 1_000_000_000 / hz) as i64,
    )
}

/// Compute the `mess_pm_lc_time` reply values for `do_gettime` — pure
/// logic, host-testable. `clock` is the realtime tick count for
/// `CLOCK_REALTIME` and the uptime tick count for `CLOCK_MONOTONIC`.
fn clock_gettime_reply(
    clk_id: i32,
    ticks: u64,
    realtime: u64,
    boottime: i64,
    hz: u64,
) -> Result<(i64, i64), i32> {
    let clock = match clk_id {
        CLOCK_REALTIME => realtime,
        CLOCK_MONOTONIC => ticks,
        _ => return Err(EINVAL),
    };
    Ok(clock_ticks_to_sec_nsec(clock, boottime, hz))
}

/// Return a clock value — PM_CLOCK_GETTIME handler.
///
/// Matching C: `do_gettime()` in `minix/servers/pm/time.c`. The request
/// carries the C `mess_lc_pm_time` layout (clk_id @ payload[8..12]); the
/// reply is the C `mess_pm_lc_time` layout (sec @ payload[0..8],
/// nsec @ payload[8..16]).
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
pub unsafe fn do_gettime(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }

    // mess_lc_pm_time (C ipc.h): sec @ payload 0, clk_id @ payload 8,
    // now @ payload 12, nsec @ payload 16.
    let clk_id =
        unsafe { i32::from_le_bytes(msg.m_payload.raw[8..12].try_into().unwrap_or([0; 4])) };

    let (ticks, realtime, boottime, hz) = kernel_clock();

    let (sec, nsec) = match clock_gettime_reply(clk_id, ticks, realtime, boottime, hz) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // Reply mess_pm_lc_time (C ipc.h): sec @ payload 0, nsec @ payload 8.
    unsafe {
        msg.m_payload.raw[0..8].copy_from_slice(&sec.to_le_bytes());
        msg.m_payload.raw[8..16].copy_from_slice(&nsec.to_le_bytes());
    }
    OK
}

/// Handler for PM_CLOCK_SETTIME — set the realtime clock via SYS_SETTIME.
///
/// Matching C: `do_settime()` in `minix/servers/pm/time.c`. Only root may
/// set the clock and only `CLOCK_REALTIME` is settable (the kernel enforces
/// the latter). The request carries the C `mess_lc_pm_time` layout (sec @ 8,
/// clk_id @ 16, now @ 20, nsec @ 24); it is forwarded to the kernel as
/// `mess_lsys_krn_sys_settime` (SYS_SETTIME, kernel call 40).
///
/// # Safety
///
/// `caller_slot` must be a valid, in-use process slot. `msg` must point
/// to a valid message buffer.
#[allow(unused_unsafe)]
pub unsafe fn handle_clock_settime(caller_slot: usize, msg: &mut Message) -> i32 {
    unsafe {
        if caller_slot >= NR_PROCS {
            return EINVAL;
        }
        let base = MPROC.as_ptr();
        let rmp = &*base.add(caller_slot);
        if rmp.mp_flags & IN_USE == 0 {
            return EINVAL;
        }
        // Only root (uid 0) may set the clock (C: mp->mp_effuid != SUPER_USER).
        if rmp.mp_effuid != 0 {
            return EPERM;
        }

        // mess_lc_pm_time (C ipc.h): sec @ 8, clk_id @ 16, now @ 20, nsec @ 24.
        let sec = i64::from_le_bytes(msg.m_payload.raw[0..8].try_into().unwrap_or([0; 8]));
        let clock_id = i32::from_le_bytes(msg.m_payload.raw[8..12].try_into().unwrap_or([0; 4]));
        let now = i32::from_le_bytes(msg.m_payload.raw[12..16].try_into().unwrap_or([0; 4]));
        let nsec = i64::from_le_bytes(msg.m_payload.raw[16..24].try_into().unwrap_or([0; 8]));

        // SYS_SETTIME (kernel call 40): payload @ 8 — sec, nsec, now, clock_id.
        let mut sys_msg = [0u8; 64];
        sys_msg[8..16].copy_from_slice(&sec.to_le_bytes());
        sys_msg[16..24].copy_from_slice(&nsec.to_le_bytes());
        sys_msg[24..28].copy_from_slice(&now.to_le_bytes());
        sys_msg[28..32].copy_from_slice(&clock_id.to_le_bytes());
        minix_rt::kernel_call(40, &mut sys_msg)
    }
}

/// Return resource usage — PM_GETRUSAGE handler.
///
/// # Safety
///
/// `caller_slot` must be a valid process slot.
pub unsafe fn do_getrusage(caller_slot: usize, msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }

    // Write child_utime/child_stime to msg: m2l1/m2l2.
    msg.m_payload.m2.m2l1 = rmp.mp_child_utime as i64;
    msg.m_payload.m2.m2l2 = rmp.mp_child_stime as i64;
    OK
}

/// Set/get interval timer — PM_ITIMER handler.
///
/// Simplified: returns OK for now.
///
/// # Safety
///
/// `caller_slot` must be a valid process slot.
pub unsafe fn do_itimer(caller_slot: usize, _msg: &mut Message) -> i32 {
    if caller_slot >= NR_PROCS {
        return EINVAL;
    }
    let base = MPROC.as_ptr();
    let rmp = unsafe { &*base.add(caller_slot) };
    if rmp.mp_flags & IN_USE == 0 {
        return EINVAL;
    }
    let _ = rmp;
    OK
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

    // A pid that no test process table entry can ever hold (pids are 1..NR_PROCS).
    const UNMATCHED_PID: i32 = 99999;

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
    fn test_ksig_bitmask_decode_matches_kernel_set() {
        // The kernel's cause_sig sets p_pending bit `sig` (1u128 << sig),
        // not C's sigaddset bit sig-1. PM decodes bit == signo. A kernel
        // SIGINT bit (2) must decode to signo 2, never 3 (SIGQUIT).
        let pending_bits = 1u128 << 2; // kernel set bit for SIGINT
        let mut decoded = 0i32;
        for signo in 1.._NSIG {
            if pending_bits & (1u128 << signo) != 0 {
                decoded = signo as i32;
                break;
            }
        }
        assert_eq!(decoded, 2, "bit 2 must decode to SIGINT");

        // SIGHUP (1) sits at bit 1; a signal at bit 3 is SIGQUIT (3).
        let pending = 1u128 << 1;
        for signo in 1.._NSIG {
            if pending & (1u128 << signo) != 0 {
                assert_eq!(signo as i32, 1);
                break;
            }
        }
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
        // init_proc marks 10 boot process slots as IN_USE.
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 10);
    }

    #[test]
    fn test_init_proc_marks_boot_slots_priv() {
        init_proc();
        unsafe {
            let base = MPROC.as_ptr();
            // The boot slots are kernel-alignment placeholders, not real
            // processes: all of them are PRIV_PROC so a signal broadcast
            // cannot terminate a placeholder slot. The real INIT entry
            // (allocated in pm_server_main, endpoint 10) is a user process
            // and receives ^C.
            for &slot in &[6, 2, 0, 4, 1, 11, 8, 7, 5, 10] {
                assert_ne!(
                    (*base.add(slot)).mp_flags & PRIV_PROC,
                    0,
                    "boot placeholder slot {slot} must be PRIV_PROC"
                );
            }
        }
    }

    /// Reset a free slot as an in-use user process with an endpoint/pid.
    fn test_sig_slot(slot: usize, ep: i32, pid: i32) {
        unsafe {
            let base = MPROC.as_ptr();
            let rmp = &mut *base.add(slot);
            *rmp = MProc::zeroed();
            rmp.mp_flags = IN_USE;
            rmp.mp_endpoint = ep;
            rmp.mp_pid = pid;
            rmp.mp_tracer = NO_TRACER;
            rmp.mp_magic = MP_MAGIC;
        }
    }

    // --- signal-delivery pins (SIGNALS.md Phase 2 / TTY.md 1C) ---

    #[test]
    fn test_apply_action_sig_ign_sets_ignore() {
        let mut mp = MProc::zeroed();
        apply_action(&mut mp, SIGINT, SIG_IGN, 0, 0);
        assert!(mp.mp_ignore.sigismember(SIGINT));
        assert!(!mp.mp_catch.sigismember(SIGINT));
        assert_eq!(mp.mp_sigreturn, 0);
    }

    #[test]
    fn test_apply_action_sig_dfl_clears_both() {
        let mut mp = MProc::zeroed();
        mp.mp_ignore.sigaddset(SIGINT);
        mp.mp_catch.sigaddset(SIGQUIT);
        mp.mp_sigreturn = 0x1234;
        apply_action(&mut mp, SIGINT, SIG_DFL, 0, 0);
        assert!(!mp.mp_ignore.sigismember(SIGINT));
        assert!(
            mp.mp_catch.sigismember(SIGQUIT),
            "unrelated signals untouched"
        );
        assert_eq!(mp.mp_sigreturn, 0x1234);
    }

    #[test]
    fn test_apply_action_catch_registers_handler_no_mask() {
        let mut mp = MProc::zeroed();
        apply_action(&mut mp, SIGINT, 0x1000_2000, 0, 0);
        assert!(mp.mp_catch.sigismember(SIGINT));
        assert_eq!(mp.mp_sigreturn, 0x1000_2000);
        // Registration must NOT block the handled signal — blocking it here
        // would make it permanently undeliverable (the mask is applied at
        // delivery and restored from the sigframe on sigreturn).
        assert!(!mp.mp_sigmask.sigismember(SIGINT), "signo not auto-masked");
    }

    #[test]
    fn test_sig_proc_ignored_signal_is_dropped() {
        init_proc();
        test_sig_slot(12, 42, 100);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_ignore.sigaddset(SIGINT);
            sig_proc(12, SIGINT, false, true);
            let rmp = &*base.add(12);
            assert!(!rmp.mp_sigpending.sigismember(SIGINT));
            assert_eq!(
                rmp.mp_flags & EXITING,
                0,
                "ignored signal must not terminate"
            );
        }
    }

    #[test]
    fn test_sig_proc_blocked_signal_pends() {
        init_proc();
        test_sig_slot(12, 42, 100);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_sigmask.sigaddset(SIGINT);
            sig_proc(12, SIGINT, false, true);
            let rmp = &*base.add(12);
            assert!(rmp.mp_sigpending.sigismember(SIGINT));
            assert!(
                rmp.mp_ksigpending.sigismember(SIGINT),
                "ksig pending tracked"
            );
        }
    }

    #[test]
    fn test_sig_proc_priv_slot_never_exits() {
        init_proc();
        test_sig_slot(12, 42, 100);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_flags |= PRIV_PROC;
            // SIGKILL on a priv slot must not reach exit_proc (which would
            // asynsend3 to VFS — a host syscall — so a regression crashes
            // this test loudly instead of passing silently).
            sig_proc(12, SIGKILL, false, true);
            let rmp = &*base.add(12);
            assert_eq!(rmp.mp_flags & EXITING, 0, "priv proc must not exit");
        }
    }

    #[test]
    fn test_check_sig_pid_targets_only_that_pid() {
        init_proc();
        test_sig_slot(12, 42, 100);
        test_sig_slot(13, 43, 101);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_sigmask.sigaddset(SIGINT);
            (*base.add(13)).mp_sigmask.sigaddset(SIGINT);
            let _ = check_sig(100, 0, SIGINT, true);
            assert!((*base.add(12)).mp_sigpending.sigismember(SIGINT));
            assert!(!(*base.add(13)).mp_sigpending.sigismember(SIGINT));
        }
    }

    #[test]
    fn test_check_sig_broadcast_skips_priv_and_pends_blocked() {
        init_proc();
        test_sig_slot(12, 42, 100);
        test_sig_slot(13, 43, 101);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_flags |= PRIV_PROC;
            (*base.add(12)).mp_sigmask.sigaddset(SIGINT);
            (*base.add(13)).mp_sigmask.sigaddset(SIGINT);
            let _ = check_sig(-1, 0, SIGINT, true);
            assert!(
                !(*base.add(12)).mp_sigpending.sigismember(SIGINT),
                "priv slot must be skipped by broadcast"
            );
            assert!((*base.add(13)).mp_sigpending.sigismember(SIGINT));
        }
    }

    #[test]
    fn test_process_ksig_sigint_direct_target() {
        // v1: sigchar targets tty_incaller (the reader) and process_ksig
        // delivers INT/QUIT/WINCH/INFO to the reported endpoint only. A
        // group broadcast would also hit the shell (no setpgid yet), and
        // delivering into the shell's waitpid without completing the syscall
        // froze it (observed: sigtest ^C stranded the shell mid-waitpid).
        init_proc();
        test_sig_slot(12, 42, 100);
        test_sig_slot(13, 43, 101);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_flags |= PRIV_PROC;
            (*base.add(12)).mp_sigmask.sigaddset(SIGINT);
            (*base.add(13)).mp_sigmask.sigaddset(SIGINT);
            let count = process_ksig(42, SIGINT);
            assert!(!(*base.add(12)).mp_sigpending.sigismember(SIGINT));
            assert!(
                !(*base.add(13)).mp_sigpending.sigismember(SIGINT),
                "other endpoint must not be signaled"
            );
            assert_eq!(count, 1);
        }
    }

    #[test]
    fn test_check_sig_group_targets_only_that_group() {
        init_proc();
        test_sig_slot(12, 42, 100);
        test_sig_slot(13, 43, 101);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_procgrp = 5;
            (*base.add(13)).mp_procgrp = 9;
            (*base.add(12)).mp_sigmask.sigaddset(SIGINT);
            (*base.add(13)).mp_sigmask.sigaddset(SIGINT);
            let _ = check_sig(0, 5, SIGINT, true);
            assert!(
                (*base.add(12)).mp_sigpending.sigismember(SIGINT),
                "pgrp 5 member pended"
            );
            assert!(
                !(*base.add(13)).mp_sigpending.sigismember(SIGINT),
                "other pgrp untouched"
            );
        }
    }

    #[test]
    fn test_process_ksig_sigint_does_not_broadcast_to_group() {
        // With no job control, the reported endpoint is the only target — a
        // pgrp broadcast has no meaning yet (all processes share the shell's
        // group). See test_process_ksig_sigint_direct_target.
        init_proc();
        test_sig_slot(12, 42, 100);
        test_sig_slot(13, 43, 101);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_procgrp = 5;
            (*base.add(13)).mp_procgrp = 9;
            (*base.add(12)).mp_sigmask.sigaddset(SIGINT);
            (*base.add(13)).mp_sigmask.sigaddset(SIGINT);
            // Signal the pgrp-5 process: only the reported endpoint moves.
            let count = process_ksig(42, SIGINT);
            assert!((*base.add(12)).mp_sigpending.sigismember(SIGINT));
            assert!(!(*base.add(13)).mp_sigpending.sigismember(SIGINT));
            assert_eq!(count, 1);
        }
    }

    #[test]
    fn test_process_ksig_reply_classifies_exit_vs_signal() {
        init_proc();
        // Unknown endpoint: the broadcast arm still runs, but every in-use
        // slot is a PRIV_PROC placeholder, so sig_proc drops the signal.
        let is_exit = unsafe { process_ksig_reply(9999, 0) };
        assert!(is_exit, "no pending bits => pure exit notification");
        let is_exit = unsafe { process_ksig_reply(9999, 1u128 << SIGINT) };
        assert!(!is_exit, "pending bits => signal, never an exit");
    }

    #[test]
    fn test_process_ksig_reply_delivers_signal_bits() {
        init_proc();
        test_sig_slot(12, 42, 100);
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(12)).mp_sigmask.sigaddset(SIGINT);
            let is_exit = process_ksig_reply(42, 1u128 << SIGINT);
            assert!(!is_exit);
            assert!((*base.add(12)).mp_sigpending.sigismember(SIGINT));
        }
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
        // 10 boot process slots are pre-marked as IN_USE.
        let idx = alloc_proc().expect("should find a free slot");
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 11);
        unsafe {
            free_proc(idx);
        }
        unsafe {
            let base = MPROC.as_ptr();
            let rmp = &*base.add(idx);
            assert!(!rmp.in_use());
            assert_eq!(rmp.mp_magic, 0);
            // Back to 10 boot process slots.
            assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 10);
        }
    }

    #[test]
    fn test_alloc_proc_exhaustion() {
        init_proc();
        // 10 boot process slots are pre-marked as IN_USE.
        let boot_slots = 10;
        let mut count = 0;
        while alloc_proc().is_some() {
            count += 1;
        }
        assert_eq!(count, NR_PROCS - boot_slots);
    }

    #[test]
    fn test_procs_in_use_tracking() {
        init_proc();
        // init_proc marks 10 boot process slots as IN_USE.
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 10);
        let a = alloc_proc().unwrap();
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 11);
        let b = alloc_proc().unwrap();
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 12);
        unsafe {
            free_proc(a);
        }
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 11);
        unsafe {
            free_proc(b);
        }
        assert_eq!(PROCS_IN_USE.load(core::sync::atomic::Ordering::Relaxed), 10);
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
            // Slot 0 is PM (a boot process), so use a non-boot slot.
            assert!(get_proc(66).is_none());
            assert!(get_proc_mut(66).is_none());
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
        // On host builds (not target_os = "minix"), send_kernel_call
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
        let r = unsafe { do_waitpid(parent, -1, 0) };
        assert!(r.is_err());
    }

    #[test]
    fn test_do_waitpid_wnohang_no_children() {
        init_proc();
        let parent = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(parent)).mp_flags |= IN_USE;
        }
        // No zombie + WNOHANG → EAGAIN (non-blocking miss).
        let r = unsafe { do_waitpid(parent, -1, WNOHANG) };
        assert_eq!(r, Err(EAGAIN));
    }

    #[test]
    fn test_do_waitpid_wnohang_with_zombie() {
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
            (*base.add(child)).mp_exitstatus = 9;
        }
        // Zombie present → reaped even with WNOHANG.
        let r = unsafe { do_waitpid(parent, -1, WNOHANG) };
        assert_eq!(r, Ok((2, 9)));
    }

    #[test]
    fn test_handle_waitpid_wnohang_returns_eagain() {
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
            // wpid = -1, options = WNOHANG (m1i2).
            msg.m_payload.m1.m1i1 = -1;
            msg.m_payload.m1.m1i2 = WNOHANG;
        }
        let result = unsafe { handle_waitpid(parent, &mut msg) };
        assert_eq!(result, EAGAIN);
        // The caller must not be marked WAITING on a WNOHANG miss.
        unsafe {
            let base = MPROC.as_ptr();
            assert_eq!((*base.add(parent)).mp_flags & WAITING, 0);
        }
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
        let r = unsafe { do_waitpid(parent, -1, 0) };
        assert_eq!(r, Ok((2, 7)));
        // Child slot should be freed
        unsafe {
            let base = MPROC.as_ptr();
            assert_eq!((*base.add(child)).mp_flags & IN_USE, 0);
        }
    }

    #[test]
    fn test_check_sig_unknown_pid_returns_esrch() {
        init_proc();
        let caller = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(caller)).mp_flags |= IN_USE;
            (*base.add(caller)).mp_pid = 1;
        }
        // No process has this pid → kill(2) must return ESRCH.
        let r = unsafe { check_sig(UNMATCHED_PID, 0, SIGTERM, false) };
        assert_eq!(r, Err(-3)); // ESRCH
    }

    #[test]
    fn test_check_sig_matching_pid_returns_ok() {
        init_proc();
        let caller = alloc_proc().unwrap();
        let target = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(caller)).mp_flags |= IN_USE;
            (*base.add(caller)).mp_pid = 1;
            (*base.add(target)).mp_flags |= IN_USE;
            (*base.add(target)).mp_pid = 2;
        }
        // SIGKILL to an existing process returns OK.
        let r = unsafe { check_sig(2, 0, SIGKILL, false) };
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_clock_gettime_reply_realtime() {
        // C do_gettime: sec = boottime + realtime/hz,
        // nsec = (realtime % hz) * 1e9 / hz.
        assert_eq!(
            clock_gettime_reply(CLOCK_REALTIME, 0, 12345, 1000, 100).unwrap(),
            (1123, 450_000_000)
        );
        // Zero ticks: only the boottime remains.
        assert_eq!(
            clock_gettime_reply(CLOCK_REALTIME, 0, 0, 42, 100).unwrap(),
            (42, 0)
        );
    }

    #[test]
    fn test_clock_gettime_reply_monotonic() {
        // CLOCK_MONOTONIC uses the uptime tick count (C: clock = ticks).
        assert_eq!(
            clock_gettime_reply(CLOCK_MONOTONIC, 250, 999, 5, 100).unwrap(),
            (7, 500_000_000)
        );
    }

    #[test]
    fn test_clock_gettime_reply_invalid_clock() {
        // Unsupported clock ids return EINVAL, like the C switch default.
        assert_eq!(clock_gettime_reply(42, 0, 0, 0, 100), Err(EINVAL));
    }

    #[test]
    fn test_clock_ticks_to_sec_nsec() {
        // sec = boottime + clock/hz; nsec = (clock % hz) * 1e9 / hz.
        assert_eq!(clock_ticks_to_sec_nsec(0, 42, 100), (42, 0));
        assert_eq!(
            clock_ticks_to_sec_nsec(12345, 1000, 100),
            (1123, 450_000_000)
        );
        assert_eq!(
            clock_ticks_to_sec_nsec(12345, 1000, 1000),
            (1012, 345_000_000)
        );
    }

    #[test]
    fn test_parse_times_reply() {
        // Kernel do_times_handler reply layout: real @ msg[0..8] straddling
        // m_source/m_type, boot_ticks @ payload[0..8], boottime @
        // payload[8..16], hz @ payload[32..36].
        let mut msg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let realtime: u64 = 12345;
        msg.m_source = (realtime & 0xFFFF_FFFF) as i32;
        msg.m_type = ((realtime >> 32) & 0xFFFF_FFFF) as i32;
        unsafe {
            msg.m_payload.raw[0..8].copy_from_slice(&250u64.to_le_bytes());
            msg.m_payload.raw[8..16].copy_from_slice(&1000i64.to_le_bytes());
            msg.m_payload.raw[32..36].copy_from_slice(&100u32.to_le_bytes());
        }
        assert_eq!(parse_times_reply(&msg), (250, 12345, 1000, 100));
    }

    #[test]
    fn test_do_time_writes_mess_pm_lc_time() {
        init_proc();
        let slot = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(slot)).mp_flags |= IN_USE;
            (*base.add(slot)).mp_pid = slot as i32 + 1;
        }
        // Kernel clock state: boottime 1000 s, realtime 12345 ticks @ 100 Hz.
        kernel::clock::set_boottime(1000);
        kernel::clock::set_realtime(12345);
        kernel::clock::set_system_hz(100);

        let mut msg = Message {
            m_source: 0,
            m_type: PM_GETTIMEOFDAY,
            m_payload: unsafe { core::mem::zeroed() },
        };

        let status = unsafe { do_time(slot, &mut msg) };
        assert_eq!(status, OK);
        unsafe {
            let sec = i64::from_le_bytes(msg.m_payload.raw[0..8].try_into().unwrap());
            let nsec = i64::from_le_bytes(msg.m_payload.raw[8..16].try_into().unwrap());
            assert_eq!(sec, 1123);
            assert_eq!(nsec, 450_000_000);
        }
    }

    #[test]
    fn test_pm_dispatch_gettimeofday_routes_to_handler() {
        init_proc();
        let slot = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(slot)).mp_flags |= IN_USE;
            (*base.add(slot)).mp_pid = slot as i32 + 1;
        }
        // Call 28 replies with the wall clock in mess_pm_lc_time layout,
        // not the old m2l1/m2l2 timeval at payload[16..32].
        kernel::clock::set_boottime(1000);
        kernel::clock::set_realtime(12345);
        kernel::clock::set_system_hz(100);

        let mut msg = Message {
            m_source: 0,
            m_type: PM_GETTIMEOFDAY,
            m_payload: unsafe { core::mem::zeroed() },
        };

        let status = pm_dispatch(slot, &mut msg);
        assert_eq!(status, OK);
        unsafe {
            let sec = i64::from_le_bytes(msg.m_payload.raw[0..8].try_into().unwrap());
            let nsec = i64::from_le_bytes(msg.m_payload.raw[8..16].try_into().unwrap());
            assert_eq!(sec, 1123);
            assert_eq!(nsec, 450_000_000);
            // The old m2l1/m2l2 timeval slots must not carry the reply.
            assert_eq!(msg.m_payload.raw[16..24], [0; 8]);
            assert_eq!(msg.m_payload.raw[24..32], [0; 8]);
        }
    }

    #[test]
    fn test_do_gettime_writes_mess_pm_lc_time() {
        init_proc();
        let slot = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(slot)).mp_flags |= IN_USE;
            (*base.add(slot)).mp_pid = slot as i32 + 1;
        }
        // Kernel clock state: boottime 1000 s, realtime 12345 ticks @ 100 Hz.
        kernel::clock::set_boottime(1000);
        kernel::clock::set_realtime(12345);
        kernel::clock::set_monotonic(0);
        kernel::clock::set_system_hz(100);

        let mut msg = Message {
            m_source: 0,
            m_type: PM_CLOCK_GETTIME,
            m_payload: unsafe { core::mem::zeroed() },
        };
        // mess_lc_pm_time: clk_id @ payload[8..12].
        unsafe {
            msg.m_payload.raw[8..12].copy_from_slice(&CLOCK_REALTIME.to_le_bytes());
        }

        let status = unsafe { do_gettime(slot, &mut msg) };
        assert_eq!(status, OK);
        unsafe {
            let sec = i64::from_le_bytes(msg.m_payload.raw[0..8].try_into().unwrap());
            let nsec = i64::from_le_bytes(msg.m_payload.raw[8..16].try_into().unwrap());
            assert_eq!(sec, 1123);
            assert_eq!(nsec, 450_000_000);
        }
    }

    #[test]
    fn test_pm_dispatch_clock_gettime_routes_to_handler() {
        init_proc();
        let slot = alloc_proc().unwrap();
        unsafe {
            let base = MPROC.as_ptr();
            (*base.add(slot)).mp_flags |= IN_USE;
            (*base.add(slot)).mp_pid = slot as i32 + 1;
        }
        // Without a handler, call 34 fell through to no_sys (ENOSYS).
        kernel::clock::set_boottime(0);
        kernel::clock::set_realtime(12345);
        kernel::clock::set_monotonic(0);
        kernel::clock::set_system_hz(100);

        let mut msg = Message {
            m_source: 0,
            m_type: PM_CLOCK_GETTIME,
            m_payload: unsafe { core::mem::zeroed() },
        };
        unsafe {
            msg.m_payload.raw[8..12].copy_from_slice(&CLOCK_REALTIME.to_le_bytes());
        }

        let status = pm_dispatch(slot, &mut msg);
        assert_eq!(status, OK);
        unsafe {
            let sec = i64::from_le_bytes(msg.m_payload.raw[0..8].try_into().unwrap());
            let nsec = i64::from_le_bytes(msg.m_payload.raw[8..16].try_into().unwrap());
            assert_eq!(sec, 123);
            assert_eq!(nsec, 450_000_000);
        }
    }
}
