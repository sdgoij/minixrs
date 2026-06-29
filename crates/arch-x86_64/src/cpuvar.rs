//! Per-CPU information — adapted from `cpuvar.h`
//!
//! **x86_64 differences from i386:**
//! - CPU info struct uses 64-bit fields for address storage
//! - Per-CPU data accessed via `swapgs` + GS segment
//! - Larger kernel stack sizes (16 KB vs 8 KB)

use core::cell::UnsafeCell;

/// Maximum number of CPUs.
const MAXCPUS: u32 = 32;

/// Per-CPU information structure.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct CpuInfo {
    /// CPU ID (0-indexed).
    pub ci_cpunumber: u32,
    /// Whether this CPU is the BSP.
    pub ci_is_bsp: u32,
    /// CPU role (boot, app, etc.).
    pub ci_role: u32,
    /// Padding.
    pub _pad: u32,
    /// Kernel stack pointer for this CPU.
    pub ci_kstack: u64,
    /// Current process pointer.
    pub ci_curproc: u64,
    /// Idle process pointer.
    pub ci_idleproc: u64,
    /// CPU frequency in Hz.
    pub ci_freq_hz: u64,
    /// TSC frequency in Hz.
    pub ci_tsc_freq: u64,
    /// Whether TSC is invariant.
    pub ci_tsc_invariant: u32,
    /// CPU family/model/stepping.
    pub ci_family: u8,
    pub ci_model: u8,
    pub ci_stepping: u8,
    pub _pad2: u8,
    /// Reserved for future use.
    pub _reserved: [u64; 8],
}

// ── CPU roles ───────────────────────────────────────────────────────────

/// CPU role: service processor (BSP bootstrap).
pub const CPU_ROLE_SP: u32 = 0;
/// CPU role: boot processor (primary).
pub const CPU_ROLE_BP: u32 = 1;
/// CPU role: application processor (secondary).
pub const CPU_ROLE_AP: u32 = 2;

/// Wrapper for `[CpuInfo; MAXCPUS as usize]`.
pub struct CpuInfoCell(UnsafeCell<[CpuInfo; MAXCPUS as usize]>);
unsafe impl Sync for CpuInfoCell {}
impl CpuInfoCell {
    pub const fn new(val: [CpuInfo; MAXCPUS as usize]) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut [CpuInfo; MAXCPUS as usize] {
        self.0.get()
    }
}

// ── Global CPU info array ───────────────────────────────────────────────

pub static CPU_INFO: CpuInfoCell = CpuInfoCell::new(
    [CpuInfo {
        ci_cpunumber: 0,
        ci_is_bsp: 0,
        ci_role: 0,
        _pad: 0,
        ci_kstack: 0,
        ci_curproc: 0,
        ci_idleproc: 0,
        ci_freq_hz: 0,
        ci_tsc_freq: 0,
        ci_tsc_invariant: 0,
        ci_family: 0,
        ci_model: 0,
        ci_stepping: 0,
        _pad2: 0,
        _reserved: [0u64; 8],
    }; MAXCPUS as usize],
);

// ── Helper functions ────────────────────────────────────────────────────

/// Get CPU info for a given CPU number.
pub fn cpu_info(cpu: u32) -> &'static CpuInfo {
    unsafe { &(*CPU_INFO.get())[cpu as usize] }
}

/// Get mutable CPU info for a given CPU number.
pub fn cpu_info_mut(cpu: u32) -> &'static mut CpuInfo {
    unsafe { &mut (*CPU_INFO.get())[cpu as usize] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_info_size() {
        assert!(size_of::<CpuInfo>() >= 64);
    }

    #[test]
    fn test_cpu_roles() {
        assert_eq!(CPU_ROLE_SP, 0);
        assert_eq!(CPU_ROLE_BP, 1);
        assert_eq!(CPU_ROLE_AP, 2);
    }

    #[test]
    fn test_cpu_info_default() {
        let ci = CpuInfo::default();
        assert_eq!(ci.ci_cpunumber, 0);
        assert_eq!(ci.ci_freq_hz, 0);
    }
}
