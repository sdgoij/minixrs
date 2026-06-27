//! x86_64 page table entry format — adapted from `pte.h`
//!
//! **x86_64 differences from i386:**
//! - PTEs are 8 bytes (not 4), giving 512 entries per page table
//! - 4-level paging: PML4 -> PDPT -> PD -> PT
//! - NX (No-Execute) bit at bit 63 (not present on i386 without PAE)
//! - PAT (Page Attribute Table) at bit 7 (same position, different encoding)
//! - PSE handled via PS bit at each level (2MB at PD, 1GB at PDPT)


/// x86_64 page table entry (8 bytes).
pub type PtEntry = u64;

// PTE bit definitions (common across page levels)

pub const PG_P: u64 = 0x0000000000000001;
pub const PG_RW: u64 = 0x0000000000000002;
pub const PG_U: u64 = 0x0000000000000004;
pub const PG_WT: u64 = 0x0000000000000008;
pub const PG_CD: u64 = 0x0000000000000010;
pub const PG_A: u64 = 0x0000000000000020;
pub const PG_D: u64 = 0x0000000000000040;
pub const PG_PS: u64 = 0x0000000000000080;
pub const PG_G: u64 = 0x0000000000000100;

/// Physical address mask (bits 12-51).
pub const PG_FRAME: u64 = 0x000FFFFFFFFFF000;

/// NX (No-Execute) bit — x86_64 only.
pub const PG_NX: u64 = 0x8000000000000000;

/// Low 12 bits of a PTE (flags).
pub const PG_PTEMASK: u64 = 0x0000000000000FFF;

// Page table index extraction (4-level paging, 9 bits per level)

pub const fn pml4_index(va: u64) -> usize {
    ((va >> 39) & 0x1FF) as usize
}
pub const fn pdpt_index(va: u64) -> usize {
    ((va >> 30) & 0x1FF) as usize
}
pub const fn pd_index(va: u64) -> usize {
    ((va >> 21) & 0x1FF) as usize
}
pub const fn pt_index(va: u64) -> usize {
    ((va >> 12) & 0x1FF) as usize
}

// PTE construction helpers

pub const fn make_pte(phys_addr: u64, flags: u64) -> PtEntry {
    (phys_addr & PG_FRAME) | (flags & PG_PTEMASK)
}

pub const fn pte_phys(pte: PtEntry) -> u64 {
    pte & PG_FRAME
}

pub const fn pte_present(pte: PtEntry) -> bool {
    (pte & PG_P) != 0
}

pub const fn pte_huge(pte: PtEntry) -> bool {
    (pte & PG_PS) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pte_size() {
        assert_eq!(size_of::<PtEntry>(), 8);
    }

    #[test]
    fn test_pg_flags() {
        assert_eq!(PG_P, 0x001);
        assert_eq!(PG_RW, 0x002);
        assert_eq!(PG_U, 0x004);
        assert_eq!(PG_PS, 0x080);
        assert_eq!(PG_G, 0x100);
        assert_eq!(PG_NX, 0x8000000000000000);
    }

    #[test]
    fn test_frame_mask() {
        assert_eq!(PG_FRAME, 0x000FFFFFFFFFF000);
    }

    #[test]
    fn test_index_extraction() {
        // Kernel virtual address at 0xFFFF800000000000
        let va = 0xFFFF800000000000u64;
        assert_eq!(pml4_index(va), 256);
        assert_eq!(pdpt_index(va), 0);
        assert_eq!(pd_index(va), 0);
        assert_eq!(pt_index(va), 0);
        
        // User address
        let uva = 0x00007F0000000000u64;
        assert_eq!(pml4_index(uva), 0xFE);
    }

    #[test]
    fn test_make_pte() {
        let pte = make_pte(0x200000, PG_P | PG_RW | PG_U);
        assert!(pte_present(pte));
        assert_eq!(pte_phys(pte), 0x200000);
    }
}
