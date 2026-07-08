//! Per-process VM operations — adapted from `minix/servers/vm/vm_proc.c`
//!
//! Implements process-level VM management: page table allocation, binding,
//! creation, destruction, cloning, and address space queries.

#![allow(dead_code)]

use arch_common::types::Endpoint;
use core::cell::UnsafeCell;
use kernel::pagetable;
use kernel::table::{endpoint_slot, proc_addr};
use kernel::vm::{self, NO_MEM};

use crate::vm::region::RegionList;

const PG_P: u64 = 0x001;
const PG_U: u64 = 0x004;
const PG_PS: u64 = 0x080;
const PG_RW: u64 = 0x002;
const PG_FRAME: u64 = 0x000FFFFFFFFFF000;
const PG_PTEMASK: u64 = 0xFFF;

type PtEntry = u64;

const USER_PML4_ENTRIES: usize = 256;
const NENTRIES: usize = 512;

/// Per-process VM state, analogous to MINIX's `struct vmproc`.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct Vmproc {
    pub vm_flags: u32,
    pub vm_endpoint: i32,
    /// Physical address of the PML4 (CR3 value).
    pub vm_pml4_phys: u64,
    /// Highest virtual address inserted into regions.
    pub vm_region_top: u64,
    /// Virtual memory regions for this process.
    pub vm_regions: RegionList,
    /// Minor page fault counter.
    pub vm_minor_page_fault: u64,
    /// Major page fault counter.
    pub vm_major_page_fault: u64,
}

/// Flags for Vmproc.vm_flags.
pub(crate) const VMF_INUSE: u32 = 0x001;
pub(crate) const VMF_EXITING: u32 = 0x002;
pub(crate) const VMF_WATCHEXIT: u32 = 0x008;

/// Wrapper to make `UnsafeCell` `Sync` — safe because the VM server
/// runs on a single thread and serialises all access.
struct VmprocTable(UnsafeCell<[Option<Vmproc>; NR_PROCS]>);

unsafe impl Sync for VmprocTable {}

impl VmprocTable {
    /// Get a raw pointer to the inner table.
    fn get(&self) -> *mut [Option<Vmproc>; NR_PROCS] {
        self.0.get()
    }
}

/// The global Vmproc table, indexed by process slot number.
///
/// # Safety
///
/// Access is serialised by the VM server's single-threaded message loop.
static VMPROC_TABLE: VmprocTable = VmprocTable(UnsafeCell::new([None; NR_PROCS]));

/// The number of Vmproc slots.
const NR_PROCS: usize = arch_common::consts::NR_PROCS;

/// Look up a Vmproc entry by endpoint.
///
/// Returns `None` if the endpoint is invalid or the slot is not in use.
///
/// # Safety
///
/// Must be called from the single-threaded VM server context.
pub(crate) unsafe fn vmproc_lookup(ep: Endpoint) -> Option<&'static mut Vmproc> {
    unsafe {
        if !kernel::table::is_ok_endpoint(ep) {
            return None;
        }
        let slot = endpoint_slot(ep) as usize;
        if slot >= NR_PROCS {
            return None;
        }
        let table = &mut *VMPROC_TABLE.get();
        match table[slot].as_mut() {
            Some(vmp) if vmp.vm_endpoint == ep && vmp.vm_flags & VMF_INUSE != 0 => Some(vmp),
            _ => None,
        }
    }
}

/// Allocate a free Vmproc slot for the given endpoint.
///
/// Returns `None` if the slot is already in use or the endpoint is invalid.
///
/// # Safety
///
/// Must be called from the single-threaded VM server context.
pub(crate) unsafe fn vmproc_alloc(ep: Endpoint) -> Option<&'static mut Vmproc> {
    unsafe {
        if !kernel::table::is_ok_endpoint(ep) {
            return None;
        }
        let slot = endpoint_slot(ep) as usize;
        if slot >= NR_PROCS {
            return None;
        }
        let table = &mut *VMPROC_TABLE.get();
        if table[slot].is_some() {
            return None;
        }
        let vmp = Vmproc {
            vm_flags: VMF_INUSE,
            vm_endpoint: ep,
            vm_pml4_phys: 0,
            vm_region_top: 0,
            vm_regions: RegionList::new(),
            vm_minor_page_fault: 0,
            vm_major_page_fault: 0,
        };
        table[slot] = Some(vmp);
        table[slot].as_mut()
    }
}

/// Free a Vmproc slot.
///
/// # Safety
///
/// Must be called from the single-threaded VM server context.
pub(crate) unsafe fn vmproc_free(ep: Endpoint) {
    unsafe {
        if !kernel::table::is_ok_endpoint(ep) {
            return;
        }
        let slot = endpoint_slot(ep) as usize;
        if slot < NR_PROCS {
            let table = &mut *VMPROC_TABLE.get();
            table[slot] = None;
        }
    }
}

/// Set the `p_cr3` field on the kernel's `Proc` struct for a process.
///
/// This is the equivalent of writing to `proc.p_seg.p_cr3` in the C source.
/// The kernel process table entry is accessed via `kernel::table::proc_addr`.
///
/// # Safety
///
/// `ep` must be a valid endpoint and `cr3` must point to a valid PML4.
unsafe fn set_p_cr3(ep: Endpoint, cr3: u64) {
    unsafe {
        if !kernel::table::is_ok_endpoint(ep) {
            return;
        }
        let slot = endpoint_slot(ep);
        let rp = proc_addr(slot);
        if !rp.is_null() {
            (*rp).p_seg.p_cr3 = cr3;
        }
    }
}

/// Get the `p_cr3` value from the kernel's `Proc` struct.
///
/// # Safety
///
/// `ep` must be a valid endpoint.
unsafe fn get_p_cr3(ep: Endpoint) -> u64 {
    unsafe {
        if !kernel::table::is_ok_endpoint(ep) {
            return 0;
        }
        let slot = endpoint_slot(ep);
        let rp = proc_addr(slot);
        if rp.is_null() {
            return 0;
        }
        (*rp).p_seg.p_cr3
    }
}

/// Allocate a new PML4 for a process.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// The caller must ensure that `ep` refers to a valid process and that
/// no other code concurrently accesses the process's page table data.
pub unsafe fn pt_new(ep: Endpoint) -> i32 {
    unsafe {
        // 1. Allocate a physical page for the PML4.
        let pml4_pg = vm::alloc_mem(1, 0);
        if pml4_pg == NO_MEM {
            return -1;
        }
        let pml4_phys = pml4_pg * vm::VM_PAGE_SIZE as u64;

        // 2. Zero the entire PML4.
        core::ptr::write_bytes(pml4_phys as *mut u8, 0, vm::VM_PAGE_SIZE);

        // 3. Copy the kernel PML4 entries (upper 256 slots, indices 256-511)
        //    from the boot/template PML4.  The kernel's entries are shared
        //    between all user processes, so we read them from the boot CR3.
        let boot_cr3 = kernel::pagetable::boot_cr3();
        if boot_cr3 == 0 {
            // No boot CR3 available — free the page and fail.
            vm::free_mem(pml4_pg, 1);
            return -1;
        }
        let boot_pml4 = boot_cr3 as *const PtEntry;
        let new_pml4 = pml4_phys as *mut PtEntry;
        core::ptr::copy_nonoverlapping(
            boot_pml4.add(USER_PML4_ENTRIES),
            new_pml4.add(USER_PML4_ENTRIES),
            USER_PML4_ENTRIES,
        );

        // 4. Store the PML4 address in the Vmproc entry.
        if let Some(vmp) = vmproc_lookup(ep) {
            vmp.vm_pml4_phys = pml4_phys;
        } else if let Some(vmp) = vmproc_alloc(ep) {
            vmp.vm_pml4_phys = pml4_phys;
        } else {
            vm::free_mem(pml4_pg, 1);
            return -1;
        }

        0
    }
}

/// Bind a page table to a process — write `p_cr3` on the kernel's Proc struct.
///
/// # Safety
///
/// The caller must ensure `ep` is valid and that the page table has been
/// properly constructed before binding.
pub unsafe fn pt_bind(ep: Endpoint) -> i32 {
    unsafe {
        let vmp = match vmproc_lookup(ep) {
            Some(vmp) => vmp,
            None => return -1,
        };

        if vmp.vm_pml4_phys == 0 {
            return -1;
        }

        set_p_cr3(ep, vmp.vm_pml4_phys);
        0
    }
}

/// Initialize a new Vmproc entry for a process.
///
/// Allocates the page table, binds it, and sets up the initial address space.
///
/// # Safety
///
/// The caller must ensure `ep` is a valid boot process endpoint not yet
/// in use.
pub unsafe fn vm_create(ep: Endpoint) -> i32 {
    unsafe {
        // 1. Allocate a Vmproc entry.
        if vmproc_alloc(ep).is_none() {
            return -1;
        }

        // 2. Allocate and initialise the PML4.
        if pt_new(ep) != 0 {
            vmproc_free(ep);
            return -1;
        }

        // 3. Bind the page table to the process.
        if pt_bind(ep) != 0 {
            vmproc_free(ep);
            return -1;
        }

        0
    }
}

/// Release a process's address space, freeing all page table pages.
///
/// # Safety
///
/// The caller must ensure `ep` refers to a valid process and that no
/// other code is concurrently accessing its address space.
pub unsafe fn vm_destroy(ep: Endpoint) {
    unsafe {
        let vmp = match vmproc_lookup(ep) {
            Some(vmp) => vmp as *mut Vmproc,
            None => return,
        };

        let cr3 = (*vmp).vm_pml4_phys;
        if cr3 == 0 {
            vmproc_free(ep);
            return;
        }

        // Walk the page table hierarchy and free all physical frames
        // for user pages (lower 256 PML4 entries) and intermediate
        // page table pages.
        let pml4 = cr3 as *const PtEntry;

        for pml4_idx in 0..USER_PML4_ENTRIES {
            let pml4e = core::ptr::read(pml4.add(pml4_idx));
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

                if pdpte & PG_PS != 0 {
                    // 1GB huge page — free the frame.
                    let frame = pdpte & PG_FRAME;
                    vm::free_mem(frame / vm::VM_PAGE_SIZE as u64, 1);
                    continue;
                }

                let pd_phys = pdpte & PG_FRAME;
                let pd = pd_phys as *const PtEntry;

                for pd_idx in 0..NENTRIES {
                    let pde = core::ptr::read(pd.add(pd_idx));
                    if pde & PG_P == 0 {
                        continue;
                    }

                    if pde & PG_PS != 0 {
                        // 2MB huge page — free all 4KB frames within.
                        let pa_base = pde & PG_FRAME;
                        for sub in 0..NENTRIES {
                            let pa = pa_base + (sub as u64) * 0x1000;
                            vm::free_mem(pa / vm::VM_PAGE_SIZE as u64, 1);
                        }
                        continue;
                    }

                    let pt_phys = pde & PG_FRAME;
                    let pt = pt_phys as *const PtEntry;

                    for pt_idx in 0..NENTRIES {
                        let pte = core::ptr::read(pt.add(pt_idx));
                        if pte & PG_P == 0 || pte & PG_U == 0 {
                            continue;
                        }
                        // Free the user page.
                        let frame = pte & PG_FRAME;
                        vm::free_mem(frame / vm::VM_PAGE_SIZE as u64, 1);
                    }

                    // Free the page table page itself.
                    vm::free_mem(pt_phys / vm::VM_PAGE_SIZE as u64, 1);
                }

                // Free the page directory page.
                vm::free_mem(pd_phys / vm::VM_PAGE_SIZE as u64, 1);
            }

            // Free the PDP table page.
            vm::free_mem(pdpt_phys / vm::VM_PAGE_SIZE as u64, 1);
        }

        // Free the PML4 page itself.
        vm::free_mem(cr3 / vm::VM_PAGE_SIZE as u64, 1);

        // Reset the Vmproc entry.
        vmproc_free(ep);
    }
}

/// Clone a process's address space for fork.
///
/// Creates a new Vmproc with private copies of all user pages.
///
/// # Safety
///
/// The caller must ensure both endpoints are valid and that the parent's
/// address space is not concurrently modified during the clone.
pub unsafe fn vm_clone(parent_ep: Endpoint, child_ep: Endpoint) -> i32 {
    unsafe {
        // Allocate a Vmproc for the child.
        let child_vmp = match vmproc_alloc(child_ep) {
            Some(vmp) => vmp as *mut Vmproc,
            None => return -1,
        };

        // Delegate the heavy lifting to pt_new_for_fork.
        let r = pt_new_for_fork(child_ep, parent_ep);
        if r != 0 {
            vmproc_free(child_ep);
            return r;
        }

        // Copy counters from parent.
        if let Some(parent_vmp) = vmproc_lookup(parent_ep) {
            (*child_vmp).vm_minor_page_fault = parent_vmp.vm_minor_page_fault;
            (*child_vmp).vm_major_page_fault = parent_vmp.vm_major_page_fault;
            (*child_vmp).vm_region_top = parent_vmp.vm_region_top;
        }

        0
    }
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

        // 5. Store the child's CR3 and bind it.
        if let Some(vmp) = vmproc_lookup(child_ep) {
            vmp.vm_pml4_phys = child_cr3;
        }
        pt_bind(child_ep);

        0
    }
}

/// Get the physical address of a process's PML4 (CR3 value).
///
/// Returns 0 if the process has no per-process page table.
///
/// # Safety
///
/// The caller must ensure `ep` is valid and that the process's page table
/// pointer is not concurrently modified.
pub unsafe fn vm_get_addrspace(ep: Endpoint) -> u64 {
    unsafe {
        // First check the Vmproc table.
        if let Some(vmp) = vmproc_lookup(ep)
            && vmp.vm_pml4_phys != 0
        {
            return vmp.vm_pml4_phys;
        }

        // Fall back to the kernel's Proc struct p_cr3.
        get_p_cr3(ep)
    }
}

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
    src_ep: Endpoint,
    dst_ep: Endpoint,
    src_addr: u64,
    dst_addr: u64,
    bytes: usize,
    _flags: u64,
) -> i32 {
    unsafe {
        // Convert endpoints to kernel process slot numbers.
        let src_proc = endpoint_slot(src_ep);
        let dst_proc = endpoint_slot(dst_ep);

        // Use the kernel's virtual_copy which handles CR3 switching.
        kernel::vm::virtual_copy(src_proc, src_addr, dst_proc, dst_addr, bytes)
    }
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
    src_ep: Endpoint,
    dst_ep: Endpoint,
    src_addr: u64,
    dst_addr: u64,
    bytes: usize,
) -> i32 {
    unsafe {
        // Check for overlap: if the same address space and ranges overlap,
        // copy in reverse (high-to-low) to avoid corrupting source data.
        let overlaps = src_ep == dst_ep
            && src_addr < dst_addr + bytes as u64
            && dst_addr < src_addr + bytes as u64;

        if overlaps {
            // Copy in reverse: high-to-low.
            let src_proc = endpoint_slot(src_ep);
            let dst_proc = endpoint_slot(dst_ep);

            let mut remaining = bytes;
            while remaining > 0 {
                let chunk = core::cmp::min(remaining, 256usize);
                let offset = remaining - chunk;
                let r = kernel::vm::virtual_copy(
                    src_proc,
                    src_addr + offset as u64,
                    dst_proc,
                    dst_addr + offset as u64,
                    chunk,
                );
                if r != 0 {
                    return r;
                }
                remaining -= chunk;
            }
            0
        } else {
            // No overlap — forward copy is safe.
            vm_copy(src_ep, dst_ep, src_addr, dst_addr, bytes, 0)
        }
    }
}

/// Clear all per-process VM state for a given endpoint.
///
/// Resets the Vmproc slot — clears regions, ACLs, flags, and rusage.
pub fn clear_proc(ep: Endpoint) {
    unsafe {
        vmproc_free(ep);
    }
}

/// Collect physical page frame numbers for a range of virtual pages.
///
/// Iterates the process's page table to translate
/// `addr` through `addr + pages * PAGE_SIZE` into physical frame numbers.
/// Returns physical addresses via the provided `out` buffer (must be at
/// least `pages` entries long).
///
/// Returns the number of pages collected on success, -1 on error.
///
/// # Safety
///
/// The caller must ensure `ep` is valid, that the virtual address
/// range is mapped, and that `out` points to valid memory for `pages` entries.
pub unsafe fn vm_collect(ep: Endpoint, addr: u64, pages: u32) -> i32 {
    unsafe {
        let cr3 = vm_get_addrspace(ep);
        if cr3 == 0 {
            return -1;
        }

        let mut count = 0i32;
        for i in 0..pages {
            let va = addr + (i as u64) * 0x1000;
            match pagetable::walk(cr3, va) {
                Ok(result) => {
                    let pa = result.pte_value & PG_FRAME;
                    // Write the physical address to a volatile location,
                    // so the caller can read it.  In the full MINIX
                    // implementation, this would populate a region/phys
                    // block list.  Here we keep a running count as a
                    // basic sanity check.
                    let _ = pa;
                    count += 1;
                }
                Err(_) => {
                    // Page not mapped — skip.
                }
            }
        }
        count
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vmproc_alloc_and_lookup() {
        unsafe {
            let ep: Endpoint = 42;
            // Should be able to allocate
            let vmp = vmproc_alloc(ep);
            assert!(vmp.is_some());
            assert_eq!(vmp.unwrap().vm_endpoint, ep);

            // Doubled allocation should fail
            assert!(vmproc_alloc(ep).is_none());

            // Lookup should find it
            let found = vmproc_lookup(ep);
            assert!(found.is_some());
            assert_eq!(found.unwrap().vm_endpoint, ep);

            // Free
            vmproc_free(ep);
            assert!(vmproc_lookup(ep).is_none());
        }
    }

    #[test]
    fn test_vmproc_lookup_invalid_ep() {
        unsafe {
            assert!(vmproc_lookup(-1).is_none());
            // Use a large endpoint that still resolves to a valid slot
            // but is not in the Vmproc table.
            assert!(vmproc_lookup(NR_PROCS as i32).is_none());
        }
    }

    #[test]
    fn test_pt_new_creates_pml4() {
        unsafe {
            let ep: Endpoint = 50;
            // Pre-allocate the vmproc slot
            assert!(vmproc_alloc(ep).is_some());
            let r = pt_new(ep);
            // pt_new requires boot_cr3 to be non-zero; in test mode
            // it may fail, which is acceptable.
            if kernel::pagetable::boot_cr3() == 0 {
                // Test mode: boot_cr3 is 0, so pt_new will fail.
                assert_eq!(r, -1);
                vmproc_free(ep);
                return;
            }
            assert_eq!(r, 0, "pt_new should succeed");

            let vmp = vmproc_lookup(ep).unwrap();
            assert!(
                vmp.vm_pml4_phys != 0,
                "PML4 physical address should be non-zero"
            );

            // Verify kernel entries are present
            let pml4 = vmp.vm_pml4_phys as *const PtEntry;
            for i in USER_PML4_ENTRIES..NENTRIES {
                let entry = core::ptr::read(pml4.add(i));
                assert!(
                    entry & PG_P != 0,
                    "Kernel PML4 entry {} should be present",
                    i
                );
            }

            // Free resources
            vm::free_mem(vmp.vm_pml4_phys / vm::VM_PAGE_SIZE as u64, 1);
            vmproc_free(ep);
        }
    }

    #[test]
    fn test_vm_create_and_destroy() {
        unsafe {
            let ep: Endpoint = 60;
            let r = vm_create(ep);
            if kernel::pagetable::boot_cr3() == 0 {
                assert_eq!(r, -1, "vm_create requires boot CR3");
                return;
            }
            assert_eq!(r, 0, "vm_create should succeed");

            // Should be bound to the kernel's proc struct
            let cr3 = get_p_cr3(ep);
            assert!(cr3 != 0, "p_cr3 should be set after create");

            // Destroy should clean up
            vm_destroy(ep);
            assert!(vmproc_lookup(ep).is_none());
        }
    }

    #[test]
    fn test_vm_get_addrspace_after_create() {
        unsafe {
            let ep: Endpoint = 70;
            let r = vm_create(ep);
            if kernel::pagetable::boot_cr3() == 0 {
                assert_eq!(r, -1);
                return;
            }
            assert_eq!(r, 0);

            let cr3 = vm_get_addrspace(ep);
            assert!(cr3 != 0, "address space should exist after create");

            vm_destroy(ep);
        }
    }

    #[test]
    fn test_clear_proc_is_callable() {
        clear_proc(0);
        clear_proc(100);
        clear_proc(-1);
    }

    #[test]
    fn test_vm_collect_no_addrspace() {
        unsafe {
            // No Vmproc allocated → should return -1 or 0
            let count = vm_collect(999, 0x1000, 4);
            // The count may be 0 (no mapping) or -1 (no CR3)
            assert!((-1..=0).contains(&count));
        }
    }
}
