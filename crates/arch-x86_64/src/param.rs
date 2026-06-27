//! x86_64 parameter constants — adapted from i386 `param.h`
//!
//! **x86_64 differences from i386:**
//! - Page size is 4KB (same), but PTE entries are 8 bytes (not 4)
//! - KERNBASE is at a high 64-bit address (0xFFFF800000000000+)
//! - NPTEPG = 512 (8-byte PTEs in 4KB page) vs 1024 (4-byte PTEs)
//! - MAXCPUS: same
//! - UPAGES/USPACE: larger kernel stacks typical on x86_64

use core::mem::size_of;

pub type PtEntry = u64;

/// Page size constants.
pub const PGSHIFT: u32 = 12;
pub const NBPG: u64 = 1 << PGSHIFT;
pub const PGOFSET: u64 = NBPG - 1;

/// Number of page table entries per page.
/// On x86_64: 4096 / 8 = 512 (8-byte PTEs).
pub const NPTEPG: u64 = NBPG / size_of::<PtEntry>() as u64;

/// Maximum number of CPUs.
pub const MAXCPUS: u32 = 32;

/// Kernel base virtual address.
pub const KERNBASE: u64 = 0xFFFF8000_00000000u64;

/// Start of kernel text (1MB after KERNBASE).
pub const KERNTEXTOFF: u64 = KERNBASE + 0x100000;

pub const DEV_BSHIFT: u32 = 9;
pub const DEV_BSIZE: u64 = 1 << DEV_BSHIFT;
pub const BLKDEV_IOSIZE: u32 = 2048;
pub const MAXPHYS: u32 = 64 * 1024;

pub const SSIZE: u32 = 1;
pub const SINCR: u32 = 1;

pub const UPAGES: u32 = 4;
pub const USPACE: u64 = (UPAGES as u64) * NBPG;
pub const INTRSTACKSIZE: u32 = 16384;

pub const MSGBUFSIZE: u64 = 8 * NBPG;

// ── Conversion macros (as const fns) ────────────────────────────────────

pub const fn round_page(x: u64) -> u64 {
    (x + PGOFSET) & !PGOFSET
}

pub const fn trunc_page(x: u64) -> u64 {
    x & !PGOFSET
}

pub const fn btop(x: u64) -> u64 {
    x >> PGSHIFT
}

pub const fn ptob(x: u64) -> u64 {
    x << PGSHIFT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_size() {
        assert_eq!(NBPG, 4096);
        assert_eq!(PGSHIFT, 12);
        assert_eq!(PGOFSET, 4095);
    }

    #[test]
    fn test_nptepg_x86_64() {
        assert_eq!(NPTEPG, 512);
    }

    #[test]
    fn test_kernbase() {
        assert_eq!(KERNBASE, 0xFFFF8000_00000000u64);
    }

    #[test]
    fn test_conversions() {
        assert_eq!(round_page(0x1234), 0x2000);
        assert_eq!(trunc_page(0x5678), 0x5000);
        assert_eq!(btop(0x8000), 8);
        assert_eq!(ptob(8), 0x8000);
    }
}
