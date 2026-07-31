//! AArch64 VM parameter constants — 4KB granule, 4-level paging.
//!
//! 4KB + 4 levels: L0→L1→L2→L3, 9 bits per level, 48-bit VA.
//! User addresses: bits 55-48 = 0x00 (0x0000000000000000 – 0x0000FFFFFFFFFFFF)

/// L1: PUD shift (bits 30-38) — 1GB blocks.
pub const L1_SHIFT: u32 = 30;
/// L2: PMD shift (bits 21-29) — 2MB blocks.
pub const L2_SHIFT: u32 = 21;
/// L3: PT shift (bits 12-20) — 4KB pages.
pub const L3_SHIFT: u32 = 12;

pub const NBPD_L1: u64 = 1u64 << L1_SHIFT;
pub const NBPD_L2: u64 = 1u64 << L2_SHIFT;
pub const NBPD_L3: u64 = 1u64 << L3_SHIFT;

pub const NENTRIES: u64 = 512;

pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
pub const PAGE_MASK: u64 = PAGE_SIZE - 1;

/// Top of user stack.
pub const USRSTACK: u64 = 0x0000_0FFF_FFFF_E000;

pub const VM_MIN_ADDRESS: u64 = 0;
pub const VM_MAXUSER_ADDRESS: u64 = 0x0000_0FFF_FFFF_FFFF;
pub const VM_MAX_ADDRESS: u64 = 0xFFFF_FFFF_FFFF_FFFF;

pub const VM_MIN_KERNEL_ADDRESS: u64 = 0xFFFF_0000_0000_0000;
pub const VM_MAX_KERNEL_ADDRESS: u64 = 0xFFFF_FFFF_FFFF_FFFF;

pub const MAXTSIZ: u64 = 256 * 1024 * 1024;
pub const DFLDSIZ: u64 = 512 * 1024 * 1024;
pub const MAXDSIZ: u64 = 128 * 1024 * 1024 * 1024;
pub const DFLSSIZ: u64 = 8 * 1024 * 1024;
pub const MAXSSIZ: u64 = 64 * 1024 * 1024;

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
        assert_eq!(NBPD_L3, 4096);
        assert_eq!(NBPD_L2, 2 * 1024 * 1024);
        assert_eq!(NBPD_L1, 1024 * 1024 * 1024);
        assert_eq!(NENTRIES, 512);
    }

    #[test]
    fn test_address_ranges() {
        assert_eq!(VM_MAXUSER_ADDRESS, 0x0000_0FFF_FFFF_FFFF);
        assert_eq!(VM_MIN_KERNEL_ADDRESS, 0xFFFF_0000_0000_0000);
    }
}
