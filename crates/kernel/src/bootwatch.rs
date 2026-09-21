//! Boot-progress watch: did a userland exec actually run?
//!
//! A boot that has stopped and a boot that is idle look the same from the run queues — a wedged
//! process is not runnable, so the system reads as quiescent either way. That is how a boot which
//! stops at `init: starting shell...` passed every gate: the suite printed its summary, exited QEMU,
//! and nothing had asked whether anyone had ever run.
//!
//! This module is the asking. A userland exec of a file-backed image is noted here, the first syscall
//! from that process marks the boundary crossed, and a tick deadline turns the absence of that
//! syscall into a verdict. The boot-test build arms it; a production kernel pays one relaxed atomic
//! load per hook and nothing else.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);
/// The process that last exec'd a userland image, or -1.
static EXEC_PNR: AtomicI32 = AtomicI32::new(-1);
/// Set when `EXEC_PNR` has issued a syscall — it left user mode, so the image ran.
static RAN: AtomicBool = AtomicBool::new(false);
/// Monotonic tick at which "exec'd but never ran" becomes a failure.
static DEADLINE: AtomicU64 = AtomicU64::new(0);
/// The verdict callback, as an address; 0 = none.
static VERDICT: AtomicUsize = AtomicUsize::new(0);

/// Arm the watch.
///
/// `deadline` is the monotonic tick to give up at, and `verdict` is called exactly once — with
/// `true` as soon as the exec'd process makes a syscall, with `false` when the deadline arrives
/// first. Called by the boot-test build after its post-mount phase.
///
/// # Safety
///
/// `verdict` is called from the timer interrupt and must not return: the boot-test verdict prints
/// its result and exits QEMU.
pub fn arm(deadline: u64, verdict: fn(bool)) {
    DEADLINE.store(deadline, Ordering::Relaxed);
    VERDICT.store(verdict as usize, Ordering::Relaxed);
    ARMED.store(true, Ordering::Release);
}

/// A userland `execve` of a file-backed image installed a new image for `p_nr`.
///
/// The boot loader's own process loads do not come through here, which is the point: those install
/// an image directly and never take the VFS path this watch exists to cover.
pub fn note_exec(p_nr: i32) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    EXEC_PNR.store(p_nr, Ordering::Relaxed);
    RAN.store(false, Ordering::Relaxed);
}

/// `p_nr` entered the kernel. If it is the process we are watching, the image it exec'd reached user
/// mode and left it again — which is the boundary, crossed.
pub fn note_syscall(p_nr: i32) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    if EXEC_PNR.load(Ordering::Relaxed) == p_nr {
        RAN.store(true, Ordering::Relaxed);
    }
}

/// One monotonic tick, from the clock interrupt.
pub fn tick(now: u64) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    if RAN.load(Ordering::Relaxed) {
        fire(true);
    } else if now >= DEADLINE.load(Ordering::Relaxed) {
        fire(false);
    }
}

/// Hand the outcome over once, then disarm.
fn fire(passed: bool) {
    ARMED.store(false, Ordering::Relaxed);
    let addr = VERDICT.load(Ordering::Relaxed);
    if addr != 0 {
        let verdict: fn(bool) = unsafe { core::mem::transmute(addr) };
        verdict(passed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU32;

    static CALLED: AtomicU32 = AtomicU32::new(0);
    static LAST: AtomicBool = AtomicBool::new(false);

    fn record(passed: bool) {
        LAST.store(passed, Ordering::Relaxed);
        CALLED.fetch_add(1, Ordering::Relaxed);
    }

    fn called() -> u32 {
        CALLED.load(Ordering::Relaxed)
    }

    /// One test, walked in order: the watch's state is a single global, so the cases share it and
    /// running them as separate tests would race (which is how this was written first, and how it
    /// failed) — a `TestLockGuard` would serialise them, but a sequence is what this is anyway.
    #[test]
    fn watch_sequence() {
        // Disarmed, nothing is noticed: this is what a production kernel pays.
        note_exec(10);
        note_syscall(10);
        tick(u64::MAX);
        assert_eq!(called(), 0, "a disarmed watch must ignore everything");

        // Armed, another process's syscall is not progress, so the deadline decides.
        arm(100, record);
        note_exec(10);
        note_syscall(9);
        tick(50);
        assert_eq!(called(), 0, "another process's syscall is not progress");
        tick(99);
        assert_eq!(called(), 0, "the deadline has not arrived");
        tick(100);
        assert_eq!(called(), 1, "exec with no run must fail at the deadline");
        assert!(!LAST.load(Ordering::Relaxed));

        // Armed, the watched process entering the kernel crosses the boundary — and it fires once.
        arm(100, record);
        note_exec(10);
        note_syscall(10);
        tick(10);
        assert_eq!(called(), 2);
        assert!(LAST.load(Ordering::Relaxed));
        tick(10);
        tick(200);
        assert_eq!(called(), 2, "a watch fires exactly once");
    }
}
