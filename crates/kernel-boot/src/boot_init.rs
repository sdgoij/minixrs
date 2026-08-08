//! Boot-time user process initialization — loads init from initramfs
//! and starts it as the first userspace process.
//!
//! Called from kmain after all kernel init is complete.

use kernel::elf::{Elf64Phdr, ElfError, LoadedElf, parse_elf_header, setup_user_stack};
use kernel::initramfs::find_initramfs_file;
use kernel::pagetable::{boot_cr3, map_page};

use crate::print;

/// Convenience alias for the Proc type.
use kernel::proc::Proc;

/// Return type for `load_and_prepare_init`, exposing the loaded ELF bounds
/// so the caller can create a per-process page table covering all pages.
pub struct InitInfo {
    /// Pointer to init's kernel `Proc` entry.
    pub proc_ptr: *mut Proc,
    /// Virtual address: page-aligned start of ELF LOAD segments.
    pub code_start: u64,
    /// Virtual address: page-aligned end (exclusive) of ELF LOAD segments.
    pub code_end: u64,
    /// Physical address of the allocated code pages.
    pub phys_code_base: u64,
    /// Virtual address: page-aligned start of the user stack.
    pub stack_start: u64,
    /// Virtual address: page-aligned end (exclusive) of the user stack.
    pub stack_end: u64,
    /// Physical address of the allocated stack pages.
    pub phys_stack_base: u64,
}

/// Load a binary from initramfs and set up its TrapFrame for sysretq ring-3.
///
/// Allocates unique physical pages for each process's code and stack,
/// so per-process page tables can map virtual→physical independently.
///
/// # Safety
///
/// Must be called after kernel::init() before any user code runs.
/// Single-threaded boot context.
/// `path` must exist in the initramfs. `proc_nr` must be a valid process
/// number with an initialized Proc entry. The VM allocator must be initialized.
pub unsafe fn load_and_prepare_proc(path: &str, proc_nr: i32, argv: &[&str]) -> Option<InitInfo> {
    let (data, _mode) = find_initramfs_file(path)?;
    let ehdr = match parse_elf_header(data) {
        Ok(ehdr) => ehdr,
        Err(_) => {
            print!("  ");
            print!(path);
            print!(": invalid ELF header\r\n");
            return None;
        }
    };
    print!("  ");
    print!(path);
    print!(": ELF64 entry=0x");
    print_hex(ehdr.e_entry);
    print!("\r\n");

    // Step 1: Calculate ELF bounds and page count without loading yet.
    let loaded = match unsafe { calc_elf_bounds(data) } {
        Ok(l) => l,
        Err(_) => {
            print!("  ");
            print!(path);
            print!(": invalid ELF\r\n");
            return None;
        }
    };
    let code_start = loaded.base & !0xFFF;
    let code_end = (loaded.top + 0xFFF) & !0xFFF;
    let code_pages = ((code_end - code_start) / 4096) as usize;

    // Step 2: Allocate contiguous physical pages for code.
    // Use the contiguous physical allocator (bottom-up) to avoid
    // conflicts with page table allocations (which use top-down).
    let phys_code_base = match unsafe { kernel::hal::alloc_phys_contig(code_pages) } {
        Some(base) => base,
        None => {
            print!("  ");
            print!(path);
            print!(": out of memory for code\r\n");
            return None;
        }
    };

    // Step 3: Load ELF data into the allocated physical pages.
    // The identity mapping covers all of 0..1GB, so writing to
    // phys_code_base + (vaddr - code_start) goes to the right pages.
    if unsafe { load_elf_at(data, phys_code_base, loaded.base) }.is_err() {
        print!("  ");
        print!(path);
        print!(": ELF load failed\r\n");
        return None;
    }

    // AArch64: clean D-cache + invalidate I-cache for loaded code.
    // The identity-mapped PA used for loading differs from the runtime
    // VA (0x1000000+). Without I-cache invalidation, VIPT aliasing
    // causes the CPU to fetch stale instructions.
    #[cfg(target_arch = "aarch64")]
    {
        let mut addr = phys_code_base;
        let end = phys_code_base + (code_end - code_start);
        let ctr_el0: u64;
        unsafe {
            core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr_el0, options(nomem, nostack));
        }
        let dcache_line_shift = ((ctr_el0 >> 16) & 0xF) + 2;
        let line_size = 4u64 << dcache_line_shift;
        while addr < end {
            unsafe {
                core::arch::asm!("dc cvau, {va}", va = in(reg) addr, options(nostack));
                core::arch::asm!("ic ivau, {va}", va = in(reg) addr, options(nostack));
            }
            addr += line_size;
        }
        unsafe {
            core::arch::asm!("dsb ish", options(nostack));
            core::arch::asm!("isb", options(nostack));
        }
    }

    // Step 4: Allocate physical pages for user stack.
    let user_stack_base: u64 = kernel::hal::user_stack_base();
    let user_stack_size: usize = kernel::hal::user_stack_size();
    let stack_pages = user_stack_size / 4096;
    let phys_stack_base = match unsafe { kernel::hal::alloc_phys_contig(stack_pages) } {
        Some(base) => base,
        None => {
            print!("  ");
            print!(path);
            print!(": out of memory for stack\r\n");
            return None;
        }
    };

    let mut user_rsp;
    #[cfg(not(target_arch = "aarch64"))]
    {
        user_rsp = 0;
        let _ = user_rsp;

        let stack_top = user_stack_base + user_stack_size as u64;
        user_rsp = match unsafe { setup_user_stack(stack_top, user_stack_size, argv) } {
            Ok(rsp) => rsp,
            Err(_) => {
                print!("  ");
                print!(path);
                print!(": stack setup failed\r\n");
                return None;
            }
        };
        // Copy identity-mapped stack data to the allocated physical pages.
        unsafe {
            core::ptr::copy_nonoverlapping(
                user_stack_base as *const u8,
                phys_stack_base as *mut u8,
                user_stack_size,
            );
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // Write stack setup directly to the physical pages.
        // phys_stack_base is in the identity-mapped range (PUD[1]).
        let stack_top_virt = phys_stack_base + user_stack_size as u64;
        user_rsp = match unsafe { setup_user_stack(stack_top_virt, user_stack_size, argv) } {
            Ok(rsp) => rsp,
            Err(_) => {
                print!("  ");
                print!(path);
                print!(": stack setup failed\r\n");
                return None;
            }
        };
        // Convert physical RSP to virtual RSP for the user process.
        let phys_rsp = user_rsp;
        user_rsp = user_stack_base + (phys_rsp - phys_stack_base);
    }

    // Step 6: Store the physical code base in the new TrapFrame.
    // Note: rsp is the VIRTUAL stack pointer; the per-process page table
    // maps the virtual stack address to phys_stack_base.
    let rp = kernel::table::proc_addr(proc_nr);
    unsafe {
        kernel::hal::set_initial_regs(&mut (*rp).p_reg, ehdr.e_entry, user_rsp, user_rsp);
    }

    let stack_start = user_stack_base & !0xFFF;
    let stack_end = (user_stack_base + user_stack_size as u64 + 0xFFF) & !0xFFF;

    print!("  ");
    print!(path);
    print!(": loaded phys=0x");
    print_hex(phys_code_base);
    print!(" stack=0x");
    print_hex(user_rsp);
    print!("\n");

    Some(InitInfo {
        proc_ptr: rp,
        code_start,
        code_end,
        phys_code_base,
        stack_start,
        stack_end,
        phys_stack_base,
    })
}

/// Calculate the bounds (base vaddr, top vaddr, entry) of an ELF binary
/// without copying data to memory.
unsafe fn calc_elf_bounds(data: &[u8]) -> Result<LoadedElf, ElfError> {
    let ehdr = parse_elf_header(data)?;

    if ehdr.e_phoff == 0
        || ehdr.e_phnum == 0
        || ehdr.e_phentsize as usize != core::mem::size_of::<Elf64Phdr>()
    {
        return Err(ElfError::NoLoadSegments);
    }

    let phoff = ehdr.e_phoff as usize;
    let phnum = ehdr.e_phnum as usize;
    let phentsize = ehdr.e_phentsize as usize;

    let mut base = u64::MAX;
    let mut top = 0u64;
    let mut found_load = false;

    for i in 0..phnum {
        let phdr = unsafe { &*(data.as_ptr().add(phoff + i * phentsize) as *const Elf64Phdr) };

        if phdr.p_type != 1 {
            continue;
        }
        found_load = true;

        let file_end = phdr
            .p_offset
            .checked_add(phdr.p_filesz)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        if file_end > data.len() as u64 {
            return Err(ElfError::SegmentOutOfBounds);
        }

        if phdr.p_vaddr < base {
            base = phdr.p_vaddr;
        }
        let seg_top = phdr
            .p_vaddr
            .checked_add(phdr.p_memsz)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        if seg_top > top {
            top = seg_top;
        }
    }

    if !found_load {
        return Err(ElfError::NoLoadSegments);
    }

    Ok(LoadedElf {
        base,
        top,
        entry: ehdr.e_entry,
    })
}

/// Load ELF segment data into memory at `phys_base`, offset by the
/// difference between each segment's vaddr and the ELF's base vaddr.
///
/// Writes through the identity mapping (virtual == physical for 0..1GB).
unsafe fn load_elf_at(data: &[u8], phys_base: u64, elf_base_vaddr: u64) -> Result<(), ElfError> {
    let ehdr = parse_elf_header(data)?;

    let phoff = ehdr.e_phoff as usize;
    let phnum = ehdr.e_phnum as usize;
    let phentsize = ehdr.e_phentsize as usize;

    for i in 0..phnum {
        let phdr = unsafe { &*(data.as_ptr().add(phoff + i * phentsize) as *const Elf64Phdr) };

        if phdr.p_type != 1 {
            continue;
        }

        let file_end = phdr
            .p_offset
            .checked_add(phdr.p_filesz)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        if file_end > data.len() as u64 {
            return Err(ElfError::SegmentOutOfBounds);
        }

        // Destination = phys_base + (segment_vaddr - elf_base_vaddr)
        let offset = phdr.p_vaddr.wrapping_sub(elf_base_vaddr);
        let dst_addr = phys_base.wrapping_add(offset);
        let dst = dst_addr as *mut u8;

        if phdr.p_filesz > 0 {
            let src = unsafe { data.as_ptr().add(phdr.p_offset as usize) };
            unsafe {
                core::ptr::copy_nonoverlapping(src, dst, phdr.p_filesz as usize);
            }
        }

        let bss_size = phdr.p_memsz.saturating_sub(phdr.p_filesz);
        if bss_size > 0 {
            let bss_dst = unsafe { dst.add(phdr.p_filesz as usize) };
            unsafe {
                core::ptr::write_bytes(bss_dst, 0, bss_size as usize);
            }
        }
    }

    Ok(())
}

/// Load /sbin/init from the embedded initramfs.
///
/// # Safety
///
/// Must be called after kernel::init(). Single-threaded boot context.
pub unsafe fn load_and_prepare_init() -> Option<InitInfo> {
    unsafe {
        load_and_prepare_proc(
            "/sbin/init",
            arch_common::com::INIT_PROC_NR,
            &["/sbin/init"],
        )
    }
}

/// Create a per-process page table for the init process.
///
/// Allocates a new PML4 → PDP → PD hierarchy, deep-copies the boot identity
/// map, and shares kernel high mappings. Returns the physical address of
/// the new PML4 (the CR3 value).
///
/// Uses the arch physical allocator (already initialized by the caller).
///
/// # Safety
///
/// Must be called after the arch allocator is initialized and with CR3
/// pointing to the boot page table.
pub unsafe fn boot_create_page_table() -> u64 {
    let boot_cr3_val = boot_cr3();
    if boot_cr3_val == 0 {
        return 0;
    }
    let levels = kernel::hal::pt_levels();
    let page_sz = kernel::hal::PAGE_SIZE as usize;

    // Walk the boot page table to find the bottom-level PD.
    let mut table_phys = boot_cr3_val;
    for lvl in (2..levels).rev() {
        let table = table_phys as *const u64;
        let idx = kernel::hal::pt_index(0, lvl);
        let entry = unsafe { core::ptr::read(table.add(idx)) };
        table_phys = kernel::hal::pte_to_phys(entry);
    }
    let boot_pd_phys = table_phys;

    // Allocate (levels-1) pages: root + intermediate + PD.
    let n_pages = (levels - 1) as usize;
    let mut pages = [0u64; 4];
    for entry in pages.iter_mut().take(n_pages) {
        *entry = match unsafe { kernel::hal::alloc_phys_page() } {
            Some(p) => p,
            None => return 0,
        };
        unsafe { core::ptr::write_bytes(*entry as *mut u8, 0, page_sz) };
    }

    // Link hierarchy: root[0] → next[0] → ... → PD.
    // On RISC-V SV39: non-leaf (branching) PTEs must have V=1 and R=W=X=0.
    // On x86_64: non-leaf entries can have R/W and U/S bits.
    // Include PTE_A|PTE_D to avoid hardware A/D bit update faults.
    #[cfg(target_arch = "x86_64")]
    let flags = kernel::hal::pte_present() | kernel::hal::pte_writable() | kernel::hal::pte_user();
    #[cfg(target_arch = "riscv64")]
    let flags = kernel::hal::pte_present() | 0xC0; // V | A | D
    #[cfg(target_arch = "aarch64")]
    let flags = kernel::hal::pte_present(); // PTE_TABLE for AArch64 non-leaf entries
    for i in 0..(n_pages - 1) {
        unsafe {
            let pte = kernel::hal::build_pte(pages[i + 1], flags);
            core::ptr::write(pages[i] as *mut u64, pte);
        }
    }

    // Deep-copy all 512 boot PD entries into new PD.
    unsafe {
        let new_pd = pages[n_pages - 1] as *mut u64;
        for i in 0..512 {
            let entry = core::ptr::read((boot_pd_phys as *const u64).add(i));
            core::ptr::write(new_pd.add(i), entry);
        }

        // Share kernel high mappings (top half of root).
        let boot_root = boot_cr3_val as *const u64;
        let new_root = pages[0] as *mut u64;
        for i in 256..512 {
            let entry = core::ptr::read(boot_root.add(i));
            core::ptr::write(new_root.add(i), entry);
        }
    }

    pages[0]
}

/// Create a restricted per-process page table that maps only the pages
/// needed by a specific process: its code segments, user stack, and the
/// shared kernel high mappings. No identity-mapped data from other
/// processes is accessible.
///
/// Uses 4KB page granularity for user mappings via `map_page()`.
///
/// # Safety
///
/// Must be called after the arch allocator and VM allocator are
/// initialized. The physical pages for `code_start..code_end` and
/// `stack_start..stack_end` must already be allocated and populated.
pub unsafe fn boot_create_restricted_page_table(
    code_start: u64,
    code_end: u64,
    code_phys: u64,
    stack_start: u64,
    stack_end: u64,
    stack_phys: u64,
    map_low_gb_dev_user: bool,
) -> Option<u64> {
    #[cfg(not(target_arch = "aarch64"))]
    let _ = map_low_gb_dev_user;
    let boot_cr3_val = boot_cr3();
    if boot_cr3_val == 0 {
        return None;
    }
    let levels = kernel::hal::pt_levels();
    let page_sz = kernel::hal::PAGE_SIZE as usize;

    // Walk the boot page table to find the bottom-level page directory (level 1).
    // On x86_64 with 4 levels, walks from PML4(3) down to PD(2), finding PD.
    // On RISC-V SV39 with 3 levels, walks from L2(2) down to L1(1)...
    // but our boot page table uses 1GB huge pages at L2 (leaf entries).
    // In that case, there is no L1-level table to copy from.
    let mut table_phys = boot_cr3_val;
    let mut found_boot_pd = false;
    for lvl in (2..levels).rev() {
        let table = table_phys as *const u64;
        let idx = kernel::hal::pt_index(0, lvl);
        let entry = unsafe { core::ptr::read(table.add(idx)) };
        // If the boot entry is a huge page leaf, there's no lower-level
        // table to deep-copy.
        // On x86_64: PG_PS bit (0x80) indicates 2MB or 1GB huge page.
        // On RISC-V SV39: PTE with V + any R/W/X at a non-leaf level is leaf.
        #[cfg(target_arch = "x86_64")]
        let is_leaf = (entry & kernel::hal::pte_present() != 0)
            && (entry & kernel::hal::pte_large_page()) != 0;
        #[cfg(target_arch = "riscv64")]
        let is_leaf = (entry & kernel::hal::pte_present() != 0) && (entry & 0x0E) != 0;
        // AArch64: block entries have bits[1:0]=01, table entries have 11.
        #[cfg(target_arch = "aarch64")]
        let is_leaf = (entry & 3) == 1;
        if is_leaf {
            found_boot_pd = false;
            break;
        }
        table_phys = kernel::hal::pte_to_phys(entry);
        found_boot_pd = true;
    }
    // If we walked all the way down, the last table is the boot PD.
    let boot_pd_phys = if found_boot_pd { table_phys } else { 0 };

    // Allocate (levels-1) pages: root + intermediate levels (PD is last).
    let n_pages = (levels - 1) as usize;
    let mut pages = [0u64; 4];
    for entry in pages.iter_mut().take(n_pages) {
        *entry = unsafe { kernel::hal::alloc_phys_page()? };
        #[cfg(not(target_arch = "aarch64"))]
        unsafe {
            core::ptr::write_bytes(*entry as *mut u8, 0, page_sz)
        };
        #[cfg(target_arch = "aarch64")]
        for i in 0..(page_sz / 8) {
            unsafe { core::ptr::write_volatile((*entry as *mut u64).add(i), 0) };
        }
    }

    // Link hierarchy: root[0] → next[0] → ... → PD.
    // On RISC-V SV39: non-leaf (branching) PTEs must have V=1 and R=W=X=0.
    // On x86_64: non-leaf entries can have R/W and U/S bits.
    #[cfg(target_arch = "x86_64")]
    let flags = kernel::hal::pte_present() | kernel::hal::pte_writable() | kernel::hal::pte_user();
    // Non-leaf (branching) PTEs on RISC-V SV39 must have V=1 and R=W=X=0.
    // A and D bits are WPRI (Write-Preserve-Read-Ignore) for non-leaf PTEs.
    // Some QEMU implementations reject A/D bits in non-leaf entries.
    #[cfg(target_arch = "riscv64")]
    let flags = kernel::hal::pte_present(); // V only (not A|D)
    // AArch64 non-leaf: PTE_TABLE (bits[1:0] = 0b11, no AP/AF/SH).
    #[cfg(target_arch = "aarch64")]
    let flags = kernel::hal::pte_present(); // PTE_TABLE for AArch64 non-leaf entries
    for i in 0..(n_pages - 1) {
        unsafe {
            let pte = kernel::hal::build_pte(pages[i + 1], flags);
            core::ptr::write(pages[i] as *mut u64, pte);
        }
    }

    if found_boot_pd && boot_pd_phys != 0 {
        // Deep-copy all 512 bottom-level entries from boot PD (identity map).
        // This applies when boot page table has a non-leaf PD-level table
        // (e.g., x86_64 boot with 2MB huge pages split into PT entries).
        unsafe {
            let new_pd = pages[n_pages - 1] as *mut u64;
            for i in 0..512 {
                let entry = core::ptr::read((boot_pd_phys as *const u64).add(i));
                core::ptr::write(new_pd.add(i), entry);
            }

            // Remove PG_U from kernel PD entries to prevent user-space access to
            // kernel code/data.  Matches C pg_mapkernel() which maps kernel virtual
            // addresses without I386_VM_USER, while keeping the identity map entries
            // separate.  The kernel binary is at 0x200000, so the first relevant PD
            // index is 1 (each 2MB entry covers 0x200000 bytes).
            // Userspace split on these PD entries (via map_page for code/stack)
            // will re-add PG_U to the specific 4KB pages that belong to the process.
            // NOTE: PG_U is NOT cleared here because boot processes (PM, VFS, VM,
            // etc.) need user-mode access to kernel identity-mapped pages. The
            // original C MINIX has all boot processes share the kernel page table,
            // so kernel pages are user-accessible.
        }
    }

    // Share kernel mappings from the boot page table.
    // On x86_64: copy BOTH the lower identity-mapped half (indices 0-255)
    // and the high-half kernel mappings (indices 256-511). The lower half
    // is needed so kernel code at 0x200000+ is accessible from ring 0 when
    // the timer ISR or other interrupt handler fires while CR3 points to
    // this per-process page table. Without it, the CPU can't fetch handler
    // code from the IDT's identity-mapped entry address, causing a #PF.
    // On RISC-V SV39: the boot page table identity-maps the full 4GB at
    //   indices 0-3 (1GB huge pages), which covers both kernel code and
    //   device memory. We copy ALL entries so the kernel remains accessible.
    //   Index 0 (0x00000000-0x3FFFFFFF) covers UART, CLINT, PLIC MMIO
    //   regions needed by kernel trap handlers under per-process page tables.
    let boot_root = boot_cr3_val as *const u64;
    let new_root = pages[0] as *mut u64;
    #[cfg(target_arch = "x86_64")]
    let copy_range = 1..512;
    #[cfg(any(target_arch = "riscv64", target_arch = "aarch64"))]
    let copy_range = 0..512;
    for i in copy_range {
        let entry = unsafe { core::ptr::read(boot_root.add(i)) };
        if entry != 0 {
            #[cfg(target_arch = "x86_64")]
            let pte = entry | kernel::pagetable::PG_U;
            #[cfg(not(target_arch = "x86_64"))]
            let pte = entry;
            unsafe {
                core::ptr::write(new_root.add(i), pte);
            }
        }
    }

    // On AArch64, replace the shared boot PUD with a private copy.
    // The boot PUD is shared via PGD[0], but map_page will split
    // PUD[0] (breaking the boot identity map), and the subsequent
    // "Restore PUD[0]" code undoes the split, corrupting the
    // per-process page table. A private PUD copy isolates the
    // per-process page table from boot page table modifications.
    #[cfg(target_arch = "aarch64")]
    {
        let boot_pgd_entry = unsafe { core::ptr::read(boot_root) };
        if boot_pgd_entry != 0 {
            let boot_pud_phys = kernel::hal::pte_to_phys(boot_pgd_entry);
            let private_pud = unsafe { kernel::hal::alloc_phys_page()? };
            // Zero private PUD.
            for i in 0..(page_sz / 8) {
                unsafe { core::ptr::write_volatile((private_pud as *mut u64).add(i), 0) };
            }
            // Copy boot PUD entries as-is (PUD[0] only — PUD[1] replaced below).
            let entry0 = unsafe { core::ptr::read((boot_pud_phys as *const u64).add(0)) };
            unsafe { core::ptr::write((private_pud as *mut u64).add(0), entry0) };

            // Create a PMD page with 512 2MB block entries
            // for the kernel identity range (0x40000000-0x7FFFFFFF).
            // PMD entry 0 (0x40000000-0x401FFFFF) uses AP=EL1_only
            // because it contains the exception vector table — changing
            // AP to EL0_RW causes a prefetch abort on kernel exception
            // entry (QEMU Cortex-A57 quirk). All other entries (1..511)
            // use AP=EL0_RW so user-mode servers like VM can access
            // physical memory.
            let user_pmd = unsafe { kernel::hal::alloc_phys_page()? };
            const PMD_BLOCK_EL1: u64 = 0b01u64 | (0b11u64 << 8) | (1u64 << 10); // 0x701, AP=EL1 only
            const PMD_BLOCK_USER: u64 = 0b01u64 | (0b01u64 << 6) | (0b11u64 << 8) | (1u64 << 10); // 0x741, AP=EL0_RW
            let ram_base: u64 = 0x4000_0000;
            // Identity-map VA == PA across the *detected* RAM window: the
            // physical allocator's range is [base, base + total_pages*4096)
            // and RAM starts at 0x40000000, so its end is the RAM top. The
            // previous code hardcoded 256 MB and derived the PA from a
            // 0-based index (`ram_base + (index & 0xFFFFFFF)`), which wrapped
            // VAs above 0x50000000 onto the first 256 MB whenever a boot
            // process's page table was loaded — silently corrupting kernel
            // state (allocator bitmap, page tables, VM phys access) at 512MB+.
            let ram_end = kernel::hal::phys_alloc_base()
                + (kernel::hal::phys_alloc_total_pages() as u64) * 4096;
            for i in 0..512usize {
                // This PMD table sits under PUD[1], so entry i covers the
                // actual VA 0x40000000 + i*2MB.
                let va = ram_base + (i as u64) * 0x20_0000;
                let entry = if va < ram_end {
                    // PMD entry 0 contains the exception vector table;
                    // use EL1-only to avoid QEMU Cortex-A57 prefetch abort.
                    let flags = if i == 0 {
                        PMD_BLOCK_EL1
                    } else {
                        PMD_BLOCK_USER
                    };
                    va | flags
                } else {
                    0 // not RAM: leave unmapped (faults loudly, never aliases)
                };
                unsafe {
                    core::ptr::write_volatile((user_pmd as *mut u64).add(i), entry);
                }
            }

            // Set PUD[1] to point to the user PMD page.
            let pud1_flags = arch_aarch64::pte::PTE_VALID | arch_aarch64::pte::PTE_TYPE;
            let pud1_entry = kernel::hal::build_pte(user_pmd, pud1_flags);
            unsafe { core::ptr::write((private_pud as *mut u64).add(1), pud1_entry) };

            // For driver processes, replace PUD[0] with a private PMD that
            // maps the low 1GB identity: EL1-only blocks, except the
            // virtio-mmio window (0x0a000000 on QEMU virt) which is EL0_RW
            // so the driver can probe and drive the device from user mode.
            // Kept separate from the shared boot table (PUD[0] is copied
            // from it above); never split that shared block in place.
            if map_low_gb_dev_user {
                let pmd_low = unsafe { kernel::hal::alloc_phys_page()? };
                for i in 0..(page_sz / 8) {
                    unsafe { core::ptr::write_volatile((pmd_low as *mut u64).add(i), 0) };
                }
                const VIRTIO_MMIO_BASE: u64 = 0x0a00_0000;
                for i in 0..512usize {
                    let va = (i as u64) * 0x20_0000;
                    let flags = if va >= VIRTIO_MMIO_BASE && va < VIRTIO_MMIO_BASE + 0x20_0000 {
                        PMD_BLOCK_USER
                    } else {
                        PMD_BLOCK_EL1
                    };
                    unsafe {
                        core::ptr::write_volatile((pmd_low as *mut u64).add(i), va | flags);
                    }
                }
                let pud0_entry = kernel::hal::build_pte(pmd_low, pud1_flags);
                unsafe { core::ptr::write((private_pud as *mut u64).add(0), pud0_entry) };
            }

            // Replace PGD[0] with private PUD.
            let flags = kernel::hal::pte_nonleaf_flags();
            let new_pgd0 = kernel::hal::build_pte(private_pud, flags);
            unsafe { core::ptr::write(new_root, new_pgd0) };
        }
    }

    // Overwrite user code pages: map_page will split huge pages to 4KB.
    // On x86_64: PG_P | PG_RW | PG_U = readable+writable+user
    // On RISC-V: need V|R|W|X|U (RISC-V requires R for read, W for write, X for exec)
    #[cfg(target_arch = "x86_64")]
    let user_flags = kernel::pagetable::PG_P | kernel::pagetable::PG_RW | kernel::pagetable::PG_U;
    #[cfg(target_arch = "riscv64")]
    let user_flags = kernel::pagetable::PG_P
        | kernel::pagetable::PG_RW
        | kernel::pagetable::PG_U
        | 0x02
        | 0x04
        | 0x08
        | 0xC0; // R|W|X|A|D
    // AArch64: use the HAL-provided user flags.
    #[cfg(target_arch = "aarch64")]
    let user_flags = kernel::hal::pte_user_flags();
    let mut va = code_start;
    let mut pa = code_phys;
    while va < code_end {
        unsafe {
            if map_page(pages[0], va, pa, user_flags).is_err() {
                return None;
            }
        }
        va += 0x1000;
        pa += 0x1000;
    }

    // Overwrite user stack pages similarly.
    let mut va = stack_start;
    let mut pa = stack_phys;
    while va < stack_end {
        unsafe {
            if map_page(pages[0], va, pa, user_flags).is_err() {
                return None;
            }
        }
        va += 0x1000;
        pa += 0x1000;
    }

    // RISC-V: map the 2MB region below the user stack as user-accessible.
    // The per-process page table copies supervisor-only 1GB/2MB huge pages
    // from the boot page table. Only the specifically mapped code/stack pages
    // get the U (user) bit. A stack underflow of even a few KB lands in an
    // adjacent supervisor-only 2MB region and triggers an immediate page
    // fault. This guard maps the 2MB below the stack with the same user
    // permissions so modest stack underflows (up to 2 MB) survive.
    //
    // The physical pages in this range (0x8FC00000-0x8FDFFFFF for RISC-V
    // QEMU virt) are identity-mapped free RAM, well above any allocated
    // boot pages, so no conflict with kernel data.
    #[cfg(target_arch = "riscv64")]
    {
        let guard_start = stack_start.wrapping_sub(0x200000);
        let guard_end = stack_start;
        let mut gva = guard_start;
        while gva < guard_end {
            unsafe {
                // Identity-map: PA == VA (memory is free RAM)
                if map_page(pages[0], gva, gva, user_flags).is_err() {
                    return None;
                }
            }
            gva += 0x1000;
        }
    }

    // With cacheable PT walks (TCR_EL1.IRGN0=ORGN0=1), the walker
    // reads from cache where our writes are already visible.
    // No explicit cache maintenance is needed.

    Some(pages[0])
}

/// Walk the AArch64 page table tree and clean + invalidate D-cache
/// for every page table page.  Must be called after all map_page
/// modifications so the page table walker sees the final state.
#[cfg(target_arch = "aarch64")]
pub unsafe fn clean_page_table_cache_aarch64(root_pa: u64) {
    let pgd = root_pa as *const u64;
    unsafe { arch_aarch64::hal::dcache_clean_invalidate_page(root_pa) };

    for pgd_idx in 0..512 {
        let pgd_entry = unsafe { core::ptr::read(pgd.add(pgd_idx)) };
        if pgd_entry & 1 == 0 {
            continue;
        }
        if (pgd_entry & 3) == 1 {
            continue;
        }
        let pud_pa = pgd_entry & 0x0000_FFFF_FFFF_F000;
        unsafe { arch_aarch64::hal::dcache_clean_invalidate_page(pud_pa) };

        let pud = pud_pa as *const u64;
        for pud_idx in 0..512 {
            let pud_entry = unsafe { core::ptr::read(pud.add(pud_idx)) };
            if pud_entry & 1 == 0 {
                continue;
            }
            if (pud_entry & 3) == 1 {
                continue;
            }
            let pmd_pa = pud_entry & 0x0000_FFFF_FFFF_F000;
            unsafe { arch_aarch64::hal::dcache_clean_invalidate_page(pmd_pa) };

            let pmd = pmd_pa as *const u64;
            for pmd_idx in 0..512 {
                let pmd_entry = unsafe { core::ptr::read(pmd.add(pmd_idx)) };
                if pmd_entry & 1 == 0 {
                    continue;
                }
                if (pmd_entry & 3) == 1 {
                    continue;
                }
                let pte_pa = pmd_entry & 0x0000_FFFF_FFFF_F000;
                unsafe { arch_aarch64::hal::dcache_clean_invalidate_page(pte_pa) };
            }
        }
    }
}

/// Jump to userspace — the final step of boot.
///
/// Sets init's per-process CR3, then calls the assembly `sysretq_to_user`
/// which loads registers from the TrapFrame and executes `sysretq`.
///
/// x86_64-only: uses sysretq instruction.
///
/// # Safety
///
/// `init` must contain a valid Proc pointer and page table physical address.
/// Never returns.
#[cfg(target_arch = "x86_64")]
pub unsafe fn boot_jump_to_user(init: &InitInfo, pt_phys: u64) -> ! {
    // Read register values from the raw byte frame.
    // x86_64 offsets: rcx=16, r11=72, rsp=168
    let frame = unsafe { &(*init.proc_ptr).p_reg };
    let entry = unsafe { core::ptr::read_volatile(frame.as_ptr().add(16) as *const u64) };
    let rflags = unsafe { core::ptr::read_volatile(frame.as_ptr().add(72) as *const u64) };
    let stack = unsafe { core::ptr::read_volatile(frame.as_ptr().add(168) as *const u64) };

    print!("Jumping to ring-3: entry=0x");
    print_hex(entry);
    print!(" stack=0x");
    print_hex(stack);
    print!(" cr3=0x");
    print_hex(pt_phys);
    print!("\n");

    // Execute sysretq with register values loaded directly.
    unsafe {
        core::arch::asm!(
            "mov    rcx, {entry}",
            "mov    r11, {rflags}",
            "mov    rax, {cr3}",
            "mov    cr3, rax",
            "mov    rsp, {stack}",
            "sysretq",
            entry = in(reg) entry,
            rflags = in(reg) rflags,
            cr3 = in(reg) pt_phys,
            stack = in(reg) stack,
            options(noreturn),
        );
    }
}

// Serial output helpers

/// Print a 64-bit hex value to serial.
pub fn print_hex(val: u64) {
    let chars = b"0123456789abcdef";
    for i in (0..16).rev() {
        let nibble = ((val >> (i * 4)) & 0xF) as usize;
        crate::serial_putc(chars[nibble]);
    }
}

// Tests

#[cfg(test)]
mod tests {

    #[test]
    fn hex_nibble_table_is_correct() {
        let chars = b"0123456789abcdef";
        assert_eq!(chars.len(), 16);
        for i in 0..16u8 {
            let expected = if i < 10 { b'0' + i } else { b'a' + i - 10 };
            assert_eq!(
                chars[i as usize], expected,
                "nibble {} maps to '{}'",
                i, expected as char
            );
        }
    }

    #[test]
    fn hex_print_loop_extracts_nibbles_correctly() {
        let val: u64 = 0xDEADBEEFCAFEBABE;
        let expected: [u8; 16] = *b"deadbeefcafebabe";
        for (i, &exp) in expected.iter().enumerate() {
            let nibble = ((val >> ((15 - i) * 4)) & 0xF) as u8;
            let c = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            };
            assert_eq!(c, exp, "position {} mismatch", i);
        }
    }

    #[test]
    fn user_stack_constants_are_within_ram() {
        // Use arch-specific values from HAL
        let stack_base = kernel::hal::user_stack_base();
        let stack_size = kernel::hal::user_stack_size() as u64;
        let ram_top = kernel::hal::kern_vaddr() + 0x40000000; // assume 1 GB RAM

        let stack_end = stack_base + stack_size;
        assert!(
            stack_end < ram_top,
            "user stack end 0x{:x} exceeds RAM top 0x{:x}",
            stack_end,
            ram_top
        );
    }

    #[test]
    fn sysret_cs_ss_from_star_msr() {
        // SYSRETQ (64-bit) loads CS from STAR[47:32] + 16, SS from STAR[47:32] + 8.
        // SYSRET_CS = 0x0010 (GDT base for user segments)
        //   CS = 0x0010 + 16 = 0x0020 | 3 = 0x0023 (GDT index 4, RPL 3)
        //   SS = 0x0010 + 8  = 0x0018 | 3 = 0x001B (GDT index 3, RPL 3)
        // GDT layout:
        //   Index 0: null
        //   Index 1: kernel code (0x08)
        //   Index 2: kernel data (0x10)
        //   Index 3: user data (0x1B)
        //   Index 4: user code (0x23)
        let sysret_cs: u16 = 0x0023;
        let sysret_ss: u16 = 0x001B;
        assert_eq!(sysret_cs & 3, 3, "CS RPL must be 3 (user mode)");
        assert_eq!(sysret_ss & 3, 3, "SS RPL must be 3 (user mode)");
        assert_eq!(sysret_cs >> 3, 4, "CS GDT index must be 4");
        assert_eq!(sysret_ss >> 3, 3, "SS GDT index must be 3");
    }

    #[test]
    fn psl_userset_has_if_and_reserved_bits() {
        // PSL_USERSET = 0x0202: bit 9 (IF) = 1, bit 1 (reserved) = 1
        let psl: u64 = 0x0202;
        assert_ne!(psl & 0x0200, 0, "IF (bit 9) must be set");
        assert_ne!(psl & 0x0002, 0, "reserved bit 1 must be set");
    }

    #[test]
    fn init_stack_size_is_reasonable() {
        // 64 KB user stack (16 pages)
        assert_eq!(65536 % 4096, 0, "stack must be page-aligned");
        assert_eq!(65536 / 4096, 16, "stack must be exactly 16 pages");
    }
}
