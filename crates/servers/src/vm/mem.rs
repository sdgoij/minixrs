//! VM memory grant management — adapted from `minix/servers/vm/vm_mem.c`
//!
//! Manages memory grants between endpoints in the system. Grants allow one
//! endpoint to share physical memory with another endpoint via the grant
//! table mechanism.

use core::cell::UnsafeCell;

use arch_common::com::{
    VMCTL_BOOTINHIBIT_CLEAR, VMCTL_CLEAR_PAGEFAULT, VMCTL_CLEARMAPCACHE, VMCTL_FLUSHTLB,
    VMCTL_GET_PDBR, VMCTL_I386_INVLPG, VMCTL_KERN_MAP_REPLY, VMCTL_KERN_PHYSMAP, VMCTL_MEMREQ_GET,
    VMCTL_MEMREQ_REPLY, VMCTL_NOPAGEZERO, VMCTL_SETADDRSPACE, VMCTL_VMINHIBIT_CLEAR,
    VMCTL_VMINHIBIT_SET,
};
use core::sync::atomic::Ordering;
use kernel::pagetable;
use kernel::table::{endpoint_slot, proc_addr};

/// Maximum number of endpoints supported by the grant table.
pub const MAX_ENDPOINTS: usize = 64;

/// Number of grant entries per endpoint.
pub const GRANTS_PER_ENDPOINT: usize = 16;

/// Grant type: direct physical memory access.
pub const GRANT_PHYS: u32 = 1;

/// Grant type: virtual address space sharing.
pub const GRANT_VIRT: u32 = 2;

/// A single grant entry in the endpoint grant table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Grant {
    /// Who granted this memory (source endpoint).
    pub g_grantor: i32,
    /// Who received the grant (destination endpoint).
    pub g_endpoint: i32,
    /// Virtual address of the granted region.
    pub g_vaddr: u64,
    /// Grant type: `GRANT_PHYS` or `GRANT_VIRT`.
    pub g_grant_type: u32,
    /// Physical address of the granted memory.
    pub g_physaddr: u64,
    /// Number of pages in the grant.
    pub g_npages: u32,
}

impl Grant {
    /// Returns a zero-initialised grant entry (all fields set to 0).
    ///
    /// Used as the const initialiser for the static grant table so that
    /// every entry starts in the "free" state (`g_grantor == 0`).
    pub const fn zeroed() -> Self {
        Grant {
            g_grantor: 0,
            g_endpoint: 0,
            g_vaddr: 0,
            g_grant_type: 0,
            g_physaddr: 0,
            g_npages: 0,
        }
    }
}

const GRANT_ZERO: Grant = Grant::zeroed();
const GRANT_ROW: [Grant; GRANTS_PER_ENDPOINT] = [GRANT_ZERO; GRANTS_PER_ENDPOINT];

/// Wrapper for `[[Grant; GRANTS_PER_ENDPOINT]; MAX_ENDPOINTS]`.
pub struct GrantTablesCell(UnsafeCell<[[Grant; GRANTS_PER_ENDPOINT]; MAX_ENDPOINTS]>);
unsafe impl Sync for GrantTablesCell {}
impl GrantTablesCell {
    pub const fn new(val: [[Grant; GRANTS_PER_ENDPOINT]; MAX_ENDPOINTS]) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut [[Grant; GRANTS_PER_ENDPOINT]; MAX_ENDPOINTS] {
        self.0.get()
    }
}

/// Global grant table: one row per endpoint, each row holding 16 grant slots.
///
/// A slot is "free" when its `g_grantor` field is 0.
pub static GRANT_TABLES: GrantTablesCell = GrantTablesCell::new([GRANT_ROW; MAX_ENDPOINTS]);

/// Find a free grant slot for the given endpoint.
///
/// Walks the endpoint's grant row looking for an entry where `g_grantor == 0`.
/// Returns a mutable reference to the free slot, or `None` if the endpoint
/// is out of range or the row is full.
///
/// # Safety
///
/// The caller must ensure single-threaded access to the mutable static
/// `GRANT_TABLES`. The VM server runs single-threaded.
pub unsafe fn find_free_grant(ep: i32) -> Option<&'static mut Grant> {
    let idx = ep as usize;
    if idx >= MAX_ENDPOINTS {
        return None;
    }
    // SAFETY: single-threaded access to GRANT_TABLES is serialised by
    // the caller. The VM server runs on a single thread and no other
    // code mutates the grant table concurrently.
    unsafe {
        let row = &mut (*GRANT_TABLES.get())[idx];
        row.iter_mut().find(|g| g.g_grantor == 0)
    }
}

/// Map a grant from source to destination address space.
///
/// Stub: validates the parameters and, for the `GRANT_PHYS` use case, returns
/// `vaddr` as the physical address.  The real implementation will walk the
/// source page tables to resolve the physical frame and program the
/// destination page table.
///
/// # Safety
///
/// The caller must ensure that `GRANT_TABLES` is not concurrently modified.
pub unsafe fn map_grant(_src_ep: i32, _dst_ep: i32, vaddr: u64, pages: u32) -> u64 {
    // Basic validation — real implementation will perform page table walks.
    if pages == 0 {
        return 0;
    }
    // Stub: treat grant as physical and return the virtual address as-if it
    // were the physical frame.  Will be replaced with real page-table logic.
    vaddr
}

/// System call: map memory from one endpoint to another.
///
/// Validates endpoints, finds a free grant slot, resolves the source address
/// via `map_grant`, and stores the completed grant entry.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// Requires exclusive access to `GRANT_TABLES`.
pub unsafe fn sys_vm_map(src_ep: i32, dst_ep: i32, src_addr: u64, pages: u32, _flags: u32) -> i32 {
    if src_ep < 0 || dst_ep < 0 || (dst_ep as usize) >= MAX_ENDPOINTS {
        return -1;
    }
    if pages == 0 {
        return -1;
    }

    // SAFETY: grant table is only accessed from the single-threaded VM
    // server; calling the grant helpers here is safe.
    let (grant, phys) = unsafe {
        let g = match find_free_grant(dst_ep) {
            Some(g) => g,
            None => return -1,
        };
        let p = map_grant(src_ep, dst_ep, src_addr, pages);
        (g, p)
    };
    if phys == 0 {
        return -1;
    }

    grant.g_grantor = src_ep;
    grant.g_endpoint = dst_ep;
    grant.g_vaddr = src_addr;
    grant.g_grant_type = GRANT_PHYS;
    grant.g_physaddr = phys;
    grant.g_npages = pages;
    0
}

/// VMCTL dispatcher.
///
/// Dispatches kernel VM control commands:
///
/// | Command                    | Description                              |
/// |----------------------------|------------------------------------------|
/// | `VMCTL_GET_PDBR`           | Get page directory base register          |
/// | `VMCTL_CLEAR_PAGEFAULT`    | Clear the page fault flag on a process    |
/// | `VMCTL_FLUSHTLB`           | Flush the TLB for an endpoint             |
/// | `VMCTL_SETADDRSPACE`       | Set a process's address space (CR3)       |
/// | `VMCTL_NOPAGEZERO`         | Disable zero-fill-on-demand for a region |
/// | `VMCTL_VMINHIBIT_SET`      | Set VM inhibit flag                       |
/// | `VMCTL_VMINHIBIT_CLEAR`    | Clear VM inhibit flag                     |
/// | `VMCTL_CLEARMAPCACHE`      | Clear the map cache                       |
/// | `VMCTL_BOOTINHIBIT_CLEAR`  | Clear the boot inhibit flag               |
/// | `VMCTL_MEMREQ_GET`         | Get memory request from kernel            |
/// | `VMCTL_MEMREQ_REPLY`       | Reply to a kernel memory request          |
/// | `VMCTL_KERN_PHYSMAP`       | Map physical memory for the kernel        |
/// | `VMCTL_KERN_MAP_REPLY`     | Reply to a kernel map request             |
///
/// # Safety
///
/// The caller must ensure the endpoint is valid and command args are
/// well-formed.
pub unsafe fn sys_vmctl(ep: i32, cmd: u32, arg: u32) -> i32 {
    unsafe {
        match cmd {
            VMCTL_GET_PDBR => {
                // Return the physical address of the page directory base.
                if ep < 0 {
                    // Use the boot CR3 for invalid/kernel endpoints.
                    let boot_cr3 = pagetable::boot_cr3();
                    boot_cr3 as i32
                } else {
                    let slot = endpoint_slot(ep);
                    let rp = proc_addr(slot);
                    if rp.is_null() {
                        return -1;
                    }
                    (*rp).p_seg.p_cr3 as i32
                }
            }
            VMCTL_CLEAR_PAGEFAULT => {
                // Forward to the kernel via SYS_VMCTL to avoid the
                // static-data-duplication issue (Blocker 5 class).
                // The kernel's do_vmctl_handler clears RTS_PAGEFAULT
                // on the real Proc struct.
                minix_rt::sys_vmctl_clear_pagefault(ep)
                    .map(|()| 0)
                    .unwrap_or(-1)
            }
            VMCTL_FLUSHTLB => {
                // Flush the TLB for the given endpoint.
                let slot = endpoint_slot(ep);
                let rp = proc_addr(slot);
                if rp.is_null() {
                    // Fall back to full TLB flush via CR3 reload.
                    let boot_cr3 = pagetable::boot_cr3();
                    if boot_cr3 != 0 {
                        pagetable::write_cr3(boot_cr3);
                    }
                    return 0;
                }
                let cr3 = (*rp).p_seg.p_cr3;
                if cr3 != 0 {
                    // Reload the same CR3 to flush the TLB.
                    pagetable::write_cr3(cr3);
                }
                0
            }
            VMCTL_SETADDRSPACE => {
                // Set a process's CR3 to a new page table.
                let cr3 = arg as u64;
                let slot = endpoint_slot(ep);
                let rp = proc_addr(slot);
                if rp.is_null() {
                    return -1;
                }
                (*rp).p_seg.p_cr3 = cr3;
                0
            }
            VMCTL_NOPAGEZERO => 0,
            VMCTL_VMINHIBIT_SET => 0,
            VMCTL_VMINHIBIT_CLEAR => 0,
            VMCTL_CLEARMAPCACHE => 0,
            VMCTL_BOOTINHIBIT_CLEAR => {
                // Clear the boot inhibit flag — make a process
                // runnable after boot-time initialization.
                let slot = endpoint_slot(ep);
                let rp = proc_addr(slot);
                if rp.is_null() {
                    return -1;
                }
                // Clear RTS_BOOTINHIBIT.
                const RTS_BOOTINHIBIT: u32 = 0x1000;
                let old = (*rp).p_rts_flags.load(Ordering::Relaxed);
                (*rp)
                    .p_rts_flags
                    .store(old & !RTS_BOOTINHIBIT, Ordering::Relaxed);
                0
            }
            VMCTL_MEMREQ_GET | VMCTL_MEMREQ_REPLY | VMCTL_KERN_PHYSMAP | VMCTL_KERN_MAP_REPLY
            | VMCTL_I386_INVLPG => 0,
            _ => -1,
        }
    }
}

/// Grant physical memory from source to destination endpoint.
///
/// Validates parameters, resolves the physical address via `map_grant`, and
/// stores the grant in the destination's table.
///
/// # Safety
///
/// Requires exclusive access to `GRANT_TABLES` and that `physaddr` points
/// to valid physical memory.
pub unsafe fn grant_physmem(src_ep: i32, dst_ep: i32, physaddr: u64, pages: u32) -> i32 {
    if src_ep < 0 || dst_ep < 0 || (dst_ep as usize) >= MAX_ENDPOINTS {
        return -1;
    }
    if pages == 0 {
        return -1;
    }

    // SAFETY: grant table is only accessed from the single-threaded VM
    // server; calling the grant helpers here is safe.
    let (vaddr, grant) = unsafe {
        let v = map_grant(src_ep, dst_ep, physaddr, pages);
        if v == 0 {
            return -1;
        }
        let g = match find_free_grant(dst_ep) {
            Some(g) => g,
            None => return -1,
        };
        (v, g)
    };

    grant.g_grantor = src_ep;
    grant.g_endpoint = dst_ep;
    grant.g_vaddr = vaddr;
    grant.g_grant_type = GRANT_PHYS;
    grant.g_physaddr = physaddr;
    grant.g_npages = pages;
    0
}

/// Allocate a grant entry for the given endpoint.
///
/// Validates that `physaddr` is page-aligned (multiple of 4096) and that
/// the page count is within a reasonable range (1..=1024).
///
/// # Safety
///
/// Requires exclusive access to `GRANT_TABLES` and that `physaddr` is
/// a valid page-aligned physical address.
pub unsafe fn grant_alloc(src_ep: i32, physaddr: u64, pages: u32) -> i32 {
    if src_ep < 0 || (src_ep as usize) >= MAX_ENDPOINTS {
        return -1;
    }
    if physaddr & 0xfff != 0 {
        return -1;
    }
    if pages == 0 || pages > 1024 {
        return -1;
    }

    // SAFETY: grant table is only accessed from the single-threaded VM
    // server; calling the grant helpers here is safe.
    let grant = unsafe {
        match find_free_grant(src_ep) {
            Some(g) => g,
            None => return -1,
        }
    };

    grant.g_grantor = src_ep;
    grant.g_endpoint = src_ep;
    grant.g_vaddr = physaddr;
    grant.g_grant_type = GRANT_PHYS;
    grant.g_physaddr = physaddr;
    grant.g_npages = pages;
    0
}

/// Free a grant entry matching the given physical address and page count.
///
/// Walks all endpoint grant tables looking for an entry with matching
/// `g_physaddr` and `g_npages` (and `g_grantor != 0`), then clears all
/// fields to mark the slot as free.
///
/// Returns 0 on success, -1 if no matching entry is found.
///
/// # Safety
///
/// Requires exclusive access to `GRANT_TABLES`.
pub unsafe fn grant_free(physaddr: u64, npages: u32) -> i32 {
    // SAFETY: single-threaded access to GRANT_TABLES; the VM server runs
    // on a single thread and no other code mutates the table concurrently.
    unsafe {
        let tables = GRANT_TABLES.get();
        for i in 0..MAX_ENDPOINTS {
            let row = &mut (*tables)[i];
            for grant in row.iter_mut() {
                if grant.g_physaddr == physaddr && grant.g_npages == npages && grant.g_grantor != 0
                {
                    grant.g_grantor = 0;
                    grant.g_endpoint = 0;
                    grant.g_vaddr = 0;
                    grant.g_grant_type = 0;
                    grant.g_physaddr = 0;
                    grant.g_npages = 0;
                    return 0;
                }
            }
        }
    }
    -1
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grant_zeroed() {
        let g = Grant::zeroed();
        assert_eq!(g.g_grantor, 0);
        assert_eq!(g.g_endpoint, 0);
        assert_eq!(g.g_vaddr, 0);
        assert_eq!(g.g_grant_type, 0);
        assert_eq!(g.g_physaddr, 0);
        assert_eq!(g.g_npages, 0);
    }

    #[test]
    fn test_grant_type_constants() {
        assert_eq!(GRANT_PHYS, 1);
        assert_eq!(GRANT_VIRT, 2);
    }

    #[test]
    fn test_grants_table_initially_zeroed() {
        unsafe {
            let tables = GRANT_TABLES.get();
            for ep_idx in 0..MAX_ENDPOINTS {
                let row = &(*tables)[ep_idx];
                for (slot_idx, grant) in row.iter().enumerate() {
                    assert_eq!(
                        grant.g_grantor, 0,
                        "ep={ep_idx} slot={slot_idx} g_grantor should be 0"
                    );
                    assert_eq!(
                        grant.g_grant_type, 0,
                        "ep={ep_idx} slot={slot_idx} g_grant_type should be 0"
                    );
                }
            }
        }
    }

    #[test]
    fn test_table_dimensions() {
        unsafe {
            assert_eq!((*GRANT_TABLES.get()).len(), MAX_ENDPOINTS);
            let tables = GRANT_TABLES.get();
            for i in 0..MAX_ENDPOINTS {
                assert_eq!((*tables)[i].len(), GRANTS_PER_ENDPOINT);
            }
        }
    }

    #[test]
    fn test_find_free_grant_on_clean_table() {
        unsafe {
            let g = find_free_grant(1);
            assert!(g.is_some());
            assert_eq!(g.unwrap().g_grantor, 0);
        }
    }

    #[test]
    fn test_find_free_grant_out_of_range() {
        unsafe {
            assert!(find_free_grant(-1).is_none());
            assert!(find_free_grant(64).is_none());
            assert!(find_free_grant(i32::MAX).is_none());
        }
    }

    #[test]
    fn test_find_free_grant_table_full() {
        unsafe {
            let ep = 7;
            // Fill all 16 slots for endpoint 7
            for i in 0..GRANTS_PER_ENDPOINT {
                let g = find_free_grant(ep).expect("should have a free slot");
                g.g_grantor = 100 + i as i32;
            }
            // Now the table should be full
            assert!(find_free_grant(ep).is_none());

            // Clean up — reset all slots
            for grant in (*GRANT_TABLES.get())[ep as usize].iter_mut() {
                grant.g_grantor = 0;
            }
        }
    }

    #[test]
    fn test_map_grant_zero_pages_returns_zero() {
        unsafe {
            // Zero pages should return 0
            assert_eq!(map_grant(0, 0, 0x1000, 0), 0);
        }
    }

    #[test]
    fn test_map_grant_valid_returns_vaddr() {
        unsafe {
            // Non-zero pages returns the vaddr as physaddr (stub)
            assert_eq!(map_grant(0, 0, 0x2000, 1), 0x2000);
            assert_eq!(map_grant(1, 2, 0x3000, 4), 0x3000);
        }
    }

    #[test]
    #[ignore = "requires kernel_call (ring 0)"]
    fn test_sys_vmctl_commands() {
        unsafe {
            // Unknown command should return -1
            assert_eq!(sys_vmctl(0, 0, 0), -1);

            // VMCTL_CLEAR_PAGEFAULT (12) — needs a valid endpoint
            // ep = 0 (idle task) should work
            assert_eq!(sys_vmctl(0, 12, 0), 0);

            // VMCTL_GET_PDBR (13) — return CR3 for endpoint
            let result = sys_vmctl(0, 13, 0);
            // Should return a non-negative value (CR3 is high address)
            assert!(result != -1, "GET_PDBR should succeed");

            // VMCTL_VMINHIBIT_SET (30)
            assert_eq!(sys_vmctl(0, 30, 1), 0);

            // VMCTL_FLUSHTLB (26)
            assert_eq!(sys_vmctl(0, 26, 0), 0);

            // VMCTL_NOPAGEZERO (18)
            assert_eq!(sys_vmctl(0, 18, 0), 0);

            // VMCTL_BOOTINHIBIT_CLEAR (33)
            assert_eq!(sys_vmctl(0, 33, 0), 0);

            // VMCTL_CLEARMAPCACHE (32)
            assert_eq!(sys_vmctl(0, 32, 0), 0);
        }
    }

    #[test]
    fn test_sys_vm_map_valid() {
        unsafe {
            let rc = sys_vm_map(1, 2, 0x1000, 1, 0);
            assert_eq!(rc, 0);

            // Verify the grant was stored
            let g = &(*GRANT_TABLES.get())[2][0];
            assert_eq!(g.g_grantor, 1);
            assert_eq!(g.g_endpoint, 2);
            assert_eq!(g.g_vaddr, 0x1000);
            assert_eq!(g.g_grant_type, GRANT_PHYS);
            assert_eq!(g.g_physaddr, 0x1000);
            assert_eq!(g.g_npages, 1);

            // Clean up
            (*GRANT_TABLES.get())[2][0] = Grant::zeroed();
        }
    }

    #[test]
    fn test_sys_vm_map_invalid_endpoint() {
        unsafe {
            assert_eq!(sys_vm_map(-1, 2, 0x1000, 1, 0), -1);
            assert_eq!(sys_vm_map(1, -1, 0x1000, 1, 0), -1);
            assert_eq!(sys_vm_map(1, 64, 0x1000, 1, 0), -1);
        }
    }

    #[test]
    fn test_sys_vm_map_zero_pages() {
        unsafe {
            assert_eq!(sys_vm_map(1, 2, 0x1000, 0, 0), -1);
        }
    }

    #[test]
    fn test_grant_physmem_valid() {
        unsafe {
            let rc = grant_physmem(1, 3, 0x2000, 2);
            assert_eq!(rc, 0);

            let g = &(*GRANT_TABLES.get())[3][0];
            assert_eq!(g.g_grantor, 1);
            assert_eq!(g.g_endpoint, 3);
            assert_eq!(g.g_grant_type, GRANT_PHYS);
            assert_eq!(g.g_physaddr, 0x2000);
            assert_eq!(g.g_npages, 2);

            (*GRANT_TABLES.get())[3][0] = Grant::zeroed();
        }
    }

    #[test]
    fn test_grant_physmem_invalid() {
        unsafe {
            assert_eq!(grant_physmem(-1, 3, 0x2000, 2), -1);
            assert_eq!(grant_physmem(1, -1, 0x2000, 2), -1);
            assert_eq!(grant_physmem(1, 3, 0x2000, 0), -1);
        }
    }

    #[test]
    fn test_grant_alloc_valid() {
        unsafe {
            let rc = grant_alloc(4, 0x3000, 8);
            assert_eq!(rc, 0);

            let g = &(*GRANT_TABLES.get())[4][0];
            assert_eq!(g.g_grantor, 4);
            assert_eq!(g.g_endpoint, 4);
            assert_eq!(g.g_grant_type, GRANT_PHYS);
            assert_eq!(g.g_physaddr, 0x3000);
            assert_eq!(g.g_npages, 8);

            (*GRANT_TABLES.get())[4][0] = Grant::zeroed();
        }
    }

    #[test]
    fn test_grant_alloc_not_page_aligned() {
        unsafe {
            assert_eq!(grant_alloc(4, 0x3001, 8), -1);
        }
    }

    #[test]
    fn test_grant_alloc_excessive_pages() {
        unsafe {
            assert_eq!(grant_alloc(4, 0x3000, 1025), -1);
            assert_eq!(grant_alloc(4, 0x3000, 0), -1);
        }
    }

    #[test]
    fn test_grant_free_finds_and_clears() {
        unsafe {
            // Allocate a grant
            let rc = grant_alloc(5, 0x4000, 4);
            assert_eq!(rc, 0);

            // Free it by physaddr + npages
            assert_eq!(grant_free(0x4000, 4), 0);

            // Slot should now be zeroed
            let g = &(*GRANT_TABLES.get())[5][0];
            assert_eq!(g.g_grantor, 0);
            assert_eq!(g.g_endpoint, 0);
            assert_eq!(g.g_physaddr, 0);
            assert_eq!(g.g_npages, 0);
        }
    }

    #[test]
    fn test_grant_free_no_match() {
        unsafe {
            // Empty table — nothing to free
            assert_eq!(grant_free(0x5000, 4), -1);
        }
    }

    #[test]
    fn test_grant_free_walks_all_tables() {
        unsafe {
            // Place a grant in endpoint 10, slot 3
            let g = &mut (*GRANT_TABLES.get())[10][3];
            g.g_grantor = 10;
            g.g_endpoint = 10;
            g.g_vaddr = 0x6000;
            g.g_grant_type = GRANT_PHYS;
            g.g_physaddr = 0x6000;
            g.g_npages = 16;

            // Free it — should find it across tables
            assert_eq!(grant_free(0x6000, 16), 0);

            // Verify cleared
            assert_eq!((*GRANT_TABLES.get())[10][3].g_grantor, 0);
            assert_eq!((*GRANT_TABLES.get())[10][3].g_physaddr, 0);
            assert_eq!((*GRANT_TABLES.get())[10][3].g_npages, 0);
        }
    }
}
