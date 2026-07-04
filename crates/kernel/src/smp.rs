//! SMP (Symmetric Multi-Processing) support — adapted from
//! `minix/kernel/smp.c` and `minix/kernel/smp.h`
//!
//! Manages per-CPU state, inter-processor interrupts (IPIs), and the
//! Big Kernel Lock (BKL) synchronization protocol.
//!
//! **Current status:** single-CPU only (CONFIG_SMP = false). All BKL
//! operations are no-ops. Multi-CPU bring-up is planned for a later phase.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::hal::hlt;
use crate::hal::{bkl_lock, bkl_unlock};

use crate::proc::{Proc, RtsFlags};

// ─────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────

/// Maximum number of CPUs the SMP layer supports.
pub const CONFIG_MAX_CPUS: usize = 32;

/// CPU is fully initialized and ready.
const CPU_IS_READY: u32 = 1;

/// CPU is currently booting.
#[allow(dead_code)]
const CPU_IS_BOOTING: u32 = 2;

// Sched IPI task flags

/// Stop the target process.
const SCHED_IPI_STOP_PROC: u32 = 1;

/// Inhibit the target process from VM operations.
const SCHED_IPI_VM_INHIBIT: u32 = 2;

/// Save the context of the target process.
#[allow(dead_code)]
const SCHED_IPI_SAVE_CTX: u32 = 4;

// ─────────────────────────────────────────────────────────────────────────
// Per-CPU information
// ─────────────────────────────────────────────────────────────────────────

/// Per-CPU state information.
#[derive(Debug)]
#[repr(C)]
pub struct CpuInfo {
    /// CPU state flags (CPU_IS_READY, CPU_IS_BOOTING).
    pub flags: AtomicU32,
    /// CPU frequency in MHz.
    pub freq: AtomicU32,
}

#[allow(clippy::declare_interior_mutable_const)]
const CPU_INFO_INIT: CpuInfo = CpuInfo {
    flags: AtomicU32::new(0),
    freq: AtomicU32::new(0),
};

/// Per-CPU information array, indexed by logical CPU ID.
pub static CPUS: [CpuInfo; CONFIG_MAX_CPUS] = [CPU_INFO_INIT; CONFIG_MAX_CPUS];

/// Number of CPUs in the system.
pub static NCPUS: AtomicU32 = AtomicU32::new(1);

/// BSP (bootstrap processor) CPU ID.
pub static BSP_CPU_ID: AtomicU32 = AtomicU32::new(0);

/// Number of hyperthreads per core.
pub static HT_PER_CORE: AtomicU32 = AtomicU32::new(1);

// ─────────────────────────────────────────────────────────────────────────
// Sched IPI data (per-target-CPU, synchronised via BKL)
// ─────────────────────────────────────────────────────────────────────────

/// Per-CPU sched-IPI payload.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SchedIpiData {
    /// Flags: SCHED_IPI_STOP_PROC | SCHED_IPI_VM_INHIBIT | SCHED_IPI_SAVE_CTX.
    flags: u32,
    /// Opaque payload (cast to `*mut Proc`).
    data: u32,
}

const SCHED_IPI_DATA_INIT: SchedIpiData = SchedIpiData { flags: 0, data: 0 };

/// Pending sched-IPI requests, one slot per target CPU.
///
/// # Safety
///
/// Accessed only while holding the BKL.
static mut SCHED_IPI_DATA: [SchedIpiData; CONFIG_MAX_CPUS] = [SCHED_IPI_DATA_INIT; CONFIG_MAX_CPUS];

/// Number of APs that have finished booting.
///
/// # Safety
///
/// Accessed only while holding the BKL (or during early boot before SMP
/// is active).
static mut AP_CPUS_BOOTED: u32 = 0;

// ─────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────

/// Get the current CPU's logical ID.
///
/// In single-CPU mode (default) always returns 0. When SMP is enabled,
/// this reads from per-CPU local storage.
///
/// # Safety
///
/// In single-CPU mode this is trivially safe. In SMP mode the per-CPU
/// storage must be initialized.
#[inline]
pub unsafe fn cpu_id() -> u32 {
    crate::hal::cpu_id()
}

// ─────────────────────────────────────────────────────────────────────────
// AP boot tracking
// ─────────────────────────────────────────────────────────────────────────

/// Wait for all Application Processors to finish booting.
///
/// # Safety
///
/// Must be called on the BSP while holding the BKL.
pub unsafe fn wait_for_aps_to_finish_booting() {
    let n = NCPUS.load(Ordering::Relaxed);
    // Count CPUs that are ready
    let mut ready = 0u32;
    for cpu in CPUS.iter().take(n as usize) {
        if cpu.flags.load(Ordering::Relaxed) & CPU_IS_READY != 0 {
            ready += 1;
        }
    }
    if ready != n {
        // TODO: print warning / log mismatch
    }

    // Drop the BKL so APs can acquire it during their boot sequence
    unsafe { bkl_unlock() };

    // Wait for all APs to finish
    unsafe {
        while AP_CPUS_BOOTED < n - 1 {
            hlt();
        }
    }

    unsafe { bkl_lock() };
}

/// Mark an Application Processor as finished booting.
///
/// # Safety
///
/// Must be called on the AP that just finished booting.
pub unsafe fn ap_boot_finished(_cpu: u32) {
    unsafe {
        AP_CPUS_BOOTED = AP_CPUS_BOOTED.wrapping_add(1);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// IPI handlers
// ─────────────────────────────────────────────────────────────────────────

/// Halt handler — stop the current CPU.
///
/// Invoked when another CPU sends a halt IPI (e.g., during shutdown).
pub fn smp_ipi_halt_handler() {
    // TODO: ipi_ack(), stop_local_timer(), arch_smp_halt_cpu()
    // These are arch-specific and will be implemented when the APIC
    // interrupt subsystem is ready.
    todo!("smp_ipi_halt_handler: arch-specific halt IPI sequence; see porting tracker");
}

/// Send a schedule IPI to another CPU.
///
/// # Safety
///
/// `cpu` must be a valid CPU ID less than `NCPUS`.
pub unsafe fn smp_schedule(cpu: u32) {
    // TODO: Call `arch_send_smp_schedule_ipi(cpu)` when the APIC IPI
    //       primitives are available.
    let _ = cpu;
    todo!("smp_schedule: arch_send_smp_schedule_ipi not yet implemented; see porting tracker");
}

/// Handle a schedule IPI's payload on the receiving CPU.
///
/// Reads the pending flags and data for the current CPU and applies
/// them to the target process.
///
/// # Safety
///
/// Must be called with interrupts disabled, holding the BKL.
pub unsafe fn smp_sched_handler() {
    let cpu = unsafe { cpu_id() as usize };
    let flgs = unsafe { SCHED_IPI_DATA[cpu].flags };
    if flgs != 0 {
        let p = unsafe { SCHED_IPI_DATA[cpu].data } as *mut Proc;
        if flgs & SCHED_IPI_STOP_PROC != 0 {
            unsafe {
                (*p).p_rts_flags
                    .fetch_or(RtsFlags::PROC_STOP.bits(), Ordering::Relaxed);
            }
        }
        if flgs & SCHED_IPI_VM_INHIBIT != 0 {
            unsafe {
                (*p).p_rts_flags
                    .fetch_or(RtsFlags::VMINHIBIT.bits(), Ordering::Relaxed);
            }
        }
        unsafe {
            SCHED_IPI_DATA[cpu].flags = 0;
        }
    }
}

/// Acknowledge a schedule IPI and mark the current process as preempted.
///
/// # Safety
///
/// Must be called on the receiving CPU with interrupts disabled.
pub unsafe fn smp_ipi_sched_handler() {
    // TODO: ipi_ack() — acknowledge the IPI at the APIC level.

    let curr = crate::hal::smp_proc_ptr() as *mut Proc;
    if !curr.is_null() && unsafe { (*curr).p_endpoint != arch_common::com::IDLE } {
        unsafe {
            (*curr)
                .p_rts_flags
                .fetch_or(RtsFlags::PREEMPTED.bits(), Ordering::Relaxed);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Sched IPI helpers (send side)
// ─────────────────────────────────────────────────────────────────────────

/// Schedule a stop-process IPI for `p`.
///
/// If the process is runnable, sends an IPI to its CPU. Otherwise sets
/// PROC_STOP directly.
///
/// # Safety
///
/// `p` must point to a valid `Proc` in the process table.
pub unsafe fn smp_schedule_stop_proc(p: *mut Proc) {
    if unsafe { (*p).is_runnable() } {
        unsafe { smp_schedule_sync(p, SCHED_IPI_STOP_PROC) };
    } else {
        unsafe {
            (*p).p_rts_flags
                .fetch_or(RtsFlags::PROC_STOP.bits(), Ordering::Relaxed);
        }
    }
}

/// Schedule a VM-inhibit IPI for `p`.
///
/// If the process is runnable, sends an IPI to its CPU. Otherwise sets
/// VMINHIBIT directly.
///
/// # Safety
///
/// `p` must point to a valid `Proc` in the process table.
pub unsafe fn smp_schedule_vminhibit(p: *mut Proc) {
    if unsafe { (*p).is_runnable() } {
        unsafe { smp_schedule_sync(p, SCHED_IPI_VM_INHIBIT) };
    } else {
        unsafe {
            (*p).p_rts_flags
                .fetch_or(RtsFlags::VMINHIBIT.bits(), Ordering::Relaxed);
        }
    }
}

/// Internal: send a synchronous sched IPI to the CPU that `p` is on,
/// and wait for the remote CPU to process it.
///
/// # Safety
///
/// `p` must point to a valid `Proc`. Must be called while holding the
/// BKL. The target CPU must be different from the current CPU.
unsafe fn smp_schedule_sync(p: *mut Proc, task: u32) {
    let cpu = unsafe { (*p).p_cpu as usize };
    let mycpu = unsafe { cpu_id() as usize };

    // Must not target ourselves
    assert!(cpu != mycpu, "smp_schedule_sync: target CPU == current CPU");

    // ── Phase 1: wait for any previous IPI to this target ──
    if unsafe { SCHED_IPI_DATA[cpu].flags != 0 } {
        unsafe { bkl_unlock() };
        while unsafe { SCHED_IPI_DATA[cpu].flags != 0 } {
            // Service our own IPI queue while we wait
            if unsafe { SCHED_IPI_DATA[mycpu].flags != 0 } {
                unsafe { bkl_lock() };
                unsafe { smp_sched_handler() };
                unsafe { bkl_unlock() };
            }
            core::hint::spin_loop();
        }
        unsafe { bkl_lock() };
    }

    // ── Phase 2: post the IPI ──
    unsafe {
        SCHED_IPI_DATA[cpu].data = p as u32;
        SCHED_IPI_DATA[cpu].flags |= task;
    }

    // TODO: arch_send_smp_schedule_ipi(cpu as u32)
    //       — send the actual IPI via APIC.

    // ── Phase 3: wait for the remote CPU to clear the flags ──
    unsafe { bkl_unlock() };
    while unsafe { SCHED_IPI_DATA[cpu].flags != 0 } {
        // Service our own IPI queue while we wait
        if unsafe { SCHED_IPI_DATA[mycpu].flags != 0 } {
            unsafe { bkl_lock() };
            unsafe { smp_sched_handler() };
            unsafe { bkl_unlock() };
        }
        core::hint::spin_loop();
    }
    unsafe { bkl_lock() };
}

// ─────────────────────────────────────────────────────────────────────────
// CPU frequency tracking
// ─────────────────────────────────────────────────────────────────────────

/// Set the frequency for a given CPU.
pub fn cpu_set_freq(cpu: u32, freq: u32) {
    if (cpu as usize) < CONFIG_MAX_CPUS {
        CPUS[cpu as usize].freq.store(freq, Ordering::Relaxed);
    }
}

/// Get the frequency of a given CPU.
pub fn cpu_get_freq(cpu: u32) -> u32 {
    if (cpu as usize) < CONFIG_MAX_CPUS {
        CPUS[cpu as usize].freq.load(Ordering::Relaxed)
    } else {
        0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ncpus_defaults_to_one() {
        assert_eq!(NCPUS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_bsp_cpu_id_defaults_to_zero() {
        assert_eq!(BSP_CPU_ID.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_ht_per_core_default() {
        assert_eq!(HT_PER_CORE.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_bkl_lock_unlock_does_not_crash() {
        unsafe {
            bkl_lock();
            bkl_unlock();
        }
    }

    #[test]
    fn test_cpu_set_get_freq_roundtrip() {
        cpu_set_freq(0, 2400);
        assert_eq!(cpu_get_freq(0), 2400);

        cpu_set_freq(1, 3200);
        assert_eq!(cpu_get_freq(1), 3200);
    }

    #[test]
    fn test_cpu_get_freq_out_of_range_returns_zero() {
        assert_eq!(cpu_get_freq(CONFIG_MAX_CPUS as u32), 0);
    }

    #[test]
    fn test_cpu_set_freq_out_of_range_is_noop() {
        cpu_set_freq(CONFIG_MAX_CPUS as u32, 9999);
        // Should not panic; result is irrelevant
    }

    #[test]
    fn test_smp_sched_handler_zero_flags_is_noop() {
        unsafe {
            // With no pending flags, smp_sched_handler should do nothing
            // and not crash.
            smp_sched_handler();
        }
    }

    #[test]
    fn test_ap_boot_finished_increments_counter() {
        unsafe {
            let before = core::ptr::addr_of_mut!(AP_CPUS_BOOTED).read();
            ap_boot_finished(1);
            let after = core::ptr::addr_of_mut!(AP_CPUS_BOOTED).read();
            assert_eq!(after, before.wrapping_add(1));
            // Reset for other tests
            core::ptr::addr_of_mut!(AP_CPUS_BOOTED).write(before);
        }
    }

    #[test]
    fn test_smp_ipi_sched_handler_null_proc_ptr() {
        unsafe {
            // When proc_ptr is null, smp_ipi_sched_handler must not crash.
            // We save and restore the proc_ptr to be safe.
            let saved = crate::hal::smp_proc_ptr();
            crate::hal::smp_set_proc_ptr(core::ptr::null_mut());
            smp_ipi_sched_handler();
            crate::hal::smp_set_proc_ptr(saved);
        }
    }

    #[test]
    fn test_cpu_id_returns_zero() {
        unsafe {
            // On RISC-V, cpu_id reads mhartid. BSP hartid is typically 0
            // but can differ on some platforms, so assert it's a valid ID.
            let id = cpu_id();
            assert!(
                (id as usize) < CONFIG_MAX_CPUS,
                "cpu_id {} out of range",
                id
            );
        }
    }

    #[test]
    fn test_cpu_info_init() {
        // Verify CpuInfo initial state.
        for cpu in CPUS.iter().take(CONFIG_MAX_CPUS) {
            assert_eq!(cpu.flags.load(Ordering::Relaxed), 0);
            assert_eq!(cpu.freq.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn test_cpu_info_flags_operations() {
        let flags_val = CPU_IS_READY | CPU_IS_BOOTING;
        CPUS[0].flags.store(flags_val, Ordering::Relaxed);
        assert_eq!(CPUS[0].flags.load(Ordering::Relaxed), flags_val);
        CPUS[0].flags.store(0, Ordering::Relaxed);
        assert_eq!(CPUS[0].flags.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_config_max_cpus() {
        const {
            assert!(CONFIG_MAX_CPUS >= 1);
        }
        const {
            assert!(CONFIG_MAX_CPUS <= 32);
        }
    }
}
