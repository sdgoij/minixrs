//! Per-CPU local storage for AArch64.
//!
//! On AArch64 uniprocessor, we use a static UnsafeCell.
//! SMP would use TPIDR_EL1 for per-core access.

#![cfg(target_arch = "aarch64")]

use core::cell::UnsafeCell;

pub const NR_SCHED_QUEUES: usize = 16;

/// Per-CPU storage structure.
#[repr(C)]
pub struct PerCpuStorage {
    /// Current process pointer.
    pub current_proc: u64,
    /// Kernel stack top.
    pub kernel_stack_top: u64,
    /// CPU ID.
    pub cpu_id: u32,
    _pad: u32,
    /// TSC/cycle counter at context switch.
    pub tsc_ctr_switch: u64,
    /// Billable process pointer.
    pub bill_proc: u64,
    /// Ready list head pointers.
    pub run_q_head: [*mut core::ffi::c_void; NR_SCHED_QUEUES],
    /// Ready list tail pointers.
    pub run_q_tail: [*mut core::ffi::c_void; NR_SCHED_QUEUES],
}

pub fn run_q_head_ptr() -> *mut [*mut core::ffi::c_void; NR_SCHED_QUEUES] {
    let storage = BOOT_CPU_STORAGE.get();
    unsafe { core::ptr::addr_of_mut!((*storage).run_q_head) }
}

pub fn run_q_tail_ptr() -> *mut [*mut core::ffi::c_void; NR_SCHED_QUEUES] {
    let storage = BOOT_CPU_STORAGE.get();
    unsafe { core::ptr::addr_of_mut!((*storage).run_q_tail) }
}

pub struct BootCpuStorageCell(UnsafeCell<PerCpuStorage>);
unsafe impl Sync for BootCpuStorageCell {}
impl BootCpuStorageCell {
    const fn new(val: PerCpuStorage) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut PerCpuStorage {
        self.0.get()
    }
}

pub static BOOT_CPU_STORAGE: BootCpuStorageCell = BootCpuStorageCell::new(PerCpuStorage {
    current_proc: 0,
    kernel_stack_top: 0,
    cpu_id: 0,
    _pad: 0,
    tsc_ctr_switch: 0,
    bill_proc: 0,
    run_q_head: [core::ptr::null_mut(); NR_SCHED_QUEUES],
    run_q_tail: [core::ptr::null_mut(); NR_SCHED_QUEUES],
});

/// Initialize per-CPU storage (no-op on uniprocessor).
pub unsafe fn init_cpulocals() {}

/// Set the current process pointer.
///
/// # Safety
///
/// `proc` must point to a valid Proc or be 0.
pub unsafe fn set_current_proc(proc: u64) {
    unsafe {
        core::ptr::write_volatile(&raw mut (*BOOT_CPU_STORAGE.get()).current_proc, proc);
    }
}

/// Get the current process pointer.
pub fn current_proc() -> u64 {
    unsafe { core::ptr::read_volatile(&raw const (*BOOT_CPU_STORAGE.get()).current_proc) }
}

/// Get the billable process pointer.
pub fn bill_proc() -> u64 {
    unsafe { core::ptr::read_volatile(&raw const (*BOOT_CPU_STORAGE.get()).bill_proc) }
}

/// Set the billable process pointer.
///
/// # Safety
///
/// `proc` must point to a valid Proc or be 0.
pub unsafe fn set_bill_proc(proc: u64) {
    unsafe {
        core::ptr::write_volatile(&raw mut (*BOOT_CPU_STORAGE.get()).bill_proc, proc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_initialized() {
        unsafe {
            assert_eq!((*BOOT_CPU_STORAGE.get()).current_proc, 0);
            assert_eq!((*BOOT_CPU_STORAGE.get()).cpu_id, 0);
        }
    }
}
