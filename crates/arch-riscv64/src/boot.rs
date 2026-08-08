//! RISC-V64 early boot code — FDT parsing, allocator init, MMU enable.
//!
//! Called from the assembly _start entry point. Sets up the kernel's
//! basic environment so it can run as a proper S-mode process.

#![cfg(target_arch = "riscv64")]

/// FDT memory parsing lives in arch-common (shared with AArch64).
pub use arch_common::fdt::parse_fdt_memory;

/// Boot information parsed from FDT / platform knowledge.
pub struct BootInfo {
    /// Physical memory base address.
    pub mem_base: u64,
    /// Physical memory size in bytes.
    pub mem_size: u64,
    /// Kernel load address (physical).
    pub kernel_base: u64,
    /// Kernel size (approx, from link script).
    pub kernel_size: u64,
    /// Hart ID (from a0).
    pub hart_id: u64,
    /// DTB pointer (from a1).
    pub dtb_ptr: u64,
}

/// Read the SATP register.
pub fn read_satp() -> u64 {
    let satp: u64;
    unsafe {
        core::arch::asm!("csrr {satp}, satp", satp = out(reg) satp, options(nomem, nostack));
    }
    satp
}

/// Enable SV39 paging by writing the SATP register.
///
/// # Safety
///
/// `root_ppn` must point to a valid, page-aligned root page table.
/// The page table must identity-map the kernel's code region.
pub unsafe fn enable_mmu(root_ppn: u64) {
    // SV39 mode = 8 (bits 60-63), ASID = 0 (bits 44-59), PPN = root_ppn >> 12
    let satp = (8u64 << 60) | (root_ppn >> 12);
    unsafe {
        core::arch::asm!("csrw satp, {satp}", satp = in(reg) satp, options(nomem, nostack));
        // Flush TLB after enabling paging
        core::arch::asm!("sfence.vma", options(nomem, nostack));
    }
}

/// Initialize the physical memory allocator from boot info.
///
/// # Safety
///
/// Must be called once during early boot.
pub unsafe fn init_phys_allocator(info: &BootInfo) {
    let mem_end = info.mem_base + info.mem_size;
    let kernel_end = info.kernel_base + info.kernel_size;
    let alloc_start = kernel_end.max(info.mem_base);

    if alloc_start < mem_end {
        let mut mmap = crate::alloc::PhysicalMemoryMap::new();
        mmap.add(alloc_start, mem_end);
        // SAFETY: Called once during early boot with valid memory info
        unsafe {
            crate::alloc::init_allocator(&mmap);
        }
    }
}

/// Early initialization — called from _start assembly.
///
/// # Safety
///
/// Must be called in S-mode with a0=hart_id and a1=dtb_ptr.
/// Only the boot hart should proceed; other harts should park.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn early_init(hart_id: u64, dtb_ptr: u64) {
    // Only hart 0 proceeds for now (SMP later in Phase 19.x)
    if hart_id != 0 {
        loop {
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack));
            }
        }
    }

    // Parse FDT for memory information
    let boot_info =
        if let Some((mem_base, mem_size)) = unsafe { parse_fdt_memory(dtb_ptr as *const u8) } {
            BootInfo {
                mem_base,
                mem_size,
                kernel_base: 0x80200000, // QEMU virt loads kernel here
                kernel_size: 0x100000,   // 1MB (approximate, will be refined)
                hart_id,
                dtb_ptr,
            }
        } else {
            // Fallback: assume 128MB RAM starting at 0x80000000
            BootInfo {
                mem_base: 0x80000000,
                mem_size: 128 * 1024 * 1024,
                kernel_base: 0x80200000,
                kernel_size: 0x100000,
                hart_id,
                dtb_ptr,
            }
        };

    // Initialize physical allocator
    // SAFETY: Called once during early boot with valid boot info
    unsafe {
        init_phys_allocator(&boot_info);
    }

    // Set up STVEC to point to the trap vector
    let trap_vec = crate::trap_asm::trap_vector_addr();
    unsafe {
        core::arch::asm!("csrw stvec, {addr}", addr = in(reg) trap_vec, options(nomem, nostack));
    }

    // Print a message via SBI to confirm we're alive
    for &b in b"Hello MINIX!\r\n" {
        crate::sbi::console_putchar(b);
    }

    // For now, halt after initialization
    // TODO: Set up page tables, enable MMU, load processes, switch to user
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}
