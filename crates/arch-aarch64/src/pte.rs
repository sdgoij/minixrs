//! AArch64 page table entry format (4KB granule, 4-level).
//!
//! ARMv8-A VMSA: 4 translation levels (0-3), 48-bit VA,
//! 4KB/2MB/1GB pages. Descriptor format identical across levels:
//! bits[1:0] = 0b11 for table (L0-2) or page (L3),
//! bits[1:0] = 0b01 for block (L0-2).

/// AArch64 page table entry (8 bytes).
pub type PtEntry = u64;

/// Valid bit (bit 0). Always set for valid descriptors.
pub const PTE_VALID: u64 = 1 << 0;
/// Type bit (bit 1). Combined with bit 0:
///   0b11 = table (L0-2) or page (L3)
///   0b01 = block (L0-2)
pub const PTE_TYPE: u64 = 1 << 1;

/// Table descriptor: bits[1:0] = 0b11 (valid + type).
pub const PTE_TABLE: u64 = PTE_VALID | PTE_TYPE;
/// Block descriptor: bits[1:0] = 0b01 (valid only).
pub const PTE_BLOCK: u64 = PTE_VALID;

/// Memory attribute index (MAIR_EL1). 3 bits at [4:2].
/// Index 0 = normal memory (WB/WA), index 1 = device nGnRE.
pub const PTE_ATTR_INDX_SHIFT: u32 = 2;
pub const PTE_ATTR_NORMAL: u64 = 0 << PTE_ATTR_INDX_SHIFT;
pub const PTE_ATTR_DEVICE: u64 = 1 << PTE_ATTR_INDX_SHIFT;

/// Access Permission bits [7:6].
/// AP[2:1] = 01 → EL0 read/write, 00 → EL1 only, 11 → EL1/0 read-only.
pub const PTE_AP_SHIFT: u32 = 6;
pub const PTE_AP_MASK: u64 = 0b11 << PTE_AP_SHIFT;
pub const PTE_AP_EL0_RW: u64 = 1 << PTE_AP_SHIFT; // AP[2:1]=01
pub const PTE_AP_EL1_ONLY: u64 = 0 << PTE_AP_SHIFT; // AP[2:1]=00
pub const PTE_AP_RO: u64 = 3 << PTE_AP_SHIFT; // AP[2:1]=11

/// Shareability bits [9:8].
/// 0b11 = Inner Shareable, 0b00 = Non-shareable, 0b10 = Outer Shareable.
pub const PTE_SH_SHIFT: u32 = 8;
pub const PTE_SH_INNER: u64 = 3 << PTE_SH_SHIFT;

/// Access Flag (bit 10). Set by hardware on access if AF=0, or
/// set by software to indicate page is accessible.
pub const PTE_AF: u64 = 1 << 10;

/// Not-Global bit (bit 11). nG=0 means global (not flushed on TLBI).
pub const PTE_NG: u64 = 1 << 11;

/// Output address mask: bits [47:12].
pub const PTE_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;
/// Lower attribute mask: bits [11:0].
pub const PTE_ATTR_MASK: u64 = 0x0000_0000_0000_0FFF;

/// For a 2MB block at level 2, the output address field is bits [47:21].
/// Block address mask (level 2, 2MB): bits [47:21].
pub const PTE_BLOCK_ADDR_MASK: u64 = 0x0000_FFFF_FFE0_0000;

/// Level 0 index (bits 39-47): maps 512GB.
pub const fn l0_index(va: u64) -> usize {
    ((va >> 39) & 0x1FF) as usize
}

/// Level 1 index (bits 30-38): maps 1GB.
pub const fn l1_index(va: u64) -> usize {
    ((va >> 30) & 0x1FF) as usize
}

/// Level 2 index (bits 21-29): maps 2MB.
pub const fn l2_index(va: u64) -> usize {
    ((va >> 21) & 0x1FF) as usize
}

/// Level 3 index (bits 12-20): maps 4KB.
pub const fn l3_index(va: u64) -> usize {
    ((va >> 12) & 0x1FF) as usize
}

/// Build a PTE from a physical address and flags.
/// AArch64 stores the PA directly in bits [47:12].
pub const fn make_pte(phys_addr: u64, flags: u64) -> PtEntry {
    (phys_addr & PTE_ADDR_MASK) | (flags & PTE_ATTR_MASK)
}

/// Extract the physical address from a PTE.
pub const fn pte_phys(pte: PtEntry) -> u64 {
    pte & PTE_ADDR_MASK
}

/// Check if a PTE is present (valid bit 0 set).
pub const fn pte_present(pte: PtEntry) -> bool {
    (pte & PTE_VALID) != 0
}

/// Check if a PTE points to a block (large page: bits[1:0] = 0b01).
pub const fn pte_is_block(pte: PtEntry) -> bool {
    (pte & 0b11) == PTE_BLOCK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pte_size() {
        assert_eq!(size_of::<PtEntry>(), 8);
    }

    #[test]
    fn test_pte_bits() {
        assert_eq!(PTE_VALID, 1);
        assert_eq!(PTE_TYPE, 2);
        assert_eq!(PTE_TABLE, 3);
        assert_eq!(PTE_BLOCK, 1);
        assert_eq!(PTE_AF, 0x400);
        assert_eq!(PTE_AP_EL0_RW, 0x40);
        assert_eq!(PTE_AP_MASK, 0xC0);
        assert_eq!(PTE_AP_RO, 0xC0);
        assert_eq!(PTE_SH_INNER, 0x300);
        assert_eq!(PTE_NG, 0x800);
    }

    #[test]
    fn test_make_pte() {
        let pte = make_pte(0x40000000, PTE_TABLE);
        assert_eq!(pte, 0x40000003);
    }

    #[test]
    fn test_pte_phys() {
        let pte = 0x40000003;
        assert_eq!(pte_phys(pte), 0x40000000);
    }

    #[test]
    fn test_pte_is_block() {
        assert!(pte_is_block(0x40000001));
        assert!(!pte_is_block(0x40000003));
        assert!(!pte_is_block(0));
    }

    #[test]
    fn test_index_functions() {
        assert_eq!(l0_index(0), 0);
        assert_eq!(l1_index(0x40000000), 1);
        assert_eq!(l2_index(0x40000000), 0);
        assert_eq!(l3_index(0x40000000), 0);
        assert_eq!(l0_index(0x0000_0080_0000_0000), 1);
    }
}
