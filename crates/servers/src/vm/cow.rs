//! COW (Copy-on-Write) page fault handling for VM.
//!
//! When a forked process (parent or child) writes to a shared page,
//! the page fault handler in `mod.rs` detects it and calls
//! `handle_cow_fault` to allocate a private copy.

use crate::vm::pb;
use crate::vm::proc::Vmproc;
use kernel::pagetable::{self, PG_FRAME, PG_P, PG_PTEMASK, PG_RW};

/// Handle a COW page fault for process `vmp` at `fault_addr`.
///
/// Called when a write page fault occurs on a present, read-only page
/// that belongs to a writable region (shared via fork).
///
/// 1. Walk the page table to find the PTE and physical address.
/// 2. Find the `PhysBlock` for this physical address.
/// 3. If the refcount is 1 (last reference), just add RW permission.
/// 4. If the refcount > 1, allocate a new page, copy the data, and
///    remap as writable with a new PhysBlock.
///
/// Returns 0 on success, -1 on failure.
pub(crate) fn handle_cow_fault(vmp: &mut Vmproc, fault_addr: u64) -> i32 {
    let cr3 = vmp.vm_pml4_phys;
    if cr3 == 0 {
        return -1;
    }

    let page_size: u64 = kernel::vm::VM_PAGE_SIZE as u64;
    let page_addr = fault_addr & !(page_size - 1);

    // 1. Walk the page table to find the PTE.
    let walk_result = match unsafe { pagetable::walk(cr3, page_addr) } {
        Ok(r) => r,
        Err(_) => return -1,
    };
    if walk_result.level != 1 {
        // Huge pages shouldn't be COW candidates at this stage.
        return -1;
    }

    let pte_val = walk_result.pte_value;
    if pte_val & PG_P == 0 {
        // Not present — not a COW fault.
        return -1;
    }
    if pte_val & PG_RW != 0 {
        // Already writable — nothing to do.
        return 0;
    }

    let phys_addr = pte_val & PG_FRAME;
    if phys_addr == 0 {
        return -1;
    }

    // 2. Find the PhysBlock for this physical address.
    let pb_idx = match pb::pb_find(phys_addr) {
        Some(idx) => idx,
        None => {
            // No PhysBlock found — this shouldn't happen for a COW page.
            // Fall back to just making the page writable.
            let new_flags = (pte_val & PG_PTEMASK) | PG_RW;
            unsafe {
                let _ = pagetable::map_page(cr3, page_addr, phys_addr, new_flags);
            }
            return 0;
        }
    };

    // Check the refcount.
    let refcount = match pb::pb_get(pb_idx) {
        Some(block) => block.refcount,
        None => return -1,
    };

    if refcount == 1 {
        // 3. Last reference — just mark writable, no copy needed.
        let new_flags = (pte_val & PG_PTEMASK) | PG_RW;
        unsafe {
            match pagetable::map_page(cr3, page_addr, phys_addr, new_flags) {
                Ok(_) => 0,
                Err(_) => -1,
            }
        }
    } else {
        // 4. Refcount > 1 — allocate a new page and copy.
        // Use vm_alloc_pages (kernel call 62) instead of direct
        // kernel::vm::alloc_mem to avoid the static-data-duplication
        // issue (Blocker 5 class).
        let new_phys = crate::vm::vm_alloc_pages(1);
        if new_phys == 0 {
            return -1;
        }

        // Copy data from the old page to the new page via kernel call.
        // The kernel runs in ring 0 and can access all physical addresses
        // through the identity map (some pages are supervisor-only).
        let copy_result = crate::vm::vm_copy_pages(phys_addr, new_phys, 1);
        if copy_result != 0 {
            crate::vm::vm_free_pages(new_phys, 1);
            return -1;
        }

        // Remap the new page as writable.
        let new_flags = (pte_val & PG_PTEMASK) | PG_RW;
        let map_result = unsafe { pagetable::map_page(cr3, page_addr, new_phys, new_flags) };

        match map_result {
            Ok(_) => {
                // Decrement the old PhysBlock refcount.
                pb::pb_unref(pb_idx);

                // Create a new PhysBlock for the private copy.
                let _ = pb::pb_new(new_phys);
                0
            }
            Err(_) => {
                // Mapping failed — free the allocated page via kernel call.
                crate::vm::vm_free_pages(new_phys, 1);
                -1
            }
        }
    }
}
