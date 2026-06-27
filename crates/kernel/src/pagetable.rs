//! x86_64 page table management (Phase 6.2)
//!
//! Provides page table allocation, walking, mapping, and unmapping
//! for the 4-level x86_64 paging scheme (PML4 → PDPT → PD → PT).
//!
//! Relies on the physical memory allocator (kernel::vm) for page frames.

use crate::vm::{self, NO_MEM};
use arch_x86_64::pte::{
    self, PG_FRAME, PG_P, PG_PTEMASK, PtEntry, pd_index, pdpt_index, pml4_index, pt_index,
};
use arch_x86_64::vmparam::VM_MAXUSER_ADDRESS;

pub const MAP_PRESENT: u64 = pte::PG_P;
pub const MAP_WRITE: u64 = pte::PG_RW;
pub const MAP_USER: u64 = pte::PG_U;
pub const MAP_NX: u64 = pte::PG_NX;

#[derive(Debug, Clone, Copy)]
pub struct PageWalkResult {
    pub pte_phys: u64,
    pub pte_virt: *mut PtEntry,
    pub pte_value: PtEntry,
    pub level: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTableError {
    OutOfMemory,
    NotMapped,
    AlreadyMapped,
    InvalidArgument,
}

unsafe fn alloc_pt_page() -> Result<u64, PageTableError> {
    unsafe {
        let page = vm::alloc_mem(1, 0);
        if page == NO_MEM {
            return Err(PageTableError::OutOfMemory);
        }
        Ok(page * vm::VM_PAGE_SIZE as u64)
    }
}

unsafe fn read_pte(pt_virt: *const PtEntry) -> PtEntry {
    unsafe { core::ptr::read(pt_virt) }
}

unsafe fn write_pte(pt_virt: *mut PtEntry, value: PtEntry) {
    unsafe {
        core::ptr::write(pt_virt, value);
    }
}

/// Walk the 4-level page table to find the PTE for a virtual address.
///
/// `cr3` is the physical address of the PML4 table.
///
/// # Safety
///
/// `cr3` must point to a valid, identity-mapped PML4 table.
pub unsafe fn walk(cr3: u64, va: u64) -> Result<PageWalkResult, PageTableError> {
    unsafe {
        let pml4 = cr3 as *const PtEntry;
        let pml4e = read_pte(pml4.add(pml4_index(va)));
        if pml4e & PG_P == 0 {
            return Err(PageTableError::NotMapped);
        }

        let pdpt_phys = pml4e & PG_FRAME;
        let pdpt = pdpt_phys as *const PtEntry;
        let pdpte = read_pte(pdpt.add(pdpt_index(va)));
        if pdpte & PG_P == 0 {
            return Err(PageTableError::NotMapped);
        }
        if pdpte & pte::PG_PS != 0 {
            return Ok(PageWalkResult {
                pte_phys: pdpt_phys + (pdpt_index(va) as u64) * 8,
                pte_virt: (pdpt_phys + (pdpt_index(va) as u64) * 8) as *mut PtEntry,
                pte_value: pdpte,
                level: 3,
            });
        }

        let pd_phys = pdpte & PG_FRAME;
        let pd = pd_phys as *const PtEntry;
        let pde = read_pte(pd.add(pd_index(va)));
        if pde & PG_P == 0 {
            return Err(PageTableError::NotMapped);
        }
        if pde & pte::PG_PS != 0 {
            return Ok(PageWalkResult {
                pte_phys: pd_phys + (pd_index(va) as u64) * 8,
                pte_virt: (pd_phys + (pd_index(va) as u64) * 8) as *mut PtEntry,
                pte_value: pde,
                level: 2,
            });
        }

        let pt_phys = pde & PG_FRAME;
        let pt = pt_phys as *const PtEntry;
        let pte = read_pte(pt.add(pt_index(va)));
        if pte & PG_P == 0 {
            return Err(PageTableError::NotMapped);
        }

        Ok(PageWalkResult {
            pte_phys: pt_phys + (pt_index(va) as u64) * 8,
            pte_virt: (pt_phys + (pt_index(va) as u64) * 8) as *mut PtEntry,
            pte_value: pte,
            level: 1,
        })
    }
}

/// Map a 4KB page. Allocates intermediate page tables as needed.
///
/// # Safety
///
/// `cr3` must point to a valid, identity-mapped PML4.
pub unsafe fn map_page(cr3: u64, va: u64, pa: u64, flags: u64) -> Result<(), PageTableError> {
    unsafe {
        let pte_flags = (flags & PG_PTEMASK) | PG_P;
        let pte_val = (pa & PG_FRAME) | pte_flags;
        let pml4 = cr3 as *mut PtEntry;

        let pml4e_addr = pml4.add(pml4_index(va));
        let pml4e = read_pte(pml4e_addr);
        let pdpt_phys = if pml4e & PG_P == 0 {
            let p = alloc_pt_page()?;
            write_pte(pml4e_addr, p | PG_P | pte::PG_RW | pte::PG_U);
            p
        } else {
            pml4e & PG_FRAME
        };

        let pdpt = pdpt_phys as *mut PtEntry;
        let pdpte_addr = pdpt.add(pdpt_index(va));
        let pdpte = read_pte(pdpte_addr);
        let pd_phys = if pdpte & PG_P == 0 {
            let p = alloc_pt_page()?;
            write_pte(pdpte_addr, p | PG_P | pte::PG_RW | pte::PG_U);
            p
        } else {
            pdpte & PG_FRAME
        };

        let pd = pd_phys as *mut PtEntry;
        let pde_addr = pd.add(pd_index(va));
        let pde = read_pte(pde_addr);
        let pt_phys = if pde & PG_P == 0 {
            let p = alloc_pt_page()?;
            write_pte(pde_addr, p | PG_P | pte::PG_RW | pte::PG_U);
            p
        } else {
            pde & PG_FRAME
        };

        let pt = pt_phys as *mut PtEntry;
        let pte_addr = pt.add(pt_index(va));
        if read_pte(pte_addr) & PG_P != 0 {
            return Err(PageTableError::AlreadyMapped);
        }
        write_pte(pte_addr, pte_val);
        Ok(())
    }
}

/// Unmap a single 4KB page. Returns the old PTE value.
///
/// # Safety
///
/// `cr3` must point to a valid PML4.
pub unsafe fn unmap_page(cr3: u64, va: u64) -> Result<PtEntry, PageTableError> {
    unsafe {
        let result = walk(cr3, va)?;
        if result.level != 1 {
            return Err(PageTableError::InvalidArgument);
        }
        write_pte(result.pte_virt, 0);
        arch_x86_64::asm::invlpg(va);
        Ok(result.pte_value)
    }
}

/// Unmap a range of pages.
///
/// # Safety
///
/// `cr3` must point to a valid PML4.
pub unsafe fn unmap_range(cr3: u64, va: u64, size: u64) -> Result<(), PageTableError> {
    unsafe {
        let start = va & !0xFFF;
        let end = ((va + size + 0xFFF) & !0xFFF).min(VM_MAXUSER_ADDRESS);
        let mut cur = start;
        while cur < end {
            let _ = unmap_page(cr3, cur);
            cur += 0x1000;
        }
        Ok(())
    }
}

pub const PF_PRESENT: u32 = 0x01;
pub const PF_WRITE: u32 = 0x02;
pub const PF_USER: u32 = 0x04;
pub const PF_RESERVED: u32 = 0x08;
pub const PF_INSTR: u32 = 0x10;

/// Page fault information for diagnostic / signal delivery.
#[derive(Debug, Clone, Copy)]
pub struct PageFaultInfo {
    pub va: u64,
    pub present: bool,
    pub write: bool,
    pub user: bool,
    pub reserved: bool,
    pub instruction: bool,
    pub protection: bool,
}

/// Decode a page fault error code into structured information.
pub fn decode_page_fault(va: u64, err: u32) -> PageFaultInfo {
    PageFaultInfo {
        va,
        present: err & PF_PRESENT != 0,
        write: err & PF_WRITE != 0,
        user: err & PF_USER != 0,
        reserved: err & PF_RESERVED != 0,
        instruction: err & PF_INSTR != 0,
        protection: err & PF_PRESENT != 0 && err & PF_WRITE != 0,
    }
}

/// Handle a page fault. Routes to the VM server for resolution.
///
/// Currently returns `false` (unhandled), which causes the kernel to send
/// SIGSEGV to the faulting process. In Phase 6.3+, this will forward the
/// fault to the VM server via IPC for demand paging, copy-on-write, etc.
///
/// # Safety
///
/// Must be called from the page fault interrupt handler with interrupts
/// disabled. `va` must be the value from CR2.
pub unsafe fn handle_page_fault(va: u64, err: u32) -> bool {
    let _info = decode_page_fault(va, err);
    // TODO: Phase 6.3 — forward to VM server via VM_PAGEFAULT message
    // TODO: Phase 12 — check process memory map for valid regions
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Initialize the VM allocator with a test memory chunk.
    unsafe fn init_vm() {
        let chunks = [vm::MemoryChunk {
            base: 0x1000,
            size: 0x10000,
        }];
        unsafe { vm::mem_init(&chunks) }
    }

    #[allow(unused)]
    unsafe fn setup_test_mapping() -> (u64, u64, u64) {
        unsafe {
            init_vm();
            let test_va = 0x10000000u64;
            let test_pa = 0x200000u64;
            let cr3 = vm::alloc_mem(1, 0);
            assert!(cr3 != NO_MEM, "failed to alloc PML4");
            let cr3_phys = cr3 * 4096;
            let r = map_page(cr3_phys, test_va, test_pa, MAP_PRESENT | MAP_WRITE);
            assert!(r.is_ok(), "map_page failed: {:?}", r);
            (cr3_phys, test_va, test_pa)
        }
    }

    #[test]
    fn test_constants() {
        assert_eq!(MAP_PRESENT, 0x001);
        assert_eq!(MAP_WRITE, 0x002);
        assert_eq!(MAP_USER, 0x004);
        assert_eq!(PF_PRESENT, 0x01);
        assert_eq!(PF_WRITE, 0x02);
        assert_eq!(PF_USER, 0x04);
        assert_eq!(PF_INSTR, 0x10);
    }

    #[test]
    fn test_decode_page_fault() {
        let info = decode_page_fault(0xdead, PF_WRITE | PF_USER);
        assert_eq!(info.va, 0xdead);
        assert!(!info.present);
        assert!(info.write);
        assert!(info.user);
        assert!(!info.reserved);
        assert!(!info.instruction);
    }

    #[test]
    fn test_decode_page_fault_protection() {
        // Protection fault: present + write
        let info = decode_page_fault(0x1000, PF_PRESENT | PF_WRITE);
        assert!(info.present);
        assert!(info.write);
        assert!(info.protection);
    }

    #[test]
    fn test_decode_page_fault_nx() {
        let info = decode_page_fault(0x2000, PF_INSTR);
        assert!(info.instruction);
        assert!(!info.protection, "NX fault is not a protection fault");
    }

    #[test]
    fn test_page_fault_handler_returns_false() {
        assert!(!unsafe { handle_page_fault(0x1000, PF_WRITE) });
    }

    #[test]
    fn test_alloc_pt_page_fails_without_init() {
        // Without VM init, alloc_pt_page should fail
        unsafe {
            let r = alloc_pt_page();
            assert!(r.is_err());
        }
    }

    #[test]
    fn test_types_are_send() {
        fn _assert_send<T: Send>() {}
        _assert_send::<PageTableError>();
    }
}
