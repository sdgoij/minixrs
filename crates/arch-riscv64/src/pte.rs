//! RISC-V64 SV39 page table entry format.
//!
//! SV39: 3-level page table with 9-bit indices, 8-byte PTEs.
//! Level 2 (VPN[2]): bits 30-38, covers 1 GB
//! Level 1 (VPN[1]): bits 21-29, covers 2 MB
//! Level 0 (VPN[0]): bits 12-20, covers 4 KB

/// RISC-V page table entry (8 bytes).
pub type PtEntry = u64;

// PTE bit definitions (RISC-V privileged spec §4.4.1)

/// Valid — page table entry is valid.
pub const PTE_V: u64 = 1 << 0;
/// Readable — page is readable.
pub const PTE_R: u64 = 1 << 1;
/// Writable — page is writable.
pub const PTE_W: u64 = 1 << 2;
/// Executable — page is executable.
pub const PTE_X: u64 = 1 << 3;
/// User — page is accessible from U-mode.
pub const PTE_U: u64 = 1 << 4;
/// Global — TLB entry is not flushed on sfence.vma.
pub const PTE_G: u64 = 1 << 5;
/// Accessed — page has been accessed.
pub const PTE_A: u64 = 1 << 6;
/// Dirty — page has been written to.
pub const PTE_D: u64 = 1 << 7;

/// Combined R/W/X permission masks.
pub const PTE_RW: u64 = PTE_R | PTE_W;
pub const PTE_RX: u64 = PTE_R | PTE_X;
pub const PTE_RWX: u64 = PTE_R | PTE_W | PTE_X;

/// Physical address mask (bits 10-53, 44 bits of PPN).
/// Shifted left by 10 to match PTE layout (PPN[0] at bits 10-18, etc.)
pub const PTE_PPN_MASK: u64 = 0x003FFFFFFFFFFC00;
/// Physical address shift within PTE.
pub const PTE_PPN_SHIFT: u64 = 10;
/// Low 10 bits of a PTE (flags).
pub const PTE_FLAGS_MASK: u64 = 0x00000000000003FF;

// Page table index extraction (SV39: 3 levels, 9 bits each)

/// Level 2 index (bits 30-38) — maps 1 GB.
pub const fn l2_index(va: u64) -> usize {
    ((va >> 30) & 0x1FF) as usize
}

/// Level 1 index (bits 21-29) — maps 2 MB.
pub const fn l1_index(va: u64) -> usize {
    ((va >> 21) & 0x1FF) as usize
}

/// Level 0 index (bits 12-20) — maps 4 KB.
pub const fn l0_index(va: u64) -> usize {
    ((va >> 12) & 0x1FF) as usize
}

// PTE construction helpers

/// Build a PTE from a physical address and flags.
/// The physical address must be page-aligned (bits 0-11 = 0).
pub const fn make_pte(phys_addr: u64, flags: u64) -> PtEntry {
    (phys_addr & PTE_PPN_MASK) | (flags & PTE_FLAGS_MASK)
}

/// Extract the physical address from a PTE.
pub const fn pte_phys(pte: PtEntry) -> u64 {
    pte & PTE_PPN_MASK
}

/// Check if a PTE is present (valid bit set).
pub const fn pte_present(pte: PtEntry) -> bool {
    (pte & PTE_V) != 0
}

/// Check if a PTE points to a huge page (not a leaf at L2 or L1).
/// In SV39, a L2 or L1 entry with R/W/X bits set is a leaf (huge page).
pub const fn pte_leaf(pte: PtEntry) -> bool {
    (pte & (PTE_R | PTE_W | PTE_X)) != 0
}

/// Check if a PTE is a page table pointer (not a leaf).
pub const fn pte_branch(pte: PtEntry) -> bool {
    pte_present(pte) && !pte_leaf(pte)
}

/// Check permissions.
pub const fn pte_readable(pte: PtEntry) -> bool {
    (pte & PTE_R) != 0
}
pub const fn pte_writable(pte: PtEntry) -> bool {
    (pte & PTE_W) != 0
}
pub const fn pte_executable(pte: PtEntry) -> bool {
    (pte & PTE_X) != 0
}
pub const fn pte_user(pte: PtEntry) -> bool {
    (pte & PTE_U) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_pte_size() {
        assert_eq!(size_of::<PtEntry>(), 8);
    }

    #[test]
    fn test_pte_flags() {
        assert_eq!(PTE_V, 0x001);
        assert_eq!(PTE_R, 0x002);
        assert_eq!(PTE_W, 0x004);
        assert_eq!(PTE_X, 0x008);
        assert_eq!(PTE_U, 0x010);
    }

    #[test]
    fn test_pte_ppn_mask() {
        // PPN covers bits 10-53
        assert_eq!(PTE_PPN_MASK, 0x003FFFFFFFFFFC00);
        assert_eq!(PTE_FLAGS_MASK, 0x3FF);
    }

    #[test]
    fn test_make_pte() {
        let phys = 0x8000_0000u64; // page-aligned
        let pte = make_pte(phys, PTE_V | PTE_R | PTE_W);
        assert!(pte_present(pte));
        assert!(pte_readable(pte));
        assert!(pte_writable(pte));
        assert!(!pte_executable(pte));
        assert_eq!(pte_phys(pte), phys & PTE_PPN_MASK);
    }

    #[test]
    fn test_pte_leaf_branch() {
        let leaf = make_pte(0x8000_0000, PTE_V | PTE_R | PTE_W);
        assert!(pte_leaf(leaf));
        assert!(!pte_branch(leaf));

        let branch = make_pte(0x8000_0000, PTE_V); // no R/W/X = page table pointer
        assert!(!pte_leaf(branch));
        assert!(pte_branch(branch));
    }

    #[test]
    fn test_page_table_indices() {
        // VA 0xFFFFFF80_00000000: L2 index
        let va: u64 = 0xFFFFFF80_00000000;
        assert_eq!(l2_index(va), ((va >> 30) & 0x1FF) as usize);
        assert_eq!(l1_index(va), ((va >> 21) & 0x1FF) as usize);
        assert_eq!(l0_index(va), ((va >> 12) & 0x1FF) as usize);

        // Each index is in range [0, 512)
        assert!(l2_index(va) < 512);
        assert!(l1_index(va) < 512);
        assert!(l0_index(va) < 512);
    }

    #[test]
    fn test_pte_user_permissions() {
        let user_page = make_pte(0x1000, PTE_V | PTE_R | PTE_W | PTE_X | PTE_U);
        assert!(pte_user(user_page));
        assert!(pte_readable(user_page));
        assert!(pte_executable(user_page));

        let kernel_page = make_pte(0x1000, PTE_V | PTE_R | PTE_W);
        assert!(!pte_user(kernel_page));
    }
}
