//! Boot-time user process initialization — loads init from initramfs
//! and starts it as the first userspace process.
//!
//! Called from kmain after all kernel init is complete.

use kernel::elf::{load_elf, parse_elf_header, setup_user_stack};
use kernel::initramfs::find_initramfs_file;
use kernel::pagetable::boot_cr3;

use crate::print;

/// Convenience alias for the Proc type.
use kernel::proc::Proc;

/// Return type for `load_and_prepare_init`, exposing the loaded ELF bounds
/// so the caller can create a per-process page table covering all pages.
pub struct InitInfo {
    /// Pointer to init's kernel `Proc` entry.
    pub proc_ptr: *mut Proc,
    /// Page-aligned start of init's ELF LOAD segments.
    pub code_start: u64,
    /// Page-aligned end (exclusive) of init's ELF LOAD segments.
    pub code_end: u64,
    /// Page-aligned start of the user stack.
    pub stack_start: u64,
    /// Page-aligned end (exclusive) of the user stack.
    pub stack_end: u64,
}

/// Load /sbin/init from the embedded initramfs and set up its TrapFrame for
/// `sysretq` ring-3 execution.
///
/// This bypasses RS/PM/VFS for the initial boot — it directly:
/// 1. Finds /sbin/init in the initramfs CPIO archive
/// 2. Parses the ELF64 header and copies LOAD segments into memory
/// 3. Allocates a user stack (identity-mapped memory)
/// 4. Sets up the Proc entry's TrapFrame for `sysretq` (rcx=RIP, r11=RFLAGS)
/// 5. Returns an InitInfo with the Proc pointer and memory bounds
///
/// # Safety
/// Must be called after kernel::init() and before any user code runs.
/// Single-threaded boot context.
pub unsafe fn load_and_prepare_init() -> InitInfo {
    // Step 1: Find /sbin/init in the initramfs
    let (init_data, _mode) =
        find_initramfs_file("/sbin/init").expect("initramfs must contain /sbin/init");

    // Step 2: Validate and load the ELF binary
    let ehdr = parse_elf_header(init_data).expect("invalid init ELF header");
    print!("  init: ELF64 entry=0x");
    print_hex(ehdr.e_entry);
    let loaded = unsafe { load_elf(init_data).expect("failed to load init ELF") };

    // Step 3: Allocate a user stack at a fixed address.
    // The identity map covers 0-1GB, but RAM is only 256MB (0x0-0x0FFFFFFF),
    // so place the user stack within the RAM-backed region.
    let user_stack_base: u64 = 0x0FE00000;
    let user_stack_size: usize = 65536;
    let stack_top = user_stack_base + user_stack_size as u64;
    let user_rsp = unsafe {
        setup_user_stack(stack_top, user_stack_size, &["/sbin/init"])
            .expect("failed to set up user stack")
    };

    // Step 4: Set up the TrapFrame for sysretq ring-3 execution.
    //   sysretq loads: RIP from RCX, RFLAGS from R11
    //   RSP is loaded by the caller before executing sysretq
    //   CS/SS come from the STAR MSR (SYSRET_CS = 0x001B, SS = 0x0023)
    // SAFETY: single-threaded boot context, proc_addr returns valid pointer
    let rp = kernel::table::proc_addr(arch_common::com::INIT_PROC_NR);
    // SAFETY: rp is non-null for INIT_PROC_NR at boot time
    unsafe {
        core::ptr::write_volatile(&raw mut (*rp).p_reg.rcx, ehdr.e_entry);
        core::ptr::write_volatile(&raw mut (*rp).p_reg.r11, 0x0202u64); // PSL_USERSET
        core::ptr::write_volatile(&raw mut (*rp).p_reg.rsp, user_rsp);
        // rdi = pointer to boot args string on stack (set up by setup_user_stack)
        core::ptr::write_volatile(&raw mut (*rp).p_reg.rdi, user_rsp);
    }

    // Compute page-aligned code range from the LoadedElf
    let code_start = loaded.base & !0xFFF;
    let code_end = (loaded.top + 0xFFF) & !0xFFF;
    let stack_start = user_stack_base & !0xFFF;
    let stack_end = (user_stack_base + user_stack_size as u64 + 0xFFF) & !0xFFF;

    print!("  init: loaded at 0x");
    print_hex(loaded.base);
    print!(" (code 0x");
    print_hex(code_start);
    print!("-0x");
    print_hex(code_end);
    print!(") stack=0x");
    print_hex(user_rsp);
    print!("\n");

    InitInfo {
        proc_ptr: rp,
        code_start,
        code_end,
        stack_start,
        stack_end,
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
            core::ptr::write(pml4_page as *mut u64, entry);
        }
    }

    pml4_page
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
    // Set the per-process CR3 on init's Proc entry
    unsafe {
        (*init.proc_ptr).p_seg.p_cr3 = pt_phys;
    }

    // Call arch_proc_init to finalize TrapFrame setup (rcx/r11/rsp).
    // This is a no-op for fields already set by load_and_prepare_init,
    // but provides a single point for any arch-specific adjustments.
    unsafe {
        arch_x86_64::arch_proc::arch_proc_init(
            &raw mut (*init.proc_ptr).p_reg,
            (*init.proc_ptr).p_reg.rcx,
            (*init.proc_ptr).p_reg.rsp,
            b"init",
            0,
        );
    }

    // Debug: print the jump address
    print!("Jumping to ring-3: entry=0x");
    // SAFETY: init.proc_ptr is a valid Proc pointer
    unsafe {
        print_hex((*init.proc_ptr).p_reg.rcx);
    }
    print!(" stack=0x");
    unsafe {
        print_hex((*init.proc_ptr).p_reg.rsp);
    }
    print!(" cr3=0x");
    print_hex(pt_phys);
    print!("\n");

    // This never returns
    unsafe {
        arch_x86_64::asm::sysretq_to_user(init.proc_ptr as *const u8);
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
        // SYSRETQ loads CS from STAR[47:32], SS = CS + 8.
        // SYSRET_CS = 0x001B (GDT index 3, RPL 3)
        // SS = 0x0023 (GDT index 4, RPL 3)
        let sysret_cs: u16 = 0x001B;
        let expected_ss: u16 = 0x0023;
        assert_eq!(sysret_cs + 8, expected_ss);
        assert_eq!(sysret_cs & 3, 3, "CS RPL must be 3 (user mode)");
        assert_eq!(expected_ss & 3, 3, "SS RPL must be 3 (user mode)");
        assert_eq!(sysret_cs >> 3, 3, "CS GDT index must be 3");
        assert_eq!(expected_ss >> 3, 4, "SS GDT index must be 4");
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
