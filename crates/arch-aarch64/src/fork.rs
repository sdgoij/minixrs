//! Fork page-table construction for AArch64.
//!
//! `vm_paging_fork` builds a fresh child root from a parent process's page
//! table. It lives in its own module (not `hal`, which is gated to the
//! aarch64 target) so it compiles and is unit-testable on the host.

use crate::pte::{PTE_ADDR_MASK, PTE_AP_MASK, PTE_AP_RO, PTE_ATTR_MASK, PTE_BLOCK, PTE_VALID};

/// Build the child's page table for fork. Walks PGD -> PUD -> PMD -> PTE,
/// giving the child its own copy of every table page, then COW-protects the
/// child's view of each owned user 4KB page (same frame, AP = read-only) —
/// the parent's PTE stays writable and untouched. Block mappings and kernel
/// pages are shared verbatim so the child keeps the same access to the
/// kernel identity map, device MMIO, and low-GB RAM alias as the parent.
///
/// Returns 0 on success, -12 (ENOMEM) on allocation failure.
///
/// # Safety
///
/// `parent_cr3` must be a valid page table root and `child_cr3` a freshly
/// allocated zero-filled root page.
pub unsafe fn vm_paging_fork(parent_cr3: u64, child_cr3: u64, _msg: &mut [u8; 64]) -> i32 {
    unsafe {
        let parent_root = parent_cr3 as *const u64;
        let child_root = child_cr3 as *mut u64;
        // Copy the root PGD entries; table entries are deep-copied below.
        core::ptr::copy_nonoverlapping(parent_root, child_root, 512);

        for pgd_idx in 0..512 {
            let pgd_e = core::ptr::read(parent_root.add(pgd_idx));
            if pgd_e & PTE_VALID == 0 || (pgd_e & 0b11) == PTE_BLOCK {
                continue;
            }
            let parent_pud = (pgd_e & PTE_ADDR_MASK) as *const u64;
            let child_pud = match crate::alloc::alloc_phys_page() {
                Some(pa) => pa as *mut u64,
                None => return -12,
            };
            core::ptr::copy_nonoverlapping(parent_pud, child_pud, 512);
            core::ptr::write(
                child_root.add(pgd_idx),
                (child_pud as u64) | (pgd_e & PTE_ATTR_MASK),
            );

            for pud_idx in 0..512 {
                let pud_e = core::ptr::read(parent_pud.add(pud_idx));
                if pud_e & PTE_VALID == 0 || (pud_e & 0b11) == PTE_BLOCK {
                    continue;
                }
                let parent_pmd = (pud_e & PTE_ADDR_MASK) as *const u64;
                let child_pmd = match crate::alloc::alloc_phys_page() {
                    Some(pa) => pa as *mut u64,
                    None => return -12,
                };
                core::ptr::copy_nonoverlapping(parent_pmd, child_pmd, 512);
                core::ptr::write(
                    child_pud.add(pud_idx),
                    (child_pmd as u64) | (pud_e & PTE_ATTR_MASK),
                );

                for pmd_idx in 0..512 {
                    let pmd_e = core::ptr::read(parent_pmd.add(pmd_idx));
                    if pmd_e & PTE_VALID == 0 || (pmd_e & 0b11) == PTE_BLOCK {
                        continue;
                    }
                    let parent_pt = (pmd_e & PTE_ADDR_MASK) as *const u64;
                    let child_pt = match crate::alloc::alloc_phys_page() {
                        Some(pa) => pa as *mut u64,
                        None => return -12,
                    };
                    core::ptr::copy_nonoverlapping(parent_pt, child_pt, 512);
                    core::ptr::write(
                        child_pmd.add(pmd_idx),
                        (child_pt as u64) | (pmd_e & PTE_ATTR_MASK),
                    );

                    // COW-share each EL0-accessible user 4KB page: the
                    // child maps the SAME frame read-only; the parent's PTE
                    // is never modified. Kernel pages (AP = EL1 only) stay
                    // shared verbatim.
                    for pt_idx in 0..512 {
                        let pte = core::ptr::read(parent_pt.add(pt_idx));
                        if pte & PTE_VALID == 0 || pte & PTE_AP_MASK == 0 {
                            continue;
                        }
                        let parent_pa = pte & PTE_ADDR_MASK;
                        if parent_pa == 0 {
                            continue;
                        }
                        let leaf_va =
                            (pgd_idx << 39) | (pud_idx << 30) | (pmd_idx << 21) | (pt_idx << 12);
                        // Shared low-GB alias / device-identity leaves (the
                        // leftover 4KB entries of a split 2MB alias block)
                        // belong to no process — keep them shared verbatim,
                        // exactly like the unsplit 2MB alias blocks above.
                        // COW-protecting them would make every boot server's
                        // live frame read-only in the child.
                        if crate::alloc::is_alias_frame(parent_pa, leaf_va as u64) {
                            core::ptr::write(child_pt.add(pt_idx), pte);
                            continue;
                        }
                        core::ptr::write(child_pt.add(pt_idx), (pte & !PTE_AP_MASK) | PTE_AP_RO);
                    }
                }
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{PhysicalMemoryMap, init_allocator};
    use crate::pte::{
        PTE_AF, PTE_AP_EL0_RW, PTE_AP_MASK, PTE_AP_RO, PTE_NG, PTE_SH_INNER, PTE_TABLE, PTE_TYPE,
        make_pte, pte_phys,
    };

    #[repr(align(4096))]
    struct PageAligned([u8; 0x40000]);

    static mut FAKE_RAM: PageAligned = PageAligned([0; 0x40000]);

    // 1GB kernel block: VALID + SH_INNER + AF, AP=EL1 only (shared verbatim).
    const KERNEL_BLOCK: u64 = 0x4000_0000 | 0x701;
    // 2MB user block: as above plus AP_EL0_RW (shared verbatim).
    const USER_BLOCK: u64 = 0x4000_0000 | 0x741;
    // Realistic L3 user page flags.
    const USER_PTE_FLAGS: u64 =
        PTE_VALID | PTE_TYPE | PTE_AF | PTE_AP_EL0_RW | PTE_SH_INNER | PTE_NG;

    fn init_fake_allocator() {
        unsafe {
            let base = &raw mut FAKE_RAM.0 as *mut u8 as u64;
            let mut mmap = PhysicalMemoryMap::new();
            mmap.add(base, base + 0x40000);
            init_allocator(&mmap);
        }
    }

    fn alloc_page() -> u64 {
        crate::alloc::alloc_phys_page().expect("fake allocator exhausted")
    }

    unsafe fn zero_page(pa: u64) {
        unsafe { core::ptr::write_bytes(pa as *mut u8, 0, 4096) };
    }

    unsafe fn rd(pa: u64, idx: usize) -> u64 {
        unsafe { core::ptr::read_volatile((pa as *const u64).add(idx)) }
    }

    unsafe fn wr(pa: u64, idx: usize, val: u64) {
        unsafe { core::ptr::write_volatile((pa as *mut u64).add(idx), val) };
    }

    /// Build the minimal parent layout used by both fork tests:
    /// PGD[0] -> PUD, PUD[0] -> PMD, PMD[0] -> PTE, with the kernel 1GB
    /// block at PUD[1], a user 2MB block at PMD[1], a user page at PTE[0]
    /// and a kernel page at PTE[1]. Returns (pgd, pte).
    unsafe fn build_parent() -> (u64, u64) {
        unsafe {
            let pgd = alloc_page();
            let pud = alloc_page();
            let pmd = alloc_page();
            let pte = alloc_page();
            let user_page = alloc_page();
            let kernel_page = alloc_page();
            zero_page(pgd);
            zero_page(pud);
            zero_page(pmd);
            zero_page(pte);
            wr(pud, 1, KERNEL_BLOCK);
            wr(pmd, 1, USER_BLOCK);
            wr(pte, 0, make_pte(user_page, USER_PTE_FLAGS));
            wr(pte, 1, make_pte(kernel_page, PTE_VALID | PTE_TYPE | PTE_AF));
            core::ptr::write_volatile(user_page as *mut u64, 0xDEADBEEF_CAFEBABE);
            wr(pgd, 0, make_pte(pud, PTE_TABLE));
            wr(pud, 0, make_pte(pmd, PTE_TABLE));
            wr(pmd, 0, make_pte(pte, PTE_TABLE));
            (pgd, pte)
        }
    }

    #[test]
    fn test_vm_paging_fork_cow_shares_user_pages() {
        init_fake_allocator();
        unsafe {
            let (parent_pgd, pte) = build_parent();
            let child_pgd = alloc_page();
            zero_page(child_pgd);

            let mut msg = [0u8; 64];
            let r = vm_paging_fork(parent_pgd, child_pgd, &mut msg);
            assert_eq!(r, 0, "vm_paging_fork should succeed");

            // Child root: own PUD table.
            let child_pud = pte_phys(rd(child_pgd, 0));
            assert_ne!(
                child_pud,
                pte_phys(rd(parent_pgd, 0)),
                "child must have its own PUD table"
            );
            assert_eq!(
                rd(child_pgd, 0) & 0b11,
                0b11,
                "child PGD[0] must be a table entry"
            );

            // Kernel 1GB block shared verbatim.
            assert_eq!(
                rd(child_pud, 1),
                KERNEL_BLOCK,
                "kernel block must be shared"
            );

            // Child PUD[0]: own PMD table.
            let parent_pud = pte_phys(rd(parent_pgd, 0));
            let child_pmd = pte_phys(rd(child_pud, 0));
            assert_ne!(
                child_pmd,
                pte_phys(rd(parent_pud, 0)),
                "child must have its own PMD table"
            );

            // User 2MB block shared verbatim.
            assert_eq!(rd(child_pmd, 1), USER_BLOCK, "user block must be shared");

            // Child PMD[0]: own PTE table.
            let child_pte = pte_phys(rd(child_pmd, 0));
            assert_ne!(child_pte, pte, "child must have its own PTE table");

            // User page: COW-shared — same frame, child read-only, parent RW.
            let parent_user_pte = rd(pte, 0);
            let child_user_pte = rd(child_pte, 0);
            assert_eq!(
                pte_phys(child_user_pte),
                pte_phys(parent_user_pte),
                "user page must share the parent's frame (COW)"
            );
            assert_eq!(
                child_user_pte & PTE_AP_MASK,
                PTE_AP_RO,
                "child's user page must be read-only"
            );
            assert_eq!(
                parent_user_pte & PTE_AP_MASK,
                PTE_AP_EL0_RW,
                "parent's user page stays writable"
            );
            assert_eq!(
                child_user_pte & !PTE_AP_MASK,
                parent_user_pte & !PTE_AP_MASK,
                "child PTE preserves all non-AP attributes"
            );

            // Kernel page shared verbatim.
            assert_eq!(rd(child_pte, 1), rd(pte, 1), "kernel page must be shared");

            // Parent tables untouched.
            assert_eq!(rd(parent_pgd, 0), make_pte(parent_pud, PTE_TABLE));
            assert_eq!(rd(pte, 0), parent_user_pte);
        }
    }

    #[test]
    fn test_vm_paging_fork_returns_enomem_when_exhausted() {
        init_fake_allocator();
        unsafe {
            let (parent_pgd, _pte) = build_parent();
            let child_pgd = alloc_page();
            zero_page(child_pgd);

            // Exhaust the fake allocator so the first internal allocation fails.
            while crate::alloc::alloc_phys_page().is_some() {}

            let mut msg = [0u8; 64];
            let r = vm_paging_fork(parent_pgd, child_pgd, &mut msg);
            assert_eq!(r, -12, "fork must fail with ENOMEM when out of pages");
        }
    }

    #[test]
    fn test_vm_paging_fork_shares_alias_leaves_verbatim() {
        let _guard = crate::alloc::WINDOW_TEST_LOCK.lock();
        init_fake_allocator();
        unsafe {
            let base = &raw mut FAKE_RAM.0 as *mut u8 as u64;
            // 16 MiB usable -> alias window 0x1000000, base = the fake RAM.
            crate::alloc::set_alias_window(base, 0x1000_0000);

            // Parent with the PTE table under PMD[8] (VA 0x1000000..0x1200000):
            // PTE[0] = a real user page (COW-shared, child read-only), PTE[1] = an
            // alias leaf (frame = base + ((0x1010000-USER_LOW) % win) = base+0x10000,
            // must be shared verbatim, never COW-protected).
            let pgd = alloc_page();
            let pud = alloc_page();
            let pmd = alloc_page();
            let pte = alloc_page();
            let user_page = alloc_page();
            zero_page(pgd);
            zero_page(pud);
            zero_page(pmd);
            zero_page(pte);
            wr(pud, 1, KERNEL_BLOCK);
            // PTE[1] VA = 0x1000000 (block 8) + 0x1000 = 0x1001000; its alias
            // frame = base + ((0x1001000 - USER_LOW) % win) = base + 0x1000.
            let alias_frame = base + 0x1000;
            wr(pte, 0, make_pte(user_page, USER_PTE_FLAGS));
            wr(pte, 1, make_pte(alias_frame, USER_PTE_FLAGS));
            core::ptr::write_volatile(user_page as *mut u64, 0xDEADBEEF_CAFEBABE);
            wr(pgd, 0, make_pte(pud, PTE_TABLE));
            wr(pud, 0, make_pte(pmd, PTE_TABLE));
            wr(pmd, 8, make_pte(pte, PTE_TABLE));

            // Replicate the fork's per-leaf inputs before calling it, to
            // isolate a window/VA mismatch.
            let parent_p1_pre = rd(pte, 1);
            // PGD[0] | PUD[0] | PMD[8] | PTE[1] (the PGD/PUD indices are 0).
            let leaf_va_pre = (8usize << 21) | (1usize << 12);
            assert_eq!(
                leaf_va_pre, 0x100_1000usize,
                "leaf VA decode: got {:#x}",
                leaf_va_pre
            );
            assert!(
                crate::alloc::is_alias_frame(
                    parent_p1_pre & crate::pte::PTE_ADDR_MASK,
                    leaf_va_pre as u64
                ),
                "pre-fork is_alias_frame must hold: pte={:#x}",
                parent_p1_pre
            );

            let child_pgd = alloc_page();
            zero_page(child_pgd);
            let mut msg = [0u8; 64];
            assert_eq!(vm_paging_fork(pgd, child_pgd, &mut msg), 0);

            let child_pud = pte_phys(rd(child_pgd, 0));
            let child_pmd = pte_phys(rd(child_pud, 0));
            let child_pte = pte_phys(rd(child_pmd, 8));

            // Real page: COW-shared — same frame, read-only in the child.
            let child_p0 = rd(child_pte, 0);
            let parent_p0 = rd(pte, 0);
            assert_eq!(
                pte_phys(child_p0),
                pte_phys(parent_p0),
                "real user page must share the parent's frame (COW)"
            );
            assert_eq!(
                child_p0 & PTE_AP_MASK,
                PTE_AP_RO,
                "real user page must be read-only in the child"
            );
            assert_eq!(
                parent_p0 & PTE_AP_MASK,
                PTE_AP_EL0_RW,
                "real user page stays writable in the parent"
            );
            // Alias leaf: shared verbatim (same frame, same PTE).
            let child_p1 = rd(child_pte, 1);
            let parent_p1 = rd(pte, 1);
            assert_eq!(
                child_p1,
                parent_p1,
                "alias leaf shared: child={:#x} parent={:#x} base={:#x} alias={:#x} is_alias={}",
                child_p1,
                parent_p1,
                base,
                alias_frame,
                crate::alloc::is_alias_frame(alias_frame, 0x100_1000),
            );
        }
    }
}
