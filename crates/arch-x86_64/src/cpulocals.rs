//! Per-CPU local variables — adapted from `minix/kernel/cpulocals.h`
//!
//! Provides per-CPU variable storage for the kernel's scheduling, idle,
//! FPU, and TSC accounting state. Currently single-CPU only (SMP support
//! can be added later by making `CPU_LOCAL_VARS` an array indexed by
//! CPU ID).
//!
//! **x86_64 differences from i386:**
//! - All pointers are 8 bytes (embedded idle_proc buffer sized accordingly)
//! - All raw proc pointers use `*mut core::ffi::c_void`, cast at use site
//! - `idle_proc` is an opaque byte buffer embedded by value (matching the
//!   C `struct proc idle_proc` layout). Accessor casts to `*mut c_void`.
//! - Run queue arrays sized by `NR_SCHED_QUEUES` (16)

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};

/// Number of scheduling priority queues.
pub const NR_SCHED_QUEUES: usize = 16;

/// Size of the embedded `idle_proc` buffer (must be >= sizeof(Proc)).
/// The `kernel` crate defines `Proc` and this is the arch crate, so we
/// use an opaque buffer sized generously enough for the x86_64 `Proc`
/// struct (regs, flags, scheduling fields, etc.).
pub const IDLE_PROC_SIZE: usize = 1024;

// CpuLocalVars

/// Per-CPU local variables — mirrors the layout of `struct cpulocal_vars`
/// from `minix/kernel/cpulocals.h`.
///
/// # Safety
///
/// All fields are accessed through raw pointers or atomic operations.
/// The `idle_proc` buffer must not be read as a `Proc` until the kernel
/// has initialized it.
#[repr(C)]
pub struct CpuLocalVars {
    /// Pointer to the currently running process.
    pub proc_ptr: *mut core::ffi::c_void,
    /// Process to bill for clock ticks.
    pub bill_ptr: *mut core::ffi::c_void,

    /// Embedded idle process struct (opaque buffer).
    /// Cast to `*mut Proc` via `idle_proc_ptr()`.
    pub idle_proc: [u8; IDLE_PROC_SIZE],

    /// Whether a page fault is being handled (recursive fault detection).
    pub pagefault_handled: i32,

    /// Which process's page tables are currently loaded.
    /// This is separate from `proc_ptr` because some processes share the
    /// kernel page tables and don't have per-process page tables.
    pub ptproc: *mut core::ffi::c_void,

    /// Ready list head pointers (one per priority queue).
    pub run_q_head: [*mut core::ffi::c_void; NR_SCHED_QUEUES],
    /// Ready list tail pointers (one per priority queue).
    pub run_q_tail: [*mut core::ffi::c_void; NR_SCHED_QUEUES],

    /// Whether this CPU is idle (atomic for SMP safety).
    pub cpu_is_idle: AtomicI32,
    /// Whether the idle loop was interrupted (for profiling).
    pub idle_interrupted: AtomicI32,

    /// Timestamp when time accounting was last switched.
    pub tsc_ctr_switch: u64,
    /// Last TSC value sent in out-of-queue message to the scheduler.
    pub cpu_last_tsc: u64,
    /// Last idle TSC value sent in out-of-queue message to the scheduler.
    pub cpu_last_idle: u64,

    /// Whether this CPU has an FPU.
    pub fpu_presence: u8,
    /// _pad to align fpu_owner.
    _pad: [u8; 7],
    /// Who owns the FPU on this CPU.
    pub fpu_owner: *mut core::ffi::c_void,
}

impl Default for CpuLocalVars {
    fn default() -> Self {
        Self {
            proc_ptr: core::ptr::null_mut(),
            bill_ptr: core::ptr::null_mut(),
            idle_proc: [0u8; IDLE_PROC_SIZE],
            pagefault_handled: 0,
            ptproc: core::ptr::null_mut(),
            run_q_head: [core::ptr::null_mut(); NR_SCHED_QUEUES],
            run_q_tail: [core::ptr::null_mut(); NR_SCHED_QUEUES],
            cpu_is_idle: AtomicI32::new(0),
            idle_interrupted: AtomicI32::new(0),
            tsc_ctr_switch: 0,
            cpu_last_tsc: 0,
            cpu_last_idle: 0,
            fpu_presence: 0,
            _pad: [0u8; 7],
            fpu_owner: core::ptr::null_mut(),
        }
    }
}

impl CpuLocalVars {
    /// Return a pointer to the embedded idle process struct.
    #[must_use]
    pub fn idle_proc_ptr(&self) -> *mut core::ffi::c_void {
        &self.idle_proc as *const _ as *mut core::ffi::c_void
    }
}

// CpuLocalStorage

/// Global wrapper around per-CPU local variables.
///
/// Provides atomic-once initialization and accessors. Single-CPU layout:
/// there is one global `CpuLocalVars` instance. For SMP, this becomes an
/// array indexed by CPU ID and accessed via `swapgs` + GS segment.
pub struct CpuLocalStorage {
    initialized: AtomicBool,
    vars: AtomicPtr<CpuLocalVars>,
}

impl CpuLocalStorage {
    /// Create a new, uninitialized storage wrapper.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            vars: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// Initialize with a pointer to the underlying storage.
    ///
    /// # Safety
    ///
    /// Must be called exactly once, before any accessor. `storage` must
    /// outlive all accessor calls.
    pub unsafe fn init(&self, storage: *mut CpuLocalVars) {
        if self.initialized.swap(true, Ordering::SeqCst) {
            return;
        }
        self.vars.store(storage, Ordering::SeqCst);
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    unsafe fn vars_ref(&self) -> *mut CpuLocalVars {
        debug_assert!(
            self.initialized.load(Ordering::Relaxed),
            "CpuLocalStorage not initialized"
        );
        self.vars.load(Ordering::Relaxed)
    }

    // ── Field accessors ──────────────────────────────────────────────────
    //
    // Each accessor wraps raw pointer dereferences in an `unsafe` block
    // to satisfy `unsafe_op_in_unsafe_fn` (Rust 2024).

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn proc_ptr(&self) -> *mut core::ffi::c_void {
        unsafe { (*self.vars_ref()).proc_ptr }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_proc_ptr(&self, val: *mut core::ffi::c_void) {
        unsafe {
            (*self.vars_ref()).proc_ptr = val;
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn bill_ptr(&self) -> *mut core::ffi::c_void {
        unsafe { (*self.vars_ref()).bill_ptr }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_bill_ptr(&self, val: *mut core::ffi::c_void) {
        unsafe {
            (*self.vars_ref()).bill_ptr = val;
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn idle_proc_ptr(&self) -> *mut core::ffi::c_void {
        unsafe { (*self.vars_ref()).idle_proc_ptr() }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn pagefault_handled(&self) -> i32 {
        unsafe { (*self.vars_ref()).pagefault_handled }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_pagefault_handled(&self, val: i32) {
        unsafe {
            (*self.vars_ref()).pagefault_handled = val;
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn ptproc(&self) -> *mut core::ffi::c_void {
        unsafe { (*self.vars_ref()).ptproc }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_ptproc(&self, val: *mut core::ffi::c_void) {
        unsafe {
            (*self.vars_ref()).ptproc = val;
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn run_q_head_ptr(&self) -> *mut [*mut core::ffi::c_void; NR_SCHED_QUEUES] {
        unsafe { &raw mut (*self.vars_ref()).run_q_head }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn run_q_tail_ptr(&self) -> *mut [*mut core::ffi::c_void; NR_SCHED_QUEUES] {
        unsafe { &raw mut (*self.vars_ref()).run_q_tail }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn cpu_is_idle(&self) -> i32 {
        unsafe { (*self.vars_ref()).cpu_is_idle.load(Ordering::Relaxed) }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_cpu_is_idle(&self, val: i32) {
        unsafe {
            (*self.vars_ref()).cpu_is_idle.store(val, Ordering::Relaxed);
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn idle_interrupted(&self) -> i32 {
        unsafe { (*self.vars_ref()).idle_interrupted.load(Ordering::Relaxed) }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_idle_interrupted(&self, val: i32) {
        unsafe {
            (*self.vars_ref())
                .idle_interrupted
                .store(val, Ordering::Relaxed);
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn tsc_ctr_switch(&self) -> u64 {
        unsafe { (*self.vars_ref()).tsc_ctr_switch }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_tsc_ctr_switch(&self, val: u64) {
        unsafe {
            (*self.vars_ref()).tsc_ctr_switch = val;
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn cpu_last_tsc(&self) -> u64 {
        unsafe { (*self.vars_ref()).cpu_last_tsc }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_cpu_last_tsc(&self, val: u64) {
        unsafe {
            (*self.vars_ref()).cpu_last_tsc = val;
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn cpu_last_idle(&self) -> u64 {
        unsafe { (*self.vars_ref()).cpu_last_idle }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_cpu_last_idle(&self, val: u64) {
        unsafe {
            (*self.vars_ref()).cpu_last_idle = val;
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn fpu_presence(&self) -> u8 {
        unsafe { (*self.vars_ref()).fpu_presence }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_fpu_presence(&self, val: u8) {
        unsafe {
            (*self.vars_ref()).fpu_presence = val;
        }
    }

    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn fpu_owner(&self) -> *mut core::ffi::c_void {
        unsafe { (*self.vars_ref()).fpu_owner }
    }
    /// # Safety
    ///
    /// The storage must be initialized.
    pub unsafe fn set_fpu_owner(&self, val: *mut core::ffi::c_void) {
        unsafe {
            (*self.vars_ref()).fpu_owner = val;
        }
    }
}

impl Default for CpuLocalStorage {
    fn default() -> Self {
        Self::new()
    }
}

// Global instance

/// Global per-CPU local storage. Must be initialized before use.
pub static CPU_LOCAL_STORAGE: CpuLocalStorage = CpuLocalStorage::new();

/// Underlying storage for CPU local vars (lives in BSS, zeroed at boot).
static mut CPU_LOCAL_VARS: CpuLocalVars = CpuLocalVars {
    proc_ptr: core::ptr::null_mut(),
    bill_ptr: core::ptr::null_mut(),
    idle_proc: [0u8; IDLE_PROC_SIZE],
    pagefault_handled: 0,
    ptproc: core::ptr::null_mut(),
    run_q_head: [core::ptr::null_mut(); NR_SCHED_QUEUES],
    run_q_tail: [core::ptr::null_mut(); NR_SCHED_QUEUES],
    cpu_is_idle: AtomicI32::new(0),
    idle_interrupted: AtomicI32::new(0),
    tsc_ctr_switch: 0,
    cpu_last_tsc: 0,
    cpu_last_idle: 0,
    fpu_presence: 0,
    _pad: [0u8; 7],
    fpu_owner: core::ptr::null_mut(),
};

/// Initialize the per-CPU local variables.
///
/// Must be called once during kernel boot, before any accessor.
///
/// # Safety
///
/// Must be called exactly once on the BSP.
pub unsafe fn init_cpulocals() {
    // Storage must outlive all accessors — it's a static, so it does.
    let vars = core::ptr::addr_of_mut!(CPU_LOCAL_VARS);
    // SAFETY: Called exactly once on the BSP per safety contract.
    unsafe {
        CPU_LOCAL_STORAGE.init(vars);
    }
}

/// Release the FPU if it is owned by `proc`. Forces reload on next use.
///
/// # Safety
///
/// `proc` should point to a valid `Proc` or be null.
pub unsafe fn release_fpu(proc: *mut core::ffi::c_void) {
    unsafe {
        let owner = (*core::ptr::addr_of_mut!(CPU_LOCAL_VARS)).fpu_owner;
        if owner == proc {
            (*core::ptr::addr_of_mut!(CPU_LOCAL_VARS)).fpu_owner = core::ptr::null_mut();
        }
    }
}

/// # Safety
///
/// `CPU_LOCAL_STORAGE` must be initialized.
pub unsafe fn get_cpulocal_proc_ptr() -> *mut core::ffi::c_void {
    unsafe { CPU_LOCAL_STORAGE.proc_ptr() }
}

/// # Safety
///
/// `CPU_LOCAL_STORAGE` must be initialized.
pub unsafe fn set_cpulocal_proc_ptr(p: *mut core::ffi::c_void) {
    unsafe {
        CPU_LOCAL_STORAGE.set_proc_ptr(p);
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_cpu_local_vars_default() {
        let v = CpuLocalVars::default();
        assert!(v.proc_ptr.is_null());
        assert!(v.bill_ptr.is_null());
        assert_eq!(v.pagefault_handled, 0);
        assert!(v.ptproc.is_null());
        assert_eq!(v.cpu_is_idle.load(Ordering::Relaxed), 0);
        assert_eq!(v.idle_interrupted.load(Ordering::Relaxed), 0);
        assert_eq!(v.tsc_ctr_switch, 0);
        assert_eq!(v.cpu_last_tsc, 0);
        assert_eq!(v.cpu_last_idle, 0);
        assert_eq!(v.fpu_presence, 0);
        assert!(v.fpu_owner.is_null());
    }

    #[test]
    fn test_run_q_arrays_size() {
        let v = CpuLocalVars::default();
        assert_eq!(v.run_q_head.len(), NR_SCHED_QUEUES);
        assert_eq!(v.run_q_tail.len(), NR_SCHED_QUEUES);
    }

    #[test]
    fn test_idle_proc_ptr_non_null() {
        let v = CpuLocalVars::default();
        assert!(!v.idle_proc_ptr().is_null());
    }

    #[test]
    fn test_storage_init_and_accessors() {
        unsafe {
            let mut vars = CpuLocalVars::default();
            let storage = CpuLocalStorage::new();
            storage.init(&mut vars as *mut CpuLocalVars);

            assert!(storage.proc_ptr().is_null());
            assert!(storage.bill_ptr().is_null());
            assert_eq!(storage.pagefault_handled(), 0);
            assert!(storage.ptproc().is_null());
            assert_eq!(storage.cpu_is_idle(), 0);
            assert_eq!(storage.idle_interrupted(), 0);
            assert_eq!(storage.tsc_ctr_switch(), 0);
            assert_eq!(storage.cpu_last_tsc(), 0);
            assert_eq!(storage.cpu_last_idle(), 0);
            assert_eq!(storage.fpu_presence(), 0);
            assert!(storage.fpu_owner().is_null());
        }
    }

    #[test]
    fn test_storage_setters() {
        unsafe {
            let mut vars = CpuLocalVars::default();
            let storage = CpuLocalStorage::new();
            storage.init(&mut vars as *mut CpuLocalVars);

            let mock_proc = 0x42 as *mut core::ffi::c_void;
            let mock_proc2 = 0x84 as *mut core::ffi::c_void;

            storage.set_proc_ptr(mock_proc);
            assert_eq!(storage.proc_ptr(), mock_proc);

            storage.set_bill_ptr(mock_proc2);
            assert_eq!(storage.bill_ptr(), mock_proc2);

            storage.set_pagefault_handled(1);
            assert_eq!(storage.pagefault_handled(), 1);

            storage.set_ptproc(mock_proc);
            assert_eq!(storage.ptproc(), mock_proc);

            storage.set_cpu_is_idle(1);
            assert_eq!(storage.cpu_is_idle(), 1);

            storage.set_idle_interrupted(1);
            assert_eq!(storage.idle_interrupted(), 1);

            storage.set_tsc_ctr_switch(0xABCD);
            assert_eq!(storage.tsc_ctr_switch(), 0xABCD);

            storage.set_cpu_last_tsc(0x1234);
            assert_eq!(storage.cpu_last_tsc(), 0x1234);

            storage.set_cpu_last_idle(0x5678);
            assert_eq!(storage.cpu_last_idle(), 0x5678);

            storage.set_fpu_presence(1);
            assert_eq!(storage.fpu_presence(), 1);

            storage.set_fpu_owner(mock_proc2);
            assert_eq!(storage.fpu_owner(), mock_proc2);
        }
    }

    #[test]
    fn test_nr_sched_queues_value() {
        assert_eq!(NR_SCHED_QUEUES, 16);
    }

    #[test]
    fn test_run_q_mut_returns_matching_arrays() {
        unsafe {
            let mut vars = CpuLocalVars::default();
            let storage = CpuLocalStorage::new();
            storage.init(&mut vars as *mut CpuLocalVars);

            let head = storage.run_q_head_ptr();
            let tail = storage.run_q_tail_ptr();
            assert_eq!((*head).len(), NR_SCHED_QUEUES);
            assert_eq!((*tail).len(), NR_SCHED_QUEUES);

            // Write through raw pointer, read back through immut ref
            (*head)[0] = 0xDEAD as *mut core::ffi::c_void;
            (*tail)[3] = 0xBEEF as *mut core::ffi::c_void;
            assert_eq!(vars.run_q_head[0], 0xDEAD as *mut core::ffi::c_void);
            assert_eq!(vars.run_q_tail[3], 0xBEEF as *mut core::ffi::c_void);
        }
    }

    #[test]
    fn test_idle_proc_ptr_consistency() {
        unsafe {
            let mut vars = CpuLocalVars::default();
            assert_eq!(
                vars.idle_proc_ptr(),
                &vars.idle_proc as *const _ as *mut core::ffi::c_void
            );

            let storage = CpuLocalStorage::new();
            storage.init(&mut vars as *mut CpuLocalVars);
            assert_eq!(storage.idle_proc_ptr(), vars.idle_proc_ptr());
        }
    }

    #[test]
    fn test_global_init() {
        unsafe {
            init_cpulocals();
            // After init, accessors should work (default values)
            assert!(get_cpulocal_proc_ptr().is_null());
        }
    }

    #[test]
    fn test_idle_proc_size_reasonable() {
        // IDLE_PROC_SIZE must be large enough to hold a proc struct.
        // A reasonable x86_64 proc (regs, flags, scheduling fields) should
        // fit in at least 512 bytes; 1024 gives generous headroom.
        const _: () = assert!(IDLE_PROC_SIZE >= 512);
        assert_eq!(IDLE_PROC_SIZE, 1024);
    }

    #[test]
    fn test_run_q_all_null_after_init() {
        unsafe {
            let mut vars = CpuLocalVars::default();
            let storage = CpuLocalStorage::new();
            storage.init(&mut vars as *mut CpuLocalVars);

            for i in 0..NR_SCHED_QUEUES {
                // Read from the backing vars directly after init.
                assert!(
                    vars.run_q_head[i].is_null(),
                    "run_q_head[{}] should be null after default init",
                    i
                );
                assert!(
                    vars.run_q_tail[i].is_null(),
                    "run_q_tail[{}] should be null after default init",
                    i
                );
            }
        }
    }

    #[test]
    fn test_storage_double_init_idempotent() {
        unsafe {
            let mut vars1 = CpuLocalVars::default();
            let mut vars2 = CpuLocalVars::default();
            let storage = CpuLocalStorage::new();

            // First init with vars1.
            storage.init(&mut vars1 as *mut CpuLocalVars);
            assert_eq!(storage.proc_ptr(), vars1.proc_ptr);

            // Second init with vars2 — should be ignored.
            storage.init(&mut vars2 as *mut CpuLocalVars);
            // Storage should still point to vars1.
            assert_eq!(storage.proc_ptr(), vars1.proc_ptr);
        }
    }

    #[test]
    fn test_storage_new_uninitialized() {
        let storage = CpuLocalStorage::new();
        // A newly created storage should report uninitialized.
        assert!(!storage.initialized.load(Ordering::Relaxed));
    }

    #[test]
    fn test_idle_proc_ptr_within_struct() {
        let v = CpuLocalVars::default();
        let idle = v.idle_proc_ptr();
        let base = &v as *const CpuLocalVars as usize;
        let idle_addr = idle as usize;
        // The idle_proc buffer must be inside the CpuLocalVars struct.
        assert!(idle_addr >= base, "idle_proc_ptr must be >= struct base");
        assert!(
            idle_addr + IDLE_PROC_SIZE <= base + size_of::<CpuLocalVars>(),
            "idle_proc buffer must fit within the struct"
        );
    }

    #[test]
    fn test_fpu_fields_set_get() {
        unsafe {
            let mut vars = CpuLocalVars::default();
            let storage = CpuLocalStorage::new();
            storage.init(&mut vars as *mut CpuLocalVars);

            // fpu_presence roundtrip.
            assert_eq!(storage.fpu_presence(), 0);
            storage.set_fpu_presence(1);
            assert_eq!(storage.fpu_presence(), 1);
            storage.set_fpu_presence(0);
            assert_eq!(storage.fpu_presence(), 0);

            // fpu_owner roundtrip.
            assert!(storage.fpu_owner().is_null());
            let mock = 0xCAFE as *mut core::ffi::c_void;
            storage.set_fpu_owner(mock);
            assert_eq!(storage.fpu_owner(), mock);
        }
    }

    #[test]
    fn test_set_proc_ptr_affects_get_cpulocal() {
        unsafe {
            init_cpulocals();
            let mock = 0xDEAD as *mut core::ffi::c_void;
            set_cpulocal_proc_ptr(mock);
            assert_eq!(get_cpulocal_proc_ptr(), mock);

            // Reset for other tests.
            set_cpulocal_proc_ptr(core::ptr::null_mut());
        }
    }
}
