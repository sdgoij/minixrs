//! Process table and boot image — adapted from `minix/kernel/table.c`
//! and `minix/kernel/proc.h`.
//!
//! Defines the global process table, endpoint encoding, process number
//! mapping, validity checks, run queue, and boot-time initialization.
//!
//! **x86_64 differences from i386:**
//! - `endpoint_t = i32` same encoding; `_ENDPOINT_GENERATION_SHIFT = 15`
//! - Process table is byte storage reinterpreted as `Proc` (avoids Rust
//!   2024 `static_mut_refs` issues with large arrays of complex types)

use core::cell::UnsafeCell;
use core::mem::size_of;

use arch_common::com::PM_PROC_NR;

use crate::r#priv::{PPRIV_ADDR, PRIV, Priv, PrivFlags};
use crate::proc::*;

// Constants

/// Size of the process table in bytes.
const PROC_TABLE_SIZE: usize = size_of::<Proc>() * NR_PROCS_TOTAL;

/// Endpoint encoding constants.
const EP_GENERATION_SHIFT: i32 = 15;
const EP_GENERATION_SIZE: i32 = 1 << EP_GENERATION_SHIFT;
const MAX_NR_TASKS: i32 = 1023;

/// Maximum generation number.
pub const EP_MAX_GENERATION: i32 = i32::MAX / EP_GENERATION_SIZE - 1;

// Process Table

/// Aligned byte array for process table storage.
#[repr(C, align(64))]
struct AlignedTable {
    data: [u8; PROC_TABLE_SIZE],
}

struct AlignedTableCell(UnsafeCell<AlignedTable>);
unsafe impl Sync for AlignedTableCell {}
impl AlignedTableCell {
    const fn new(val: AlignedTable) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut AlignedTable {
        self.0.get()
    }
}

/// Raw process table storage (BSS, cache-line aligned).
///
/// Accessed through `proc_addr()` which maps process numbers to slots.
/// Layout: tasks occupy indices [0, NR_TASKS), user procs occupy
/// indices [NR_TASKS, NR_PROCS_TOTAL).
static PROC_TABLE_ALIGNED: AlignedTableCell = AlignedTableCell::new(AlignedTable {
    data: [0u8; PROC_TABLE_SIZE],
});

/// Return a raw pointer to the process table as `[Proc]` (unsized slice).
fn proc_table_ptr() -> *mut Proc {
    PROC_TABLE_ALIGNED.get().cast::<u8>().cast::<Proc>()
}

/// Get a pointer to the process at index `i` in the table.
///
/// # Safety
///
/// `i` must be < `NR_PROCS_TOTAL`.
unsafe fn proc_index(i: usize) -> *mut Proc {
    unsafe { proc_table_ptr().add(i) }
}

/// Map process number to `Proc` pointer.
///
/// For negative `n` (kernel tasks): returns `&proc[NR_TASKS + n]`.
/// For non-negative `n` (user processes): returns `&proc[NR_TASKS + n]`.
pub fn proc_addr(n: i32) -> *mut Proc {
    let idx = (NR_TASKS as i32 + n) as usize;
    if idx < NR_PROCS_TOTAL {
        unsafe { proc_index(idx) }
    } else {
        core::ptr::null_mut()
    }
}

/// Return a pointer to the start of the process table.
pub fn proc_table_base() -> *mut Proc {
    unsafe { proc_index(0) }
}

/// Constant version of `proc_addr` (const fn, but returns a raw pointer).
/// Only valid for compile-time-known process numbers.
pub fn proc_addr_const(n: i32) -> *const Proc {
    proc_addr(n) as *const Proc
}

// Address constants (as functions)

pub fn beg_proc_addr() -> *mut Proc {
    unsafe { proc_index(0) }
}

pub fn beg_user_addr() -> *mut Proc {
    unsafe { proc_index(NR_TASKS) }
}

pub fn end_proc_addr() -> *mut Proc {
    unsafe { proc_index(NR_PROCS_TOTAL) }
}

// Endpoint encoding

/// Encode a generation number and process slot into an endpoint.
pub const fn make_endpoint(r#gen: i32, slot: i32) -> i32 {
    (r#gen << EP_GENERATION_SHIFT) + slot
}

/// Extract the generation number from an endpoint.
pub const fn endpoint_gen(ep: i32) -> i32 {
    (ep + MAX_NR_TASKS) >> EP_GENERATION_SHIFT
}

/// Extract the process slot number from an endpoint.
pub const fn endpoint_slot(ep: i32) -> i32 {
    ((ep + MAX_NR_TASKS) & (EP_GENERATION_SIZE - 1)) - MAX_NR_TASKS
}

// Validity checks

/// Check if a process number is valid.
pub fn is_ok_proc_nr(n: i32) -> bool {
    let idx = NR_TASKS as i32 + n;
    (idx as usize) < NR_PROCS_TOTAL
}

/// Check if a process is a kernel task (negative process number).
pub fn is_kernel_nr(n: i32) -> bool {
    n < 0
}

/// Check if a process pointer refers to a kernel task.
///
/// # Safety
///
/// `rp` must point into the process table.
pub unsafe fn is_kernel_proc(rp: *const Proc) -> bool {
    rp < beg_user_addr() as *const Proc
}

/// Check if a process is a user process.
///
/// # Safety
///
/// `rp` must point into the process table.
pub unsafe fn is_user_proc(rp: *const Proc) -> bool {
    unsafe { !is_kernel_proc(rp) }
}

/// Check if a process is empty (slot free).
///
/// # Safety
///
/// `rp` must point to a valid `Proc` within the process table.
pub unsafe fn is_empty_proc(rp: *const Proc) -> bool {
    unsafe {
        let flags = (*rp)
            .p_rts_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        // NO_ENDPOINT may also be set by clear_endpoint after exit.
        // Accept SLOT_FREE with or without NO_ENDPOINT.
        flags == RtsFlags::SLOT_FREE.bits()
            || flags == (RtsFlags::SLOT_FREE.bits() | RtsFlags::NO_ENDPOINT.bits())
    }
}

/// Check if an endpoint is valid.
/// Returns `true` and optionally the extracted process number if valid.
pub fn is_ok_endpoint(ep: i32) -> bool {
    let g = endpoint_gen(ep);
    if !(0..=EP_MAX_GENERATION).contains(&g) {
        return false;
    }
    let p = endpoint_slot(ep);
    is_ok_proc_nr(p)
}

/// Look up a process by endpoint. Returns null if not found.
pub fn endpoint_lookup(ep: i32) -> *mut Proc {
    if !is_ok_endpoint(ep) {
        return core::ptr::null_mut();
    }
    let p = endpoint_slot(ep);
    let rp = proc_addr(p);
    unsafe {
        if is_empty_proc(rp) {
            return core::ptr::null_mut();
        }
    }
    rp
}

// Boot image

/// Boot-time process descriptor.
/// Number of boot processes.
pub const NR_BOOT_PROCS: usize = 17;

/// Boot image entry.
#[derive(Debug, Clone, Copy)]
pub struct BootImage {
    /// Process number/endpoint.
    pub proc_nr: i32,
    /// Process name.
    pub name: &'static str,
}

/// The boot image — defines which processes are started at boot.
///
/// Order matches `minix/kernel/table.c` and must agree with the boot
/// image layout. Kernel tasks come first (negative numbers), then
/// system processes.
pub static BOOT_IMAGE: [BootImage; NR_BOOT_PROCS] = [
    // Kernel tasks (5)
    BootImage {
        proc_nr: -5,
        name: "asyncm",
    },
    BootImage {
        proc_nr: -4,
        name: "idle",
    },
    BootImage {
        proc_nr: -3,
        name: "clock",
    },
    BootImage {
        proc_nr: -2,
        name: "system",
    },
    BootImage {
        proc_nr: -1,
        name: "kernel",
    },
    // System processes (11)
    BootImage {
        proc_nr: 6,
        name: "ds",
    }, // DS_PROC_NR
    BootImage {
        proc_nr: 2,
        name: "rs",
    }, // RS_PROC_NR
    BootImage {
        proc_nr: 0,
        name: "pm",
    }, // PM_PROC_NR
    BootImage {
        proc_nr: 4,
        name: "sched",
    }, // SCHED_PROC_NR
    BootImage {
        proc_nr: 1,
        name: "vfs",
    }, // VFS_PROC_NR
    BootImage {
        proc_nr: 3,
        name: "memory",
    }, // MEM_PROC_NR
    BootImage {
        proc_nr: 5,
        name: "tty",
    }, // TTY_PROC_NR
    BootImage {
        proc_nr: 7,
        name: "mfs",
    }, // MFS_PROC_NR
    BootImage {
        proc_nr: 8,
        name: "vm",
    }, // VM_PROC_NR
    BootImage {
        proc_nr: 9,
        name: "pfs",
    }, // PFS_PROC_NR
    BootImage {
        proc_nr: 10,
        name: "init",
    }, // INIT_PROC_NR
    BootImage {
        proc_nr: 11,
        name: "ramdisk",
    }, // RAMDISK_PROC_NR
];

// Run queue

/// Multi-level run queue.
///
/// 16 priority levels (0 = highest, 15 = lowest). Each level is a
/// singly-linked list threaded through `Proc::p_nextready`.
pub struct RunQueue {
    /// Head pointers for each priority level.
    pub head: [*mut Proc; NR_SCHED_QUEUES],
    /// Tail pointers for each priority level.
    pub tail: [*mut Proc; NR_SCHED_QUEUES],
}

impl RunQueue {
    /// Create a new empty run queue.
    pub const fn new() -> Self {
        Self {
            head: [core::ptr::null_mut(); NR_SCHED_QUEUES],
            tail: [core::ptr::null_mut(); NR_SCHED_QUEUES],
        }
    }

    /// Check if a specific priority queue is empty.
    pub fn is_empty(&self, priority: usize) -> bool {
        if priority >= NR_SCHED_QUEUES {
            return true;
        }
        self.head[priority].is_null()
    }

    /// Check if all queues are empty.
    pub fn all_empty(&self) -> bool {
        self.head.iter().all(|&h| h.is_null())
    }

    /// Get the highest priority level that has a ready process.
    pub fn highest_ready(&self) -> Option<usize> {
        (0..NR_SCHED_QUEUES).find(|&q| !self.head[q].is_null())
    }
}

impl Default for RunQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrapper for `RunQueue`.
pub struct RunQueueCell(UnsafeCell<RunQueue>);
unsafe impl Sync for RunQueueCell {}
impl RunQueueCell {
    pub const fn new(val: RunQueue) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut RunQueue {
        self.0.get()
    }
}

/// Global run queue.
pub static RUN_QUEUE: RunQueueCell = RunQueueCell::new(RunQueue::new());

// proc_init

/// Initialize the process table.
///
/// Must be called once during kernel boot, before any process access.
/// Sets up all process slots with magic numbers, endpoints, and
/// privilege structures.
///
/// # Safety
///
/// Must be called exactly once on the BSP, before any concurrent access.
pub unsafe fn proc_init() {
    // Initialize each slot
    for i in 0..NR_PROCS_TOTAL {
        unsafe {
            let rp = proc_index(i);
            // Set magic number for pointer validation
            (*rp).p_magic = PMAGIC;
            // Clear run queue link (prevents stale pointers between tests)
            (*rp).p_nextready = core::ptr::null_mut();
            // Clear misc flags (prevents REPLY_PEND leakage between tests)
            (*rp)
                .p_misc_flags
                .store(0, core::sync::atomic::Ordering::Relaxed);
            // Mark slot as free
            (*rp).p_rts_flags.store(
                RtsFlags::SLOT_FREE.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    // Initialize boot processes
    for bi in &BOOT_IMAGE {
        unsafe {
            let rp = proc_addr(bi.proc_nr);
            if rp.is_null() {
                continue;
            }
            // Clear SLOT_FREE flag
            (*rp).p_rts_flags.store(
                RtsFlags::empty().bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            // Set process number
            (*rp).p_nr = bi.proc_nr;
            // Set priority: idle at lowest, kernel tasks at high
            if bi.proc_nr == arch_common::com::IDLE {
                (*rp).p_priority = 15; // NR_SCHED_QUEUES - 1 (lowest)
            } else {
                (*rp).p_priority = 0;
            }
            // Set endpoint (generation 0, so ep == proc_nr for hardcoded values)
            (*rp).p_endpoint = make_endpoint(0, bi.proc_nr);
            // Copy name
            let name_bytes = bi.name.as_bytes();
            let name_len = name_bytes.len().min(PROC_NAME_LEN - 1);
            for (j, &b) in name_bytes.iter().enumerate().take(name_len) {
                (*rp).p_name[j] = b;
            }
            (*rp).p_name[name_len] = 0; // null-terminate
        }
    }

    // Initialize privilege table pointers
    unsafe {
        let base = PRIV.get() as *mut Priv;
        let ptrs_base = PPRIV_ADDR.get() as *mut *mut Priv;
        for i in 0..NR_SYS_PROCS {
            // SAFETY: i < NR_SYS_PROCS, the array is exactly that size.
            *ptrs_base.add(i) = base.add(i);
        }
    }

    unsafe {
        for (i, bi) in BOOT_IMAGE.iter().enumerate() {
            if i >= NR_SYS_PROCS {
                break;
            }
            let priv_ptr = (*PPRIV_ADDR.get())[i];
            if priv_ptr.is_null() {
                continue;
            }
            // Set up basic privilege fields
            (*priv_ptr).s_proc_nr = bi.proc_nr;
            (*priv_ptr).s_id = i as i16;
            (*priv_ptr).s_flags = PrivFlags::SYS_PROC | PrivFlags::PREEMPTIBLE;
            // Allow all kernel calls (IPC, safecopy, etc.) for boot processes.
            for chunk in (*priv_ptr).s_k_call_mask.iter_mut() {
                *chunk = !0u32;
            }

            // Set the signal manager to PM so that do_getksig_handler
            // finds processes with SIGNALED set (matching C: PM registers
            // as sig_mgr via SYS_PRIVCTL during init; we set it directly
            // since we don't have that protocol yet).
            (*priv_ptr).s_sig_mgr = PM_PROC_NR;

            // Link the privilege structure to the process.
            // This enables notification delivery (mini_notify stores
            // pending notifications in s_notify_pending via p_priv).
            let rp = proc_addr(bi.proc_nr);
            if !rp.is_null() {
                (*rp).p_priv = priv_ptr;
            }
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proc_addr_tasks() {
        // Task -1 (KERNEL/HARDWARE) should be at index 4
        let rp = proc_addr(-1);
        assert!(!rp.is_null());
    }

    #[test]
    fn test_proc_addr_user() {
        // Process 0 (PM) should be at index 5
        let rp = proc_addr(0);
        assert!(!rp.is_null());
    }

    #[test]
    fn test_proc_addr_invalid() {
        // Process number out of range
        let rp = proc_addr(256);
        assert!(rp.is_null());
        let rp = proc_addr(-6);
        assert!(rp.is_null());
    }

    #[test]
    fn test_beg_end_addr_order() {
        assert!(beg_proc_addr() <= end_proc_addr());
        assert!(beg_user_addr() > beg_proc_addr());
    }

    #[test]
    fn test_endpoint_make_and_extract() {
        for g in 0..=3 {
            for slot in -5..=10 {
                let ep = make_endpoint(g, slot);
                assert_eq!(
                    endpoint_gen(ep),
                    g,
                    "generation mismatch at gen={}, slot={}",
                    g,
                    slot
                );
                assert_eq!(
                    endpoint_slot(ep),
                    slot,
                    "slot mismatch at gen={}, slot={}",
                    g,
                    slot
                );
            }
        }
    }

    #[test]
    fn test_endpoint_zero_gen_roundtrip() {
        // With generation 0, endpoint == proc_nr
        assert_eq!(make_endpoint(0, -1), -1);
        assert_eq!(make_endpoint(0, 0), 0);
        assert_eq!(make_endpoint(0, 5), 5);
        assert_eq!(endpoint_gen(-1), 0);
        assert_eq!(endpoint_slot(-1), -1);
    }

    #[test]
    fn test_is_ok_proc_nr() {
        assert!(is_ok_proc_nr(-5));
        assert!(is_ok_proc_nr(-1));
        assert!(is_ok_proc_nr(0));
        assert!(is_ok_proc_nr(255));
        assert!(!is_ok_proc_nr(256));
        assert!(!is_ok_proc_nr(-6));
    }

    #[test]
    fn test_is_kernel_nr() {
        assert!(is_kernel_nr(-1));
        assert!(is_kernel_nr(-5));
        assert!(!is_kernel_nr(0));
        assert!(!is_kernel_nr(1));
    }

    #[test]
    fn test_boot_image_count() {
        assert_eq!(BOOT_IMAGE.len(), NR_BOOT_PROCS);
    }

    #[test]
    fn test_boot_image_names() {
        assert_eq!(BOOT_IMAGE[0].name, "asyncm");
        assert_eq!(BOOT_IMAGE[1].name, "idle");
        assert_eq!(BOOT_IMAGE[4].name, "kernel");
        assert_eq!(BOOT_IMAGE[15].name, "init");
    }

    #[test]
    fn test_run_queue_new() {
        let rq = RunQueue::new();
        assert!(rq.all_empty());
        for q in 0..NR_SCHED_QUEUES {
            assert!(rq.is_empty(q));
        }
    }

    #[test]
    fn test_run_queue_highest_ready() {
        let rq = RunQueue::new();
        assert!(rq.highest_ready().is_none());
    }

    #[test]
    fn test_proc_init_sets_magic() {
        unsafe {
            proc_init();
            // Check a task slot
            let rp = proc_addr(-1);
            assert!(!rp.is_null());
            assert_eq!((*rp).p_magic, PMAGIC);
            // Check a user slot
            let rp = proc_addr(0);
            assert!(!rp.is_null());
            assert_eq!((*rp).p_magic, PMAGIC);
        }
    }

    #[test]
    fn test_proc_init_boot_procs_not_free() {
        unsafe {
            proc_init();
            for bi in &BOOT_IMAGE {
                let rp = proc_addr(bi.proc_nr);
                assert!(!rp.is_null());
                // Should NOT have SLOT_FREE
                let flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                assert!(
                    flags & RtsFlags::SLOT_FREE.bits() == 0,
                    "boot process {} should not be free",
                    bi.name
                );
            }
        }
    }

    #[test]
    fn test_proc_init_non_boot_slots_free() {
        unsafe {
            proc_init();
            // Check a slot that should be free (e.g., process 100)
            let rp = proc_addr(100);
            assert!(!rp.is_null());
            let flags = (*rp)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            assert_eq!(flags, RtsFlags::SLOT_FREE.bits());
        }
    }

    #[test]
    fn test_proc_init_sets_names() {
        unsafe {
            proc_init();
            let rp = proc_addr(0); // PM
            let name: &[u8] = &(*rp).p_name;
            // Find null terminator
            let len = name.iter().position(|&c| c == 0).unwrap_or(PROC_NAME_LEN);
            let name_str = core::str::from_utf8(&name[..len]).unwrap_or("");
            assert_eq!(name_str, "pm");
        }
    }

    #[test]
    fn test_endpoint_lookup_nonexistent() {
        unsafe {
            proc_init();
            // Process 100 should be free, lookup returns null
            let rp = endpoint_lookup(make_endpoint(0, 100));
            assert!(rp.is_null());
        }
    }

    #[test]
    fn test_endpoint_lookup_boot_proc() {
        unsafe {
            proc_init();
            let rp = endpoint_lookup(make_endpoint(0, 0)); // PM
            assert!(!rp.is_null());
        }
    }

    #[test]
    fn test_proc_init_sets_sig_mgr() {
        unsafe {
            proc_init();
            for bi in &BOOT_IMAGE {
                let rp = proc_addr(bi.proc_nr);
                assert!(!rp.is_null(), "{}: null proc", bi.name);
                let priv_ptr = (*rp).p_priv;
                assert!(!priv_ptr.is_null(), "{}: p_priv is null", bi.name);
                assert_eq!(
                    (*priv_ptr).s_sig_mgr,
                    PM_PROC_NR,
                    "{}: s_sig_mgr should be PM_PROC_NR ({})",
                    bi.name,
                    PM_PROC_NR
                );
            }
        }
    }
}
