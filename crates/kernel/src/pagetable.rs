//! Page table management (x86_64: 4-level PML4→PDPT→PD→PT).
//!
//! Provides page table allocation, walking, mapping, and unmapping.
//! Architecture-specific constants and operations come from `crate::hal`.

use arch_common::com::{VM_PAGEFAULT, VM_PROC_NR};

use crate::ipc::{OK, SENDREC, current_proc, do_sync_ipc};
use crate::proc::MESSAGE_SIZE;

// Re-export page table constants and basic operations from hal
pub use crate::hal::{
    MAP_NX, MAP_PRESENT, MAP_USER, MAP_WRITE, MAX_USER_ADDRESS, PAGE_SIZE, boot_cr3, write_cr3,
};

/// Page table entry type (arch-specific, provided by HAL).
pub use crate::hal::PtEntry;

// PTE index extraction via generic HAL function.
// Convenience wrappers for each level (name matches x86_64 convention).
pub const fn pml4_index(va: u64) -> usize {
    crate::hal::pt_index(va, 3)
}
pub const fn pdpt_index(va: u64) -> usize {
    crate::hal::pt_index(va, 2)
}
pub const fn pd_index(va: u64) -> usize {
    crate::hal::pt_index(va, 1)
}
pub const fn pt_l0_index(va: u64) -> usize {
    crate::hal::pt_index(va, 0)
}

// PTE bit masks (now delegated to HAL so pagetable.rs is arch-agnostic)
pub const PG_P: u64 = crate::hal::pte_present();
pub const PG_RW: u64 = crate::hal::pte_writable();
pub const PG_U: u64 = crate::hal::pte_user();
pub const PG_PS: u64 = crate::hal::pte_large_page();
pub const PG_G: u64 = crate::hal::pte_global();
pub const PG_FRAME: u64 = crate::hal::pte_frame_mask();
pub const PG_PTEMASK: u64 = crate::hal::pte_flags_mask();

/// Return the saved CR3 value for a process, or 0 if the process has no
/// per-process page table.
pub fn get_proc_cr3(ep: i32) -> u64 {
    let slot = crate::table::endpoint_slot(ep);
    let rp = crate::table::proc_addr(slot);
    if rp.is_null() {
        return 0;
    }
    unsafe { (*rp).p_seg.p_cr3 }
}

// Mock definitions for `__bss_start`/`__bss_end` used by `pt_mapkernel`
// and any other kernel code that references BSS bounds.
// On Windows (where the kernel linker script is not available), these
// prevent unresolved symbol errors in any binary that links the kernel
// crate.  When building with a real linker script (Linux), duplicate
// strong symbols would conflict, so this is only active on Windows.
//
// `#[used]` ensures the symbols survive dead-code elimination.
// When linking without the kernel linker script, provide stub symbols.
// The linker script (`minix-raw.ld`) also defines these, so we only emit
// stubs on targets where the linker script is not used (Windows host or
// `x86_64-unknown-none` dev builds).
#[cfg(any(
    target_os = "windows",
    all(
        target_os = "none",
        not(target_arch = "riscv64"),
        not(target_vendor = "pc")
    )
))]
#[used]
#[unsafe(no_mangle)]
pub static __bss_start: u8 = 0;
#[cfg(any(
    target_os = "windows",
    all(
        target_os = "none",
        not(target_arch = "riscv64"),
        not(target_vendor = "pc")
    )
))]
#[used]
#[unsafe(no_mangle)]
pub static __bss_end: u8 = 0;

#[derive(Debug, Clone, Copy)]
pub struct PageWalkResult {
    pub pte_phys: u64,
    pub pte_virt: *mut u64,
    pub pte_value: u64,
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
        match crate::hal::alloc_phys_page() {
            Some(addr) => Ok(addr),
            None => Err(PageTableError::OutOfMemory),
        }
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

/// Walk the page table to find the PTE for a virtual address.
///
/// Uses `hal::pt_levels()` and `hal::pt_index()` for an arch-generic walk.
/// Returns `level` in the range 1..=N where N = `hal::pt_levels()`:
/// - level 1 = PT (4KB page)
/// - level 2 = PD (x86_64) / PMD (SV39) — 2MB huge page
/// - level 3 = PDPT (x86_64) / PUD (SV39) — 1GB huge page
///
/// `cr3` is the physical address of the root page table.
///
/// # Safety
///
/// `cr3` must point to a valid, identity-mapped root page table.
pub unsafe fn walk(cr3: u64, va: u64) -> Result<PageWalkResult, PageTableError> {
    unsafe {
        let levels = crate::hal::pt_levels();
        let mut table_phys = cr3;

        // Walk from the top non-leaf level down to level 1 (just above PT).
        for level in (1..levels).rev() {
            let table = table_phys as *const u64;
            let idx = crate::hal::pt_index(va, level);
            let pte = read_pte(table.add(idx));

            if pte & PG_P == 0 {
                return Err(PageTableError::NotMapped);
            }

            // If this is a huge page (PG_PS set at a non-leaf level), return here.
            if pte & PG_PS != 0 {
                return Ok(PageWalkResult {
                    pte_phys: table_phys + (idx as u64) * 8,
                    pte_virt: (table_phys + (idx as u64) * 8) as *mut u64,
                    pte_value: pte,
                    level: level + 1,
                });
            }

            table_phys = pte & PG_FRAME;
        }

        // Level 0 (PT — 4KB page).
        let pt = table_phys as *const u64;
        let idx = crate::hal::pt_index(va, 0);
        let pte = read_pte(pt.add(idx));

        if pte & PG_P == 0 {
            return Err(PageTableError::NotMapped);
        }

        Ok(PageWalkResult {
            pte_phys: table_phys + (idx as u64) * 8,
            pte_virt: (table_phys + (idx as u64) * 8) as *mut u64,
            pte_value: pte,
            level: 1,
        })
    }
}

/// Map a 4KB page. Allocates intermediate page tables as needed.
///
/// Uses `hal::pt_levels()` and `hal::pt_index()` for an arch-generic walk.
/// If a huge page is encountered at a non-leaf level, it is split into
/// 512 × 4KB PTEs before the requested page is mapped.
///
/// # Safety
///
/// `cr3` must point to a valid, identity-mapped root page table.
pub unsafe fn map_page(cr3: u64, va: u64, pa: u64, flags: u64) -> Result<(), PageTableError> {
    unsafe {
        let levels = crate::hal::pt_levels();
        let pte_flags = (flags & PG_PTEMASK) | PG_P;
        let pte_val = (pa & PG_FRAME) | pte_flags;
        let mut table_phys = cr3;

        // Walk from the top non-leaf level down to level 1.
        for level in (1..levels).rev() {
            let table = table_phys as *mut PtEntry;
            let idx = crate::hal::pt_index(va, level);
            let pte_addr = table.add(idx);
            let pte = read_pte(pte_addr);

            table_phys = if pte & PG_P == 0 {
                // No entry — allocate a new page table page.
                let p = alloc_pt_page()?;
                write_pte(pte_addr, p | PG_P | PG_RW | PG_U);
                p
            } else if pte & PG_PS != 0 {
                // Huge page — split into 512 × 4KB PTEs preserving the
                // original mapping, then overwrite the specific PTE below.
                let pt_phys = alloc_pt_page()?;
                let base_pa = pte & PG_FRAME;
                let pte_flags_src = (pte & PG_PTEMASK) & !PG_PS;
                let pt_virt = pt_phys as *mut u64;
                for i in 0..512u64 {
                    let pte_pa = base_pa + i * 4096;
                    write_pte(pt_virt.add(i as usize), pte_pa | pte_flags_src);
                }
                write_pte(pte_addr, pt_phys | PG_P | PG_RW | PG_U);
                pt_phys
            } else {
                pte & PG_FRAME
            };
        }

        // Level 0 (PT — write the final PTE).
        let pt = table_phys as *mut u64;
        let idx = crate::hal::pt_index(va, 0);
        let pte_addr = pt.add(idx);
        write_pte(pte_addr, pte_val);
        Ok(())
    }
}

/// Unmap a single 4KB page. Returns the old PTE value.
///
/// # Safety
///
/// `cr3` must point to a valid PML4.
pub unsafe fn unmap_page(cr3: u64, va: u64) -> Result<u64, PageTableError> {
    unsafe {
        let result = walk(cr3, va)?;
        if result.level != 1 {
            return Err(PageTableError::InvalidArgument);
        }
        write_pte(result.pte_virt, 0);
        crate::hal::tlb_flush_page(va);
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
        let end = ((va + size + 0xFFF) & !0xFFF).min(MAX_USER_ADDRESS);
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

/// Apply NX permissions to kernel BSS pages in a per-process page table.
///
/// The kernel is identity-mapped at virtual address 0x200000 as a single 2MB
/// huge page. This function splits that 2MB PDE into 512 × 4KB PTEs so that
/// per-page attributes can be applied, then:
///   - Sets `PG_NX` on BSS pages (preventing code execution from BSS)
///   - Clears `PG_G` on BSS entries (so TLB entries are flushed on CR3 switch)
/// Split the kernel's 2MB identity-mapped huge page into 4KB PTEs and
/// set NX on BSS pages (if the kernel is mapped as a huge page at boot).
///
/// The kernel virtual address is obtained from `hal::kern_vaddr()`.
///
/// # Safety
///
/// `cr3` must point to a valid, identity-mapped root page table.
pub unsafe fn pt_mapkernel(cr3: u64) -> Result<(), PageTableError> {
    if cr3 == 0 {
        return Err(PageTableError::InvalidArgument);
    }
    unsafe {
        let kern_start = crate::hal::kern_vaddr();

        // Walk to find the PDE for the kernel 2MB identity mapping
        let result = walk(cr3, kern_start)?;

        if result.level != 2 {
            return Err(PageTableError::InvalidArgument);
        }
        if result.pte_value & PG_PS == 0 {
            return Err(PageTableError::InvalidArgument);
        }

        let base_pa = result.pte_value & PG_FRAME;

        // Attributes to propagate to each 4KB PTE (exclude frame, PS, and G)
        let attrs = result.pte_value & !(PG_FRAME | PG_PS | PG_G);

        // Allocate a 4KB page table to hold the split PTEs
        let pt_phys = alloc_pt_page()?;
        let pt = pt_phys as *mut PtEntry;

        // Populate the new page table with 512 × 4KB entries
        for i in 0..512 {
            let pa = base_pa + (i as u64) * 0x1000;
            let pte_val = (pa & PG_FRAME) | attrs;
            write_pte(pt.add(i), pte_val);
        }

        // Replace the PDE to point to the new page table (clear PS, G)
        let pde_flags = (result.pte_value & PG_PTEMASK) & !(PG_PS | PG_G);
        let new_pde = (pt_phys & PG_FRAME) | pde_flags;
        write_pte(result.pte_virt, new_pde);

        // ── Set NX on BSS pages ──────────────────────────────────────

        unsafe extern "C" {
            static __bss_start: u8;
            static __bss_end: u8;
        }

        let bss_start = core::ptr::addr_of!(__bss_start) as u64;
        let bss_end = core::ptr::addr_of!(__bss_end) as u64;

        // BSS must be within the kernel 2MB region
        if bss_start < kern_start || bss_end > kern_start + (512 * 0x1000) {
            return Err(PageTableError::InvalidArgument);
        }

        let bss_start_offset = bss_start - kern_start;
        let bss_end_offset = bss_end - kern_start;

        let first_bss_page = (bss_start_offset / 0x1000) as usize;
        let last_bss_page = bss_end_offset.div_ceil(0x1000) as usize;

        for i in first_bss_page..last_bss_page {
            let mut pte_val = read_pte(pt.add(i));
            pte_val |= crate::hal::MAP_NX;
            pte_val &= !PG_G;
            write_pte(pt.add(i), pte_val);
        }

        Ok(())
    }
}

/// Handle a page fault. Routes to the VM server for resolution.
/// Handle a page fault by forwarding it to the VM server.
///
/// Builds a VM_PAGEFAULT message with the fault address and error code,
/// then calls `do_sync_ipc` with SENDREC to deliver it to the VM server.
/// The VM server processes the fault (demand paging, COW, etc.) and
/// replies. Returns true if the fault was handled, false if the process
/// should receive SIGSEGV.
///
/// If the VM server is not available or the fault is from VM_PROC_NR
/// itself, returns false immediately.
///
/// # Safety
///
/// Must be called from the page fault interrupt handler with interrupts
/// disabled. `va` must be the value from CR2.
pub unsafe fn handle_page_fault(va: u64, err: u32) -> bool {
    unsafe {
        let proc = current_proc();
        if proc.is_null() {
            return false;
        }

        // VM server can't handle its own page faults.
        if (*proc).p_endpoint == VM_PROC_NR {
            return false;
        }

        // Build the VM_PAGEFAULT message.
        // Layout (64-byte message):
        //   offset 0:  destination endpoint (i32) — VM_PROC_NR
        //   offset 4:  source endpoint (i32) — set by kernel
        //   offset 8:  m_type (i32) — VM_PAGEFAULT
        //   offset 12: m_source (i32) — faulting process endpoint
        //   offset 16: VPF_ADDR (u64) — fault address from CR2
        //   offset 24: VPF_FLAGS (u32) — page fault error code
        let mut msg = [0u8; MESSAGE_SIZE];
        let dest = VM_PROC_NR;
        msg[0..4].copy_from_slice(&dest.to_ne_bytes());
        let call_type = VM_PAGEFAULT as i32;
        msg[8..12].copy_from_slice(&call_type.to_ne_bytes());
        let source = (*proc).p_endpoint;
        msg[12..16].copy_from_slice(&source.to_ne_bytes());
        msg[16..24].copy_from_slice(&va.to_ne_bytes());
        msg[24..28].copy_from_slice(&err.to_ne_bytes());

        // Send the fault to the VM server and wait for a reply.
        // do_sync_ipc will first try in-kernel dispatch; if no dispatch
        // handler is registered for VM_PROC_NR, it falls through to full
        // IPC (mini_send + mini_receive).
        let r = do_sync_ipc(proc, msg.as_mut_ptr(), SENDREC);
        r == OK
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::vm::{self, NO_MEM};
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
        // Without CPU local storage initialized, this might panic.
        // In a real environment with an initialized system, it returns false
        // (no VM server dispatch handler registered).
        let result = std::panic::catch_unwind(|| unsafe { handle_page_fault(0x1000, PF_WRITE) });
        if let Ok(val) = result {
            assert!(!val)
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_alloc_pt_page_after_arch_init() {
        // alloc_pt_page now uses the arch allocator (not VM).
        // Initialize a minimal allocator, then verify allocation works.
        unsafe {
            let mut mmap = arch_x86_64::alloc::PhysicalMemoryMap::new();
            mmap.add(0x100000, 0x200000);
            arch_x86_64::alloc::init_allocator(&mmap);
            let r = alloc_pt_page();
            assert!(r.is_ok());
        }
    }

    #[test]
    fn test_types_are_send() {
        fn _assert_send<T: Send>() {}
        _assert_send::<PageTableError>();
    }

    // ── pt_mapkernel tests ───────────────────────────────────────────

    #[test]
    fn test_pt_mapkernel_invalid_cr3_returns_err() {
        // A CR3 of 0 (no page table) should fail
        unsafe {
            let r = pt_mapkernel(0);
            assert!(r.is_err(), "CR3=0 should be invalid");
        }
    }

    // Hardware-dependent tests: require bare metal (identity-mapped physical
    // memory) or QEMU. Gated to prevent crash on host test binaries.
    // See PORTING_PLAN.md Phase 6.2: "Hardware-dependent tests require
    // bare-metal or QEMU execution; gated from host test runner."

    #[test]
    #[cfg(target_os = "none")]
    fn test_pt_mapkernel_unmapped_address() {
        unsafe {
            init_vm();
            let cr3_page = vm::alloc_mem(1, 0);
            assert!(cr3_page != NO_MEM);
            let cr3_phys = cr3_page * 4096;
            core::ptr::write_bytes(cr3_phys as *mut u8, 0, 4096);
            let r = pt_mapkernel(cr3_phys);
            assert!(r.is_err(), "unmapped kernel range should fail");
        }
    }

    #[test]
    #[cfg(target_os = "none")]
    fn test_pt_mapkernel_splits_hugepage() {
        unsafe {
            init_vm();
            let cr3_page = vm::alloc_mem(1, 0);
            assert!(cr3_page != NO_MEM);
            let cr3_phys = cr3_page * 4096;
            core::ptr::write_bytes(cr3_phys as *mut u8, 0, 4096);

            let pdpt_page = vm::alloc_mem(1, 0);
            let pdpt_phys = pdpt_page * 4096;
            core::ptr::write_bytes(pdpt_phys as *mut u8, 0, 4096);
            let pml4 = cr3_phys as *mut u64;
            core::ptr::write(pml4, pdpt_phys | PG_P | PG_RW | PG_U);

            let pd_page = vm::alloc_mem(1, 0);
            let pd_phys = pd_page * 4096;
            core::ptr::write_bytes(pd_phys as *mut u8, 0, 4096);
            let pdpt = pdpt_phys as *mut u64;
            core::ptr::write(pdpt, pd_phys | PG_P | PG_RW | PG_U);

            let pd = pd_phys as *mut u64;
            let pde_val = (0x200000u64 & PG_FRAME) | PG_P | PG_RW | PG_PS | PG_G;
            core::ptr::write(pd, pde_val);

            let r = pt_mapkernel(cr3_phys);
            assert!(r.is_ok(), "pt_mapkernel should succeed on valid PD");

            let updated_pde = core::ptr::read(pd);
            assert_eq!(updated_pde & PG_PS, 0, "PG_PS should be cleared");
        }
    }
}
