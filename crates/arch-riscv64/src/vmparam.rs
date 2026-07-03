//! RISC-V64 VM parameter constants — SV39 paging.
//!
//! SV39: 3-level page table (L2 → L1 → L0), 9 bits per level, 39-bit VA.
//! User addresses: bit 38 = 0 (0x0000000000 – 0x0000003FFFFFFF)
//! Kernel addresses: bits 63–39 = all 1s (0xFFFFFF8000000000+)

// ── Paging level shifts (SV39) ───────────────────────────────────────────

/// L2: PML4 shift (bits 30-38)
pub const L2_SHIFT: u32 = 30;
/// L1: PD shift (bits 21-29)
pub const L1_SHIFT: u32 = 21;
/// L0: PT shift (bits 12-20)
pub const L0_SHIFT: u32 = 12;

/// Bytes mapped by one L2 entry (1 GB)
pub const NBPD_L2: u64 = 1u64 << L2_SHIFT;
/// Bytes mapped by one L1 entry (2 MB)
pub const NBPD_L1: u64 = 1u64 << L1_SHIFT;
/// Bytes mapped by one L0 entry (4 KB)
pub const NBPD_L0: u64 = 1u64 << L0_SHIFT;

/// Number of entries per page table (all levels, 9-bit index).
pub const NENTRIES: u64 = 512;

// ── Page size ───────────────────────────────────────────────────────────

pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
pub const PAGE_MASK: u64 = PAGE_SIZE - 1;

// ── Virtual address space layout (SV39) ─────────────────────────────────

/// Top of user stack (end of user address space).
pub const USRSTACK: u64 = 0x0000003FFFFFFFE000u64;

pub const VM_MIN_ADDRESS: u64 = 0;
pub const VM_MAXUSER_ADDRESS: u64 = 0x0000003FFFFFFFFFFFu64;
pub const VM_MAX_ADDRESS: u64 = 0xFFFFFFFFFFFFFFFFu64;

pub const VM_MIN_KERNEL_ADDRESS: u64 = 0xFFFFFF8000000000u64;
pub const VM_MAX_KERNEL_ADDRESS: u64 = 0xFFFFFFFFFFFFFFFFu64;

// ── Process size limits (same as x86_64 for now) ─────────────────────────

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
        assert_eq!(NBPD_L0, 4096);
        assert_eq!(NBPD_L1, 2 * 1024 * 1024);
        assert_eq!(NBPD_L2, 1024 * 1024 * 1024);
        assert_eq!(NENTRIES, 512);
    }

    #[test]
    fn test_address_ranges() {
        assert_eq!(VM_MIN_ADDRESS, 0);
        assert_eq!(VM_MAXUSER_ADDRESS, 0x0000003FFFFFFFFFFF);
        assert_eq!(VM_MIN_KERNEL_ADDRESS, 0xFFFFFF8000000000);
    }
}
