//! RISC-V64 parameter constants.
//!
//! Matches `arch-x86_64/src/param.rs` pattern but for RISC-V.

/// Page size (4KB, same as x86_64).
pub const PGSHIFT: u32 = 12;
pub const NBPG: u64 = 1 << PGSHIFT;
pub const PGOFSET: u64 = NBPG - 1;

/// Number of page table entries per page (8-byte PTEs).
pub const NPTEPG: u64 = 512;

/// Maximum number of CPUs (QEMU virt: up to 8 harts).
pub const MAXCPUS: u32 = 8;

/// Kernel base virtual address (SV39: 0xFFFFFF8000000000+).
pub const KERNBASE: u64 = 0xFFFFFF8000000000u64;

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

/// RISC-V kernel stack size (4KB per page, 4 pages).
pub const KERNEL_STACK_SIZE: u64 = 16384;

pub const INTRSTACKSIZE: u32 = 16384;
pub const MSGBUFSIZE: u64 = 8 * NBPG;

// ── Conversion macros ────────────────────────────────────────────────────

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
    fn test_nptepg() {
        assert_eq!(NPTEPG, 512);
    }

    #[test]
    fn test_kernbase() {
        assert_eq!(KERNBASE, 0xFFFFFF8000000000u64);
    }

    #[test]
    fn test_conversions() {
        assert_eq!(round_page(0x1234), 0x2000);
        assert_eq!(trunc_page(0x5678), 0x5000);
        assert_eq!(btop(0x8000), 8);
        assert_eq!(ptob(8), 0x8000);
    }
}
