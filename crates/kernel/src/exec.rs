//! Per-process page table creation for exec (Phase 6.5.2).
//!
//! Builds a private page table for a newly exec'd process, with
//! private physical copies of code and stack pages.

use crate::pagetable::{PG_P, PG_RW, PG_U};
use crate::vm::{self, NO_MEM};

/// Create a per-process page table for an exec'd process.
///
/// Allocates a new page table hierarchy (levels depend on arch),
/// deep-copies the boot identity map, shares kernel high mappings,
/// and returns the physical address of the root page table (CR3/SATP).
///
/// Returns 0 on failure (out of memory).
///
/// # Safety
///
/// Must be called with interrupts disabled, and only after `load_elf`
/// has written the binary to identity-mapped physical pages.
pub unsafe fn exec_setup_new_page_table() -> u64 {
    unsafe {
        let boot_cr3 = crate::hal::boot_cr3();
        if boot_cr3 == 0 {
            return 0; // boot not initialized
        }
        let levels = crate::hal::pt_levels();

        // Walk the boot page table to find the bottom-level page directory
        // (PD on x86_64, PMD on SV39). Iterates levels 2..N-1.
        let mut table_phys = boot_cr3;
        for level in (2..levels).rev() {
            let table = table_phys as *const u64;
            let idx = crate::hal::pt_index(0, level); // va=0 to get PML4[0]/PUD[0]
            let entry = core::ptr::read(table.add(idx));
            table_phys = crate::hal::pte_to_phys(entry);
        }
        let boot_pd_phys = table_phys;

        // Allocate level pages: (levels-1) for the hierarchy (root + PD).
        let n_pages = (levels - 1) as usize; // root + intermediate + PD
        let mut page_addrs: [u64; 4] = [0u64; 4]; // max 4 levels
        for entry in page_addrs.iter_mut().take(n_pages) {
            let p = vm::alloc_mem(1, 0);
            if p == NO_MEM {
                return 0;
            }
            *entry = p * vm::VM_PAGE_SIZE as u64;
            core::ptr::write_bytes(*entry as *mut u8, 0, vm::VM_PAGE_SIZE);
        }

        // Link hierarchy: root[0] → level[1][0] → ... → PD.
        // For x86_64 (4 levels): PML4[0] → PDPT[0] → PD
        // For SV39 (3 levels):    PUD[0]  → PMD      (already at bottom)
        let flags = PG_P | PG_RW | PG_U;
        for i in 0..(n_pages - 1) {
            let parent = page_addrs[i] as *mut u64;
            let child = page_addrs[i + 1];
            core::ptr::write(parent, child | flags);
        }

        // Deep-copy the boot PD entries into the new PD (shared identity map).
        let new_pd = page_addrs[n_pages - 1] as *mut u64;
        let boot_pd = boot_pd_phys as *const u64;
        for i in 0..512 {
            let entry = core::ptr::read(boot_pd.add(i));
            core::ptr::write(new_pd.add(i), entry);
        }

        // Share kernel high mappings (top half of address space).
        // For x86_64 PML4: entries 256-511.
        // For SV39 PUD: entries are arch-defined.
        let boot_root = boot_cr3 as *const u64;
        let new_root = page_addrs[0] as *mut u64;
        let half_entries = 512 / 2; // 256 entries
        for i in half_entries..512 {
            let entry = core::ptr::read(boot_root.add(i));
            core::ptr::write(new_root.add(i), entry);
        }

        page_addrs[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that exec_setup_new_page_table returns 0 when BOOT_CR3 is 0
    /// (uninitialized boot path). This is the only unit test that can run
    /// in user space — the function dereferences physical addresses to walk
    /// the boot page table, which requires the kernel's identity mapping.
    ///
    /// Full structural validation (linking, deep-copy, kernel high sharing)
    /// must be done in kernel integration tests or on real hardware.
    #[test]
    fn test_exec_setup_new_page_table_fails_without_boot_cr3() {
        unsafe {
            // BOOT_CR3 is initially 0 before hal::init().
            let result = exec_setup_new_page_table();
            assert_eq!(result, 0, "Should fail when BOOT_CR3 is 0");
        }
    }

    #[test]
    fn test_boot_cr3_initial_value_is_zero() {
        // Before hal::init() is called, boot_cr3() should be 0.
        // This verifies the static initializer works.
        assert_eq!(crate::hal::boot_cr3(), 0);
    }
}
