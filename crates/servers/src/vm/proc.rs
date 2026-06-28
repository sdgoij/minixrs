//! Per-process VM operations — adapted from `minix/servers/vm/vm_proc.c`
//!
//! Implements process-level VM management: page table allocation, binding,
//! creation, destruction, cloning, and address space queries.

use arch_common::types::Endpoint;
use kernel::pagetable;
use kernel::vm::{self, NO_MEM};

// ── x86_64 page table constants ────────────────────────────────────────
// These mirror arch_x86_64::pte values for use in raw page table walks.
// They are architecture-specific constants, not magic numbers.

const PG_P: u64 = 0x001;
const PG_U: u64 = 0x004;
const PG_PS: u64 = 0x080;
const PG_FRAME: u64 = 0x000FFFFFFFFFF000;
const PG_PTEMASK: u64 = 0xFFF; // Low 12 bits

// Page table entry type (8 bytes on x86_64)
type PtEntry = u64;

const USER_PML4_ENTRIES: usize = 256;
const NENTRIES: usize = 512;

// ── Page table management ───────────────────────────────────────────────

/// Allocate a new page directory for a process.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// The caller must ensure that `_ep` refers to a valid process and that
/// no other code concurrently accesses the process's page table data.
pub unsafe fn pt_new(_ep: Endpoint) -> i32 {
    // TODO: Phase 6.5 full — allocate a new PML4 via kernel::vm::alloc_mem(),
    //       zero it, link shared kernel entries (BOOT_PDP, APIC MMIO),
    //       and store the result in the process's Vmproc entry.
    0
}

/// Bind a page table to a process — write p_cr3 on the kernel's Proc struct.
///
/// # Safety
///
/// The caller must ensure `_ep` is valid and that the page table has been
/// properly constructed before binding.
pub unsafe fn pt_bind(_ep: Endpoint) -> i32 {
    // TODO: Phase 6.5 full — issue a VMCTL syscall to the kernel to set
    //       the process's p_cr3 to the physical address of the PML4.
    0
}

// ── Process lifecycle ───────────────────────────────────────────────────

/// Initialize a new Vmproc entry for a boot process.
///
/// Allocates the page table, binds it, and sets up the initial address space.
///
/// # Safety
///
/// The caller must ensure `_ep` is a valid boot process endpoint not yet
/// in use.
pub unsafe fn vm_create(_ep: Endpoint) -> i32 {
    // TODO: Phase 6.5 full — create Vmproc entry, allocate regions for
    //       code, data, stack, heap. Call pt_new + pt_bind.
    0
}

/// Release a process's address space, freeing all page table pages.
///
/// # Safety
///
/// The caller must ensure `_ep` refers to a valid process and that no
/// other code is concurrently accessing its address space.
pub unsafe fn vm_destroy(_ep: Endpoint) {
    // TODO: Phase 6.5 full — walk the page table, free all physical frames
    //       allocated for code, stack, and page table hierarchy (PML4, PDP,
    //       PD, PT pages). Call into kernel::vm::free_mem for each page.
}

/// Clone a process's address space for fork.
///
/// Creates a new Vmproc with private copies of all user pages.
///
/// # Safety
///
/// The caller must ensure both endpoints are valid and that the parent's
/// address space is not concurrently modified during the clone.
pub unsafe fn vm_clone(_parent_ep: Endpoint, _child_ep: Endpoint) -> i32 {
    // TODO: Phase 6.5 full — walk parent's page table (via identity map),
    //       allocate new physical frames for each user page, copy data,
    //       build new page table hierarchy for child, bind it.
    0
}

/// Create a child page table with private copies of parent's user pages.
///
/// Walks the parent's page table (via identity map), allocates new physical
/// frames for each user page, copies data, and builds the child's page table.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// Both endpoints must be valid and the parent's address space must not be
/// concurrently modified.
pub unsafe fn pt_new_for_fork(child_ep: Endpoint, parent_ep: Endpoint) -> i32 {
    // SAFETY: the function-level safety invariant requires the caller to ensure
    // valid endpoints and no concurrent address space modification. Within the
    // body, all unsafe pointer operations are justified by this invariant.
    unsafe {
        // 1. Get parent's CR3 (physical address of the PML4)
        let parent_cr3 = vm_get_addrspace(parent_ep);
        if parent_cr3 == 0 {
            return -1;
        }

        // 2. Allocate a new PML4 for the child
        let child_pml4_pg = vm::alloc_mem(1, 0);
        if child_pml4_pg == NO_MEM {
            return -1;
        }
        let child_cr3 = child_pml4_pg * vm::VM_PAGE_SIZE as u64;

        // 3. Copy kernel entries from parent (upper 256 PML4 slots,
        //    indices 256-511). Kernel entries are shared between
        //    parent and child.
        let parent_pml4 = parent_cr3 as *const PtEntry;
        let child_pml4 = child_cr3 as *mut PtEntry;
        core::ptr::copy_nonoverlapping(
            parent_pml4.add(USER_PML4_ENTRIES),
            child_pml4.add(USER_PML4_ENTRIES),
            USER_PML4_ENTRIES,
        );

        // 4. Walk each user PML4 entry (0..USER_PML4_ENTRIES)
        //    and private-copy user-accessible 4KB pages.
        for pml4_idx in 0..USER_PML4_ENTRIES {
            let pml4e = core::ptr::read(parent_pml4.add(pml4_idx));
            if pml4e & PG_P == 0 {
                continue;
            }

            let pdpt_phys = pml4e & PG_FRAME;
            let pdpt = pdpt_phys as *const PtEntry;

            for pdpt_idx in 0..NENTRIES {
                let pdpte = core::ptr::read(pdpt.add(pdpt_idx));
                if pdpte & PG_P == 0 {
                    continue;
                }

                let va_l3 = (pml4_idx as u64) << 39 | (pdpt_idx as u64) << 30;

                if pdpte & PG_PS != 0 {
                    // 1GB huge page — shared identity mapping,
                    // skip private copy.
                    continue;
                }

                let pd_phys = pdpte & PG_FRAME;
                let pd = pd_phys as *const PtEntry;

                for pd_idx in 0..NENTRIES {
                    let pde = core::ptr::read(pd.add(pd_idx));
                    if pde & PG_P == 0 {
                        continue;
                    }

                    let va_l2 = va_l3 | (pd_idx as u64) << 21;

                    if pde & PG_PS != 0 {
                        // 2MB huge page — shared identity mapping.
                        // Each 4KB sub-page within the 2MB range
                        // shares the parent's physical frame.
                        let pa_base = pde & PG_FRAME;
                        let pte_flags = (pde & PG_PTEMASK) & !PG_PS;

                        for sub in 0..NENTRIES {
                            let va = va_l2 | (sub as u64) << 12;
                            let pa = pa_base + ((sub as u64) << 12);
                            if pagetable::map_page(
                                child_cr3,
                                va,
                                pa,
                                pte_flags | pagetable::MAP_PRESENT,
                            )
                            .is_err()
                            {
                                return -1;
                            }
                        }
                        continue;
                    }

                    let pt_phys = pde & PG_FRAME;
                    let pt = pt_phys as *const PtEntry;

                    for pt_idx in 0..NENTRIES {
                        let pte_val = core::ptr::read(pt.add(pt_idx));
                        if pte_val & PG_P == 0 || pte_val & PG_U == 0 {
                            continue;
                        }

                        let va = va_l2 | (pt_idx as u64) << 12;
                        let parent_pa = pte_val & PG_FRAME;

                        // Allocate a new physical frame for the child
                        let child_pg = vm::alloc_mem(1, 0);
                        if child_pg == NO_MEM {
                            return -1;
                        }
                        let child_pa = child_pg * vm::VM_PAGE_SIZE as u64;

                        // Copy data from parent's physical page to
                        // child's (identity-mapped: physical == virtual).
                        core::ptr::copy_nonoverlapping(
                            parent_pa as *const u8,
                            child_pa as *mut u8,
                            vm::VM_PAGE_SIZE,
                        );

                        // Map the child's page at the same virtual
                        // address, preserving parent's PTE flags (minus
                        // PG_PS since this is now a 4KB entry).
                        let map_flags = pte_val & !PG_PS;
                        if pagetable::map_page(child_cr3, va, child_pa, map_flags).is_err() {
                            return -1;
                        }
                    }
                }
            }
        }

        // 5. Bind the child's page table (currently a stub — will
        //    write p_cr3 when kernel process table integration is done).
        pt_bind(child_ep);

        0
    }
}

// ── Address space queries ───────────────────────────────────────────────

/// Get the physical address of a process's PML4 (CR3 value).
///
/// Returns 0 if the process has no per-process page table.
///
/// # Safety
///
/// The caller must ensure `_ep` is valid and that the process's page table
/// pointer is not concurrently modified.
pub unsafe fn vm_get_addrspace(_ep: Endpoint) -> u64 {
    // TODO: Phase 6.5 full — query kernel::proc for the process's p_cr3
    //       via the system call interface, or read it directly if
    //       this runs in-kernel.
    0
}

// ── Cross-address-space memory operations ─────────────────────────────────

/// Copy data from one process's address space to another.
///
/// Performs a cross-address-space memory copy from `src_ep`'s virtual address
/// `src_addr` to `dst_ep`'s virtual address `dst_addr` for `bytes` bytes.
/// `flags` may specify copy semantics (e.g., non-faulting behavior).
///
/// Returns 0 on success, nonzero on error.
///
/// # Safety
///
/// The caller must ensure both endpoints are valid and that the virtual
/// address ranges are mapped in their respective page tables.
pub unsafe fn vm_copy(
    _src_ep: Endpoint,
    _dst_ep: Endpoint,
    _src_addr: u64,
    _dst_addr: u64,
    _bytes: usize,
    _flags: u64,
) -> i32 {
    // TODO: Phase 6.6 full — walk source and destination page tables to
    //       validate that both virtual address ranges are mapped, then
    //       perform a physical-to-physical copy via kernel::vm::memcpy.
    0
}

/// Copy data between address spaces, handling overlapping ranges.
///
/// Like `vm_copy` but copies in reverse (from high address to low) when
/// the source and destination ranges overlap, to avoid corrupting data.
///
/// Returns 0 on success, nonzero on error.
///
/// # Safety
///
/// The caller must ensure both endpoints are valid and that the virtual
/// address ranges are mapped. Overlapping ranges are handled safely.
pub unsafe fn vm_copy_overwrite(
    _src_ep: Endpoint,
    _dst_ep: Endpoint,
    _src_addr: u64,
    _dst_addr: u64,
    _bytes: usize,
) -> i32 {
    // TODO: Phase 6.6 full — determine whether src_addr..src_addr+bytes
    //       overlaps with dst_addr..dst_addr+bytes.  If yes, copy in
    //       reverse (high-to-low) to avoid overwriting unread source data.
    0
}

/// Clear all per-process VM state for a given endpoint.
///
/// Resets the Vmproc slot — clears regions, ACLs, flags, and rusage.
pub fn clear_proc(_ep: Endpoint) {
    // TODO: Phase 6.12 full — reset the Vmproc entry:
    //   region_init(&vmp->vm_regions_avl);
    //   acl_clear(vmp);
    //   vmp->vm_flags = 0;
    //   vmp->vm_region_top = 0;
    //   reset_vm_rusage(vmp);
}

/// Collect physical page frame numbers for a range of virtual pages.
///
/// Iterates the process's regions and walks the page table to translate
/// `addr` through `addr + pages * PAGE_SIZE` into physical frame numbers.
///
/// Returns 0 on success, nonzero on error.
///
/// # Safety
///
/// The caller must ensure `_ep` is valid and that the virtual address
/// range is mapped in the process's page table.
pub unsafe fn vm_collect(_ep: Endpoint, _addr: u64, _pages: u32) -> i32 {
    // TODO: Phase 6.6 full — walk the Vmproc region list, then walk the
    //       page table for each page, collecting physical frame numbers
    //       into a caller-provided buffer or returning them via callback.
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pt_new_returns_zero() {
        unsafe {
            assert_eq!(pt_new(0), 0);
        }
    }

    #[test]
    fn test_pt_bind_returns_zero() {
        unsafe {
            assert_eq!(pt_bind(0), 0);
        }
    }

    #[test]
    fn test_vm_create_returns_zero() {
        unsafe {
            assert_eq!(vm_create(0), 0);
        }
    }

    #[test]
    fn test_vm_destroy_is_callable() {
        unsafe {
            vm_destroy(0);
        }
    }

    #[test]
    fn test_vm_clone_returns_zero() {
        unsafe {
            assert_eq!(vm_clone(0, 1), 0);
        }
    }

    #[test]
    fn test_vm_get_addrspace_returns_zero() {
        unsafe {
            assert_eq!(vm_get_addrspace(0), 0);
        }
    }

    #[test]
    fn test_vm_copy_returns_zero() {
        unsafe {
            assert_eq!(vm_copy(0, 1, 0x1000, 0x2000, 4096, 0), 0);
        }
    }

    #[test]
    fn test_vm_copy_overwrite_returns_zero() {
        unsafe {
            assert_eq!(vm_copy_overwrite(0, 1, 0x1000, 0x1100, 256), 0);
        }
    }

    #[test]
    fn test_vm_collect_returns_zero() {
        unsafe {
            assert_eq!(vm_collect(0, 0x1000, 4), 0);
        }
    }

    #[test]
    fn test_clear_proc_is_callable() {
        // clear_proc is safe (not unsafe)
        clear_proc(0);
        clear_proc(100);
        clear_proc(-1);
    }

    #[test]
    fn test_pt_new_for_fork_fails_with_no_addrspace() {
        // When vm_get_addrspace returns 0 (no address space),
        // pt_new_for_fork should fail with -1.
        unsafe {
            assert_eq!(pt_new_for_fork(1, 0), -1);
        }
    }
}
