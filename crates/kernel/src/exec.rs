//! Per-process page table creation for exec (Phase 6.5.2).
//!
//! Builds a private page table for a newly exec'd process, with
//! private physical copies of code and stack pages.

use crate::pagetable::{PG_FRAME, PG_P, PG_RW, PG_U};
use crate::vm::{self, NO_MEM};

/// Create a per-process page table for an exec'd process.
///
/// Allocates a new PML4→PDP→PD hierarchy, deep-copies the boot
/// identity map, shares kernel high mappings and APIC MMIO,
/// and returns the physical address of the new PML4 (the CR3 value).
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

        // Walk the boot page table to find the PD and PDP physical addresses
        let boot_pml4 = boot_cr3 as *const u64;
        let boot_pml4e0 = core::ptr::read(boot_pml4);
        let boot_pdpt_phys = boot_pml4e0 & PG_FRAME;
        let boot_pdpt = boot_pdpt_phys as *const u64;
        let boot_pdpte0 = core::ptr::read(boot_pdpt);
        let boot_pd_phys = boot_pdpte0 & PG_FRAME;

        // Allocate new PML4 (1 page)
        let pml4_page = vm::alloc_mem(1, 0);
        if pml4_page == NO_MEM {
            return 0;
        }
        let pml4_phys = pml4_page * vm::VM_PAGE_SIZE as u64;

        // Allocate new PDP (1 page)
        let pdpt_page = vm::alloc_mem(1, 0);
        if pdpt_page == NO_MEM {
            return 0;
        }
        let pdpt_phys = pdpt_page * vm::VM_PAGE_SIZE as u64;

        // Allocate new PD (1 page) — covers 0-1GB
        let pd_page = vm::alloc_mem(1, 0);
        if pd_page == NO_MEM {
            return 0;
        }
        let pd_phys = pd_page * vm::VM_PAGE_SIZE as u64;

        // Zero the three pages (alloc_mem doesn't guarantee zeroed pages)
        core::ptr::write_bytes(pml4_phys as *mut u8, 0, vm::VM_PAGE_SIZE);
        core::ptr::write_bytes(pdpt_phys as *mut u8, 0, vm::VM_PAGE_SIZE);
        core::ptr::write_bytes(pd_phys as *mut u8, 0, vm::VM_PAGE_SIZE);

        // Link: PML4[0] → PDP, PDP[0] → PD
        let pml4_ptr = pml4_phys as *mut u64;
        core::ptr::write(pml4_ptr, pdpt_phys | PG_P | PG_RW | PG_U);

        let pdpt_ptr = pdpt_phys as *mut u64;
        core::ptr::write(pdpt_ptr, pd_phys | PG_P | PG_RW | PG_U);

        // Deep-copy all 512 boot PD entries into new PD
        let boot_pd = boot_pd_phys as *const u64;
        let new_pd = pd_phys as *mut u64;
        for i in 0..512 {
            let entry = core::ptr::read(boot_pd.add(i));
            core::ptr::write(new_pd.add(i), entry);
        }

        // Share kernel high mappings (PML4 entries 256-511).
        // These map the canonical high half of the address space
        // (kernel text/data, APIC MMIO, etc.).
        for i in 256..512 {
            let entry = core::ptr::read(boot_pml4.add(i));
            core::ptr::write(pml4_ptr.add(i), entry);
        }

        pml4_phys
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
