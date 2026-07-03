//! RISC-V64 per-CPU data — stored via the `tp` (x4) register.
//!
//! On RISC-V, `tp` (x4 / thread pointer) serves the same role as
//! x86_64's GS segment: it points to a per-CPU structure holding
//! kernel context for the currently running hart.
//!
//! Unlike x86_64's `swapgs` mechanism, we can read/write tp
//! directly with `mv` instructions.

#![cfg(target_arch = "riscv64")]

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Per-CPU storage structure.
///
/// Each hart has its own copy of this structure. The `tp` register
/// points to the current hart's instance.
#[repr(C)]
pub struct PerCpuStorage {
    /// Current process pointer (opaque, kernel Proc).
    pub current_proc: u64,
    /// Kernel stack top for this hart.
    pub kernel_stack_top: u64,
    /// Hart ID (0 for BSP, 1+ for APs).
    pub hart_id: u32,
    /// Padding to 64-byte cache line.
    _pad: [u8; 44],
}

/// Static storage for the boot hart (hart 0).
/// APs will allocate their own during SMP init.
pub static mut BOOT_CPU_STORAGE: PerCpuStorage = PerCpuStorage {
    current_proc: 0,
    kernel_stack_top: 0,
    hart_id: 0,
    _pad: [0u8; 44],
};

/// Initialize per-CPU storage for the boot hart.
///
/// Sets `tp` to point to `BOOT_CPU_STORAGE`.
///
/// # Safety
///
/// Must be called once during early boot on the BSP.
pub unsafe fn init_cpulocals() {
    let ptr = core::ptr::addr_of_mut!(BOOT_CPU_STORAGE) as u64;
    set_tp_pointer(ptr);
}

/// Set the tp register to point to a PerCpuStorage instance.
fn set_tp_pointer(addr: u64) {
    unsafe {
        core::arch::asm!("mv tp, {addr}", addr = in(reg) addr, options(nomem, nostack));
    }
}

/// Get the current hart's PerCpuStorage pointer from tp.
fn tp_ptr() -> *mut PerCpuStorage {
    let ptr: u64;
    unsafe {
        core::arch::asm!("mv {ptr}, tp", ptr = out(reg) ptr, options(nomem, nostack));
    }
    ptr as *mut PerCpuStorage
}

/// Set the current process pointer for this hart.
///
/// # Safety
///
/// `proc` must point to a valid `Proc` or be null.
pub unsafe fn set_current_proc(proc: u64) {
    let storage = tp_ptr();
    unsafe {
        (*storage).current_proc = proc;
    }
}

/// Get the current process pointer for this hart.
pub fn current_proc() -> u64 {
    let storage = tp_ptr();
    unsafe { (*storage).current_proc }
}

/// Set the kernel stack top for this hart.
///
/// # Safety
///
/// Must be called during stack setup for each hart.
pub unsafe fn set_kernel_stack_top(sp: u64) {
    let storage = tp_ptr();
    unsafe {
        (*storage).kernel_stack_top = sp;
    }
}

/// Get the kernel stack top for this hart.
pub fn kernel_stack_top() -> u64 {
    let storage = tp_ptr();
    unsafe { (*storage).kernel_stack_top }
}

/// Get the hart ID.
pub fn hart_id() -> u32 {
    let storage = tp_ptr();
    unsafe { (*storage).hart_id }
}

/// Set the hart ID.
///
/// # Safety
///
/// Must be called during hart initialization.
pub unsafe fn set_hart_id(id: u32) {
    let storage = tp_ptr();
    unsafe {
        (*storage).hart_id = id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_size() {
        assert_eq!(core::mem::size_of::<PerCpuStorage>(), 64);
    }

    #[test]
    fn test_boot_storage_initialized() {
        unsafe {
            assert_eq!(BOOT_CPU_STORAGE.current_proc, 0);
            assert_eq!(BOOT_CPU_STORAGE.hart_id, 0);
        }
    }
}
