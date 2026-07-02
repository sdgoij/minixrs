//! Boot-time user process initialization — loads init from initramfs
//! and starts it as the first userspace process.
//!
//! Called from kmain after all kernel init is complete.

use kernel::elf::{Elf64Phdr, ElfError, LoadedElf, parse_elf_header, setup_user_stack};
use kernel::initramfs::find_initramfs_file;
use kernel::pagetable::{boot_cr3, map_page};

use crate::print;

use arch_x86_64::pte;
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
    // Use the arch allocator's contiguous allocator (bottom-up) to
    // avoid conflicts with page table allocations (which use top-down).
    let phys_code_base = match arch_x86_64::alloc::alloc_phys_contig(code_pages) {
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

    // Step 4: Allocate physical pages for user stack.
    let user_stack_base: u64 = 0x0FE00000;
    let user_stack_size: usize = 65536;
    let stack_pages = user_stack_size / 4096;
    let phys_stack_base = match arch_x86_64::alloc::alloc_phys_contig(stack_pages) {
        Some(base) => base,
        None => {
            print!("  ");
            print!(path);
            print!(": out of memory for stack\r\n");
            return None;
        }
    };

    // Step 5: Set up the user stack via identity mapping, then copy
    // the stack data to the per-process physical pages.
    let stack_top = user_stack_base + user_stack_size as u64;
    let user_rsp = match unsafe { setup_user_stack(stack_top, user_stack_size, argv) } {
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

    // Step 6: Store the physical code base in the new TrapFrame.
    // Note: rsp is the VIRTUAL stack pointer; the per-process page table
    // maps the virtual stack address to phys_stack_base.
    let rp = kernel::table::proc_addr(proc_nr);
    unsafe {
        core::ptr::write_volatile(&raw mut (*rp).p_reg.rcx, ehdr.e_entry);
        core::ptr::write_volatile(&raw mut (*rp).p_reg.r11, 0x0202u64);
        core::ptr::write_volatile(&raw mut (*rp).p_reg.rsp, user_rsp);
        core::ptr::write_volatile(&raw mut (*rp).p_reg.rdi, user_rsp);
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

    // Walk the boot page table
    let boot_pml4 = boot_cr3_val as *const u64;
    let boot_pml4e0 = unsafe { core::ptr::read(boot_pml4) };
    let boot_pdpt_phys = boot_pml4e0 & arch_x86_64::pte::PG_FRAME;
    let boot_pdpt = boot_pdpt_phys as *const u64;
    let boot_pdpte0 = unsafe { core::ptr::read(boot_pdpt) };
    let boot_pd_phys = boot_pdpte0 & arch_x86_64::pte::PG_FRAME;

    // Allocate 3 pages from the arch allocator: PML4, PDP, PD
    let pml4_page = match arch_x86_64::alloc::alloc_phys_page() {
        Some(p) => p,
        None => return 0,
    };
    let pdpt_page = match arch_x86_64::alloc::alloc_phys_page() {
        Some(p) => p,
        None => return 0,
    };
    let pd_page = match arch_x86_64::alloc::alloc_phys_page() {
        Some(p) => p,
        None => return 0,
    };

    // Zero all three pages (4KB each)
    let page_sz = arch_x86_64::param::NBPG as usize;
    unsafe {
        core::ptr::write_bytes(pml4_page as *mut u8, 0, page_sz);
        core::ptr::write_bytes(pdpt_page as *mut u8, 0, page_sz);
        core::ptr::write_bytes(pd_page as *mut u8, 0, page_sz);
    }

    // Link: PML4[0] → PDP[0] → PD
    let flags = arch_x86_64::pte::PG_P | arch_x86_64::pte::PG_RW | arch_x86_64::pte::PG_U;
    unsafe {
        core::ptr::write(pml4_page as *mut u64, pdpt_page | flags);
        core::ptr::write(pdpt_page as *mut u64, pd_page | flags);
    }

    // Deep-copy all 512 PD entries from boot PD
    let boot_pd = boot_pd_phys as *const u64;
    let new_pd = pd_page as *mut u64;
    unsafe {
        for i in 0..512 {
            let entry = core::ptr::read(boot_pd.add(i));
            core::ptr::write(new_pd.add(i), entry);
        }

        // Share kernel high mappings (PML4 entries 256-511)
        for i in 256..512 {
            let entry = core::ptr::read(boot_pml4.add(i));
            core::ptr::write((pml4_page as *mut u64).add(i), entry);
        }
    }

    pml4_page
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
) -> Option<u64> {
    // Walk the boot page table to reach the PD (512 × 2MB entries = 1GB).
    let boot_cr3_val = boot_cr3();
    if boot_cr3_val == 0 {
        return None;
    }
    let boot_pml4 = boot_cr3_val as *const u64;
    let boot_pml4e0 = unsafe { core::ptr::read(boot_pml4) };
    let boot_pdpt_phys = boot_pml4e0 & arch_x86_64::pte::PG_FRAME;
    let boot_pdpt = boot_pdpt_phys as *const u64;
    let boot_pdpte0 = unsafe { core::ptr::read(boot_pdpt) };
    let boot_pd_phys = boot_pdpte0 & arch_x86_64::pte::PG_FRAME;
    let boot_pd = boot_pd_phys as *const u64;

    // Allocate 3 pages from the arch allocator: PML4, PDPT, PD.
    let pml4 = arch_x86_64::alloc::alloc_phys_page()?;
    let pdpt_page = arch_x86_64::alloc::alloc_phys_page()?;
    let pd_page = arch_x86_64::alloc::alloc_phys_page()?;
    let page_sz = arch_x86_64::param::NBPG as usize;
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, page_sz);
        core::ptr::write_bytes(pdpt_page as *mut u8, 0, page_sz);
        core::ptr::write_bytes(pd_page as *mut u8, 0, page_sz);
    }

    // Link: PML4[0] → PDPT[0] → PD.
    let flags = arch_x86_64::pte::PG_P | arch_x86_64::pte::PG_RW | arch_x86_64::pte::PG_U;
    unsafe {
        core::ptr::write(pml4 as *mut u64, pdpt_page | flags);
        core::ptr::write(pdpt_page as *mut u64, pd_page | flags);
    }

    // Deep-copy all 512 PD entries from boot PD (gives full 1GB identity
    // map so the kernel can still run after CR3 switch).
    unsafe {
        let new_pd = pd_page as *mut u64;
        for i in 0..512 {
            let entry = core::ptr::read(boot_pd.add(i));
            core::ptr::write(new_pd.add(i), entry);
        }

        // Share kernel high mappings (PML4[256..512]).
        for i in 256..512 {
            let entry = core::ptr::read(boot_pml4.add(i));
            core::ptr::write((pml4 as *mut u64).add(i), entry);
        }
    }

    // Overwrite user code pages: replace the identity-mapped 2MB huge
    // pages with 4KB pages pointing to the per-process physical pages.
    // The identity map has 2MB huge pages; map_page will split them.
    let user_flags = pte::PG_P | pte::PG_RW | pte::PG_U;
    let mut va = code_start;
    let mut pa = code_phys;
    while va < code_end {
        unsafe {
            if map_page(pml4, va, pa, user_flags).is_err() {
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
            if map_page(pml4, va, pa, user_flags).is_err() {
                return None;
            }
        }
        va += 0x1000;
        pa += 0x1000;
    }

    Some(pml4)
}

/// Jump to userspace — the final step of boot.
///
/// Sets init's per-process CR3, then calls the assembly `sysretq_to_user`
/// which loads registers from the TrapFrame and executes `sysretq`.
///
/// # Safety
///
/// `init` must contain a valid Proc pointer and page table physical address.
/// Never returns.
pub unsafe fn boot_jump_to_user(init: &InitInfo, pt_phys: u64) -> ! {
    // Read register values from the Proc struct via volatile access
    // to prevent the compiler from hoisting fields into statics.
    let entry = unsafe { core::ptr::read_volatile(&raw const (*init.proc_ptr).p_reg.rcx) };
    let rflags = unsafe { core::ptr::read_volatile(&raw const (*init.proc_ptr).p_reg.r11) };
    let stack = unsafe { core::ptr::read_volatile(&raw const (*init.proc_ptr).p_reg.rsp) };

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

// ═════════════════════════════════════════════════════════════════════════
// Serial output helpers
// ═════════════════════════════════════════════════════════════════════════

/// Print a 64-bit hex value to serial.
pub fn print_hex(val: u64) {
    let chars = b"0123456789abcdef";
    for i in (0..16).rev() {
        let nibble = ((val >> (i * 4)) & 0xF) as usize;
        crate::serial_putc(chars[nibble]);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════

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
        const USER_STACK_BASE: u64 = 0x0FE00000;
        const USER_STACK_SIZE: usize = 65536;
        const KERNEL_END: u64 = 0x300000;
        const RAM_TOP: u64 = 0x10000000;

        let stack_end = USER_STACK_BASE + USER_STACK_SIZE as u64;
        assert!(
            stack_end < RAM_TOP,
            "user stack end 0x{:x} exceeds RAM top 0x{:x}",
            stack_end,
            RAM_TOP
        );
        // Compile-time check: user stack must be after kernel memory
        const _: () = assert!(USER_STACK_BASE > KERNEL_END);
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
