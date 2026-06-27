//! VM parameter constants — adapted from i386 `vmparam.h`
//!
//! **x86_64 differences from i386:**
//! - Virtual address space: 48-bit (256 TB) instead of 32-bit (4 GB)
//! - User/kernel split: canonical upper half (0xFFFF8000...) vs 0xC0000000
//! - USRSTACK at top of user space (0x00007FFFFFFFFFFF) vs 0xBFBFE000
//! - No execute permission via NX bit (not segment-based like i386)
//! - Larger process size limits (no 4 GB ceiling)
//! - Paging level constants for 4-level paging

// ── Paging level shifts and page directory sizes ───────────────────────

/// L4: PML4 shift (bits 39-47)
pub const L4_SHIFT: u32 = 39;
/// L3: PDPT shift (bits 30-38)
pub const L3_SHIFT: u32 = 30;
/// L2: PD shift (bits 21-29)
pub const L2_SHIFT: u32 = 21;
/// L1: PT shift (bits 12-20)
pub const L1_SHIFT: u32 = 12;

/// Bytes mapped by one L4 entry (512 GB)
pub const NBPD_L4: u64 = 1u64 << L4_SHIFT;
/// Bytes mapped by one L3 entry (1 GB)
pub const NBPD_L3: u64 = 1u64 << L3_SHIFT;
/// Bytes mapped by one L2 entry (2 MB)
pub const NBPD_L2: u64 = 1u64 << L2_SHIFT;
/// Bytes mapped by one L1 entry (4 KB)
pub const NBPD_L1: u64 = 1u64 << L1_SHIFT;

/// Number of entries per page table (all levels).
pub const NENTRIES: u64 = 512;

// ── Page size ───────────────────────────────────────────────────────────

pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
pub const PAGE_MASK: u64 = PAGE_SIZE - 1;

// ── Virtual address space layout ───────────────────────────────────────

/// Top of user stack (end of user address space).
pub const USRSTACK: u64 = 0x00007FFFFFFFFFFFu64;

pub const VM_MIN_ADDRESS: u64 = 0;
pub const VM_MAXUSER_ADDRESS: u64 = 0x00007FFFFFFFFFFFu64;
pub const VM_MAX_ADDRESS: u64 = 0xFFFFFFFFFFFFFFFFu64;

pub const VM_MIN_KERNEL_ADDRESS: u64 = 0xFFFF800000000000u64;
pub const VM_MAX_KERNEL_ADDRESS: u64 = 0xFFFFFFFFFFFFFFFFu64;

// ── Process size limits ────────────────────────────────────────────────

pub const MAXTSIZ: u64 = 256 * 1024 * 1024;
pub const DFLDSIZ: u64 = 512 * 1024 * 1024;
pub const MAXDSIZ: u64 = 128 * 1024 * 1024 * 1024;
pub const DFLSSIZ: u64 = 8 * 1024 * 1024;
pub const MAXSSIZ: u64 = 64 * 1024 * 1024;

// ── Physical memory ─────────────────────────────────────────────────────

pub const USRIOSIZE: u32 = 300;
pub const VM_PHYS_SIZE: u64 = USRIOSIZE as u64 * PAGE_SIZE;
pub const VM_MAX_KERNEL_BUF: u64 = 384 * 1024 * 1024;

pub const VM_PHYSSEG_MAX: u32 = 32;
pub const VM_NFREELIST: u32 = 2;
pub const VM_FREELIST_DEFAULT: u32 = 0;
pub const VM_FREELIST_FIRST16: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paging_constants() {
        assert_eq!(PAGE_SIZE, 4096);
        assert_eq!(NBPD_L1, 4096);
        assert_eq!(NBPD_L2, 2 * 1024 * 1024);
        assert_eq!(NBPD_L3, 1024 * 1024 * 1024);
        assert_eq!(NBPD_L4, 512 * 1024 * 1024 * 1024);
        assert_eq!(NENTRIES, 512);
    }

    #[test]
    fn test_address_ranges() {
        assert_eq!(VM_MIN_ADDRESS, 0);
        assert_eq!(VM_MAXUSER_ADDRESS, 0x00007FFFFFFFFFFF);
        assert_eq!(VM_MIN_KERNEL_ADDRESS, 0xFFFF800000000000);
    }

    #[test]
    fn test_size_limits() {
        assert_eq!(MAXTSIZ, 256 * 1024 * 1024);
        assert_eq!(DFLDSIZ, 512 * 1024 * 1024);
        assert_eq!(MAXDSIZ, 128u64 * 1024 * 1024 * 1024);
        assert_eq!(MAXSSIZ, 64 * 1024 * 1024);
    }
}
