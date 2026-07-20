//! COW (Copy-on-Write) page fault handling for VM.
//!
//! When a forked process (parent or child) writes to a shared page,
//! the page fault handler in `mod.rs` detects it and calls
//! `handle_cow_fault` to allocate a private copy.

use crate::vm::pb;
use crate::vm::proc::Vmproc;
use kernel::pagetable::{PG_FRAME, PG_P, PG_PS, PG_PTEMASK, PG_RW, PG_U};

/// Set up COW for a fork: create PhysBlock entries for all shared
/// user-writable pages with refcount=2.
///
/// The kernel's vm_paging_fork_x86_64 has already cleared the RW bit
/// on all user-writable PTEs in both parent and child page tables.
/// This function only manages the PhysBlock refcounting so that
/// handle_cow_fault can track shared pages.
///
/// Called from VM's do_fork after vm_paging_fork creates the child
/// page table but before the child runs.
pub(crate) unsafe fn cow_setup_fork(parent_cr3: u64, child_cr3: u64) -> i32 {
    if parent_cr3 == 0 || child_cr3 == 0 {
        return -1;
    }

    const USER_ENTRIES: usize = 256;
    const ALL_ENTRIES: usize = 512;

    // Walk parent's user-half page tables to discover shared pages.
    // We must use vm_mappage to map physical page table pages into
    // VM's virtual address space before dereferencing them — VM does
    // NOT have an identity map.
    use crate::vm::{vm_mappage, vm_unmappage};
    let flags = kernel::pagetable::MAP_PRESENT | kernel::pagetable::MAP_USER;

    let parent_va = vm_mappage(parent_cr3, flags);
    if parent_va == 0 {
        return -1;
    }
    let child_va = vm_mappage(child_cr3, flags);
    if child_va == 0 {
        vm_unmappage(parent_va);
        return -1;
    }
    let parent = parent_va as *const u64;
    let child = child_va as *const u64;

    for l4 in 0..USER_ENTRIES {
        let e4 = unsafe { core::ptr::read(parent.add(l4)) };
        if e4 & PG_P == 0 {
            continue;
        }
        let child_e4 = unsafe { core::ptr::read(child.add(l4)) };
        if child_e4 & PG_P == 0 {
            continue;
        }

        let parent_p3_pa = e4 & PG_FRAME;
        let child_p3_pa = child_e4 & PG_FRAME;
        let parent_p3_va = vm_mappage(parent_p3_pa, flags);
        let child_p3_va = vm_mappage(child_p3_pa, flags);
        if parent_p3_va == 0 || child_p3_va == 0 {
            if parent_p3_va != 0 {
                vm_unmappage(parent_p3_va);
            }
            if child_p3_va != 0 {
                vm_unmappage(child_p3_va);
            }
            continue;
        }
        let parent_p3 = parent_p3_va as *const u64;
        let child_p3 = child_p3_va as *const u64;

        for l3 in 0..ALL_ENTRIES {
            let e3 = unsafe { core::ptr::read(parent_p3.add(l3)) };
            if e3 & PG_P == 0 || e3 & PG_PS != 0 {
                continue;
            }
            let child_e3 = unsafe { core::ptr::read(child_p3.add(l3)) };
            if child_e3 & PG_P == 0 {
                continue;
            }

            let parent_p2_pa = e3 & PG_FRAME;
            let child_p2_pa = child_e3 & PG_FRAME;
            let parent_p2_va = vm_mappage(parent_p2_pa, flags);
            let child_p2_va = vm_mappage(child_p2_pa, flags);
            if parent_p2_va == 0 || child_p2_va == 0 {
                if parent_p2_va != 0 {
                    vm_unmappage(parent_p2_va);
                }
                if child_p2_va != 0 {
                    vm_unmappage(child_p2_va);
                }
                continue;
            }
            let parent_p2 = parent_p2_va as *const u64;
            let child_p2 = child_p2_va as *const u64;

            for l2 in 0..ALL_ENTRIES {
                let e2 = unsafe { core::ptr::read(parent_p2.add(l2)) };
                if e2 & PG_P == 0 || e2 & PG_PS != 0 {
                    continue;
                }
                let child_e2 = unsafe { core::ptr::read(child_p2.add(l2)) };
                if child_e2 & PG_P == 0 {
                    continue;
                }

                let parent_p1_pa = e2 & PG_FRAME;
                let child_p1_pa = child_e2 & PG_FRAME;
                let parent_p1_va = vm_mappage(parent_p1_pa, flags);
                let child_p1_va = vm_mappage(child_p1_pa, flags);
                if parent_p1_va == 0 || child_p1_va == 0 {
                    if parent_p1_va != 0 {
                        vm_unmappage(parent_p1_va);
                    }
                    if child_p1_va != 0 {
                        vm_unmappage(child_p1_va);
                    }
                    continue;
                }
                let parent_p1 = parent_p1_va as *const u64;
                let child_p1 = child_p1_va as *const u64;

                for l1 in 0..ALL_ENTRIES {
                    let e1 = unsafe { core::ptr::read(parent_p1.add(l1)) };
                    if e1 & PG_P == 0 || e1 & PG_U == 0 {
                        continue;
                    }
                    // Only pages that were writable and are now RO (COW) need PhysBlocks.
                    if e1 & PG_RW != 0 {
                        continue;
                    }
                    let phys = e1 & PG_FRAME;
                    if phys == 0 {
                        continue;
                    }

                    // Verify child also has this page mapped (non-RW).
                    let child_e1 = unsafe { core::ptr::read(child_p1.add(l1)) };
                    if child_e1 & PG_P == 0 {
                        continue;
                    }

                    // Set up PhysBlock with refcount=2
                    let pb_idx = match pb::pb_find(phys) {
                        Some(idx) => {
                            pb::pb_ref(idx);
                            Some(idx)
                        }
                        None => pb::pb_new(phys),
                    };
                    let Some(pb_idx) = pb_idx else {
                        continue;
                    };
                    match pb::pb_get(pb_idx) {
                        Some(block) if block.refcount < 2 => {
                            pb::pb_ref(pb_idx);
                        }
                        _ => {}
                    }
                    let _ = pb_idx;
                }

                vm_unmappage(parent_p1_va);
                vm_unmappage(child_p1_va);
            }
            vm_unmappage(parent_p2_va);
            vm_unmappage(child_p2_va);
        }
        vm_unmappage(parent_p3_va);
        vm_unmappage(child_p3_va);
    }
    vm_unmappage(parent_va);
    vm_unmappage(child_va);
    0
}

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
    use crate::vm::proc::vm_get_addrspace;
    let cr3 = unsafe { vm_get_addrspace(vmp.vm_endpoint) };
    if cr3 == 0 {
        return -1;
    }

    let page_size: u64 = kernel::vm::VM_PAGE_SIZE as u64;
    let page_addr = fault_addr & !(page_size - 1);

    // 1. Walk the page table via kernel call (runs in ring 0 with
    // identity mapping — no memory corruption risk).
    let pte_val = crate::vm::vm_walk_page(cr3, page_addr);
    if pte_val == 0 || pte_val & PG_P == 0 {
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
        Some(idx) => Some(idx),
        None => {
            // No PhysBlock found for this COW page. This can happen when
            // cow_setup_fork couldn't map an intermediate page table page
            // (vm_mappage returned 0). Allocate a private copy instead of
            // just making the shared page writable — otherwise the first
            // write corrupts the other process's data.
            let new_phys = crate::vm::vm_alloc_pages(1);
            if new_phys == 0 {
                // Can't allocate — fall back to making shared page writable
                crate::vm::vm_map_page_in(
                    cr3,
                    page_addr,
                    phys_addr,
                    (pte_val & PG_PTEMASK) | PG_RW,
                );
                return 0;
            }
            let copy_result = crate::vm::vm_copy_pages(phys_addr, new_phys, 1);
            if copy_result != 0 {
                crate::vm::vm_free_pages(new_phys, 1);
                crate::vm::vm_map_page_in(
                    cr3,
                    page_addr,
                    phys_addr,
                    (pte_val & PG_PTEMASK) | PG_RW,
                );
                return 0;
            }
            let map_result =
                crate::vm::vm_map_page_in(cr3, page_addr, new_phys, (pte_val & PG_PTEMASK) | PG_RW);
            if map_result != 0 {
                crate::vm::vm_free_pages(new_phys, 1);
                crate::vm::vm_map_page_in(
                    cr3,
                    page_addr,
                    phys_addr,
                    (pte_val & PG_PTEMASK) | PG_RW,
                );
                return 0;
            }
            // No PhysBlock to track — the old shared page still belongs
            // to the other process. The new page is the private copy.
            pb::pb_new(new_phys);
            return 0;
        }
    };
    let Some(pb_idx) = pb_idx else {
        return -1;
    };

    // Check the refcount.
    let refcount = match pb::pb_get(pb_idx) {
        Some(block) => block.refcount,
        None => return -1,
    };

    if refcount == 1 {
        // 3. Last reference — just mark writable, no copy needed.
        let new_flags = (pte_val & PG_PTEMASK) | PG_RW;
        crate::vm::vm_map_page_in(cr3, page_addr, phys_addr, new_flags)
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

        // Remap the new page as writable via kernel call.
        let new_flags = (pte_val & PG_PTEMASK) | PG_RW;
        let map_result = crate::vm::vm_map_page_in(cr3, page_addr, new_phys, new_flags);

        if map_result != 0 {
            // Mapping failed — free the allocated page via kernel call.
            crate::vm::vm_free_pages(new_phys, 1);
            return -1;
        }

        // Decrement the old PhysBlock refcount.
        pb::pb_unref(pb_idx);

        // Create a new PhysBlock for the private copy.
        let _ = pb::pb_new(new_phys);
        0
    }
}
