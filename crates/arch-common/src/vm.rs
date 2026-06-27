//! VM interface types and prototypes from `minix/vm.h`

use crate::types::{Endpoint, PhysBytes, VirBytes};

// ─── VM statistics ──────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VmStatsInfo {
    pub vsi_pagesize: u32,
    pub vsi_total: u64,
    pub vsi_free: u64,
    pub vsi_largest: u64,
    pub vsi_cached: u64,
}

// ─── VM usage ───────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VmUsageInfo {
    pub vui_total: VirBytes,
    pub vui_common: VirBytes,
    pub vui_shared: VirBytes,
}

// ─── VM region ──────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VmRegionInfo {
    pub vri_addr: VirBytes,
    pub vri_length: VirBytes,
    pub vri_prot: i32,
    pub vri_flags: i32,
}

/// Maximum number of regions provided at once.
pub const MAX_VRI_COUNT: usize = 64;

// ─── VM cache flags ─────────────────────────────────────────────────────

pub const VMMC_FLAGS_LOCKED: u32 = 0x01;
pub const VMMC_DIRTY: u32 = 0x02;
pub const VMMC_EVICTED: u32 = 0x04;
pub const VMMC_BLOCK_LOCKED: u32 = 0x08;

/// Special inode number for VM cache functions (disk block, no file).
pub const VMC_NO_INODE: u64 = 0;

// ─── VM mmap flags ──────────────────────────────────────────────────────

pub const MVM_WRITABLE: u16 = 0x8000;

// ─── VM request types ───────────────────────────────────────────────────

pub const VMPTYPE_NONE: u32 = 0;
pub const VMPTYPE_CHECK: u32 = 1;

// ─── Function prototypes (extern "C" stubs) ─────────────────────────────

pub type VmExitFn = unsafe extern "C" fn(Endpoint) -> i32;
pub type VmForkFn = unsafe extern "C" fn(Endpoint, i32, *mut Endpoint) -> i32;
pub type VmWillexitFn = unsafe extern "C" fn(Endpoint) -> i32;
pub type VmAddDmaFn = unsafe extern "C" fn(Endpoint, PhysBytes, PhysBytes) -> i32;
pub type VmDelDmaFn = unsafe extern "C" fn(Endpoint, PhysBytes, PhysBytes) -> i32;
pub type VmGetDmaFn = unsafe extern "C" fn(*mut Endpoint, *mut PhysBytes, *mut PhysBytes) -> i32;
pub type VmMapPhysFn = unsafe extern "C" fn(Endpoint, *mut u8, usize) -> *mut u8;
pub type VmUnmapPhysFn = unsafe extern "C" fn(Endpoint, *mut u8, usize) -> i32;
pub type VmNotifySigFn = unsafe extern "C" fn(Endpoint, Endpoint) -> i32;
pub type VmSetPrivFn = unsafe extern "C" fn(Endpoint, *mut u8, i32) -> i32;
pub type VmUpdateFn = unsafe extern "C" fn(Endpoint, Endpoint) -> i32;
pub type VmMemCtlFn = unsafe extern "C" fn(Endpoint, i32) -> i32;
pub type VmQueryExitFn = unsafe extern "C" fn(*mut Endpoint) -> i32;
pub type VmWatchExitFn = unsafe extern "C" fn(Endpoint) -> i32;
pub type VmForgetBlockFn = unsafe extern "C" fn(u64) -> i32;
pub type VmForgetBlocksFn = unsafe extern "C" fn();
pub type VmInfoStatsFn = unsafe extern "C" fn(*mut VmStatsInfo) -> i32;
pub type VmInfoUsageFn = unsafe extern "C" fn(Endpoint, *mut VmUsageInfo) -> i32;
pub type VmInfoRegionFn = unsafe extern "C" fn(Endpoint, *mut VmRegionInfo, i32, *mut VirBytes) -> i32;
pub type VmProcCtlClearFn = unsafe extern "C" fn(Endpoint) -> i32;
pub type VmProcCtlHandlememFn = unsafe extern "C" fn(Endpoint, VirBytes, VirBytes, i32) -> i32;
pub type VmSetCacheBlockFn = unsafe extern "C" fn(*mut u8, u64, u64, u64, u64, *mut u32, i32) -> i32;
pub type VmMapCacheBlockFn = unsafe extern "C" fn(u64, u64, u64, u64, *mut u32, i32) -> *mut u8;
pub type VmClearCacheFn = unsafe extern "C" fn(u64) -> i32;

// ─── Minix VFS mmap ─────────────────────────────────────────────────────

// minix_vfs_mmap uses dev_t, ino_t, u16 flags — defined here for completeness.

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_vm_constants() {
        assert_eq!(MVM_WRITABLE, 0x8000);
        assert_eq!(VMPTYPE_NONE, 0);
        assert_eq!(VMPTYPE_CHECK, 1);
        assert_eq!(MAX_VRI_COUNT, 64);
    }

    #[test]
    fn test_vm_cache_flags() {
        assert_eq!(VMMC_FLAGS_LOCKED, 0x01);
        assert_eq!(VMMC_DIRTY, 0x02);
        assert_eq!(VMMC_EVICTED, 0x04);
        assert_eq!(VMMC_BLOCK_LOCKED, 0x08);
    }

    #[test]
    fn test_vm_struct_sizes() {
        assert!(size_of::<VmStatsInfo>() >= 32);
        assert!(size_of::<VmUsageInfo>() >= 24);
        assert!(size_of::<VmRegionInfo>() >= 24);
    }

    #[test]
    fn test_vmc_no_inode() {
        assert_eq!(VMC_NO_INODE, 0);
    }
}
