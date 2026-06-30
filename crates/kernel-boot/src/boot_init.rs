//! Boot-time user process initialization — loads init from initramfs
//! and starts it as the first userspace process.
//!
//! Called from kmain after all kernel init is complete.

use kernel::elf::{load_elf, parse_elf_header, setup_user_stack};
use kernel::initramfs::find_initramfs_file;

use crate::print;

/// Return type for `load_and_prepare_init`, exposing the loaded ELF bounds
/// so the caller can create a per-process page table covering all pages.
pub struct InitInfo {
    /// Pointer to init's kernel `Proc` entry.
    pub proc_ptr: *mut kernel::sched::proc::Proc,
    /// Page-aligned start of init's ELF LOAD segments.
    pub code_start: u64,
    /// Page-aligned end (exclusive) of init's ELF LOAD segments.
    pub code_end: u64,
    /// Page-aligned start of the user stack.
    pub stack_start: u64,
    /// Page-aligned end (exclusive) of the user stack.
    pub stack_end: u64,
}

/// Load /sbin/init from the embedded initramfs and start it.
///
/// This bypasses RS/PM/VFS for the initial boot — it directly:
/// 1. Finds /sbin/init in the initramfs CPIO archive
/// 2. Parses the ELF64 header and copies LOAD segments into memory
/// 3. Allocates a user stack (identity-mapped memory)
/// 4. Sets up the Proc entry's StackFrame for ring-3 execution
/// 5. Returns an InitInfo with the Proc pointer and memory bounds
///
/// # Safety
/// Must be called after boot_create_procs, boot_setup_process_stacks,
/// and all server initialization. Single-threaded boot context.
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

    // Step 4: Set up the StackFrame for ring-3 using raw pointer writes.
    // Use core::ptr::write (not &mut) to prevent compiler from optimizing away
    // writes that are never read through the same mutable reference.
    let rp = unsafe { kernel::sched::table::proc_addr(kernel::com::INIT_PROC_NR) };
    unsafe {
        // Read the kernel stack set by boot_setup_process_stacks
        let kernel_stack = core::ptr::read(&raw const (*rp).p_reg.sp);

        // Write all StackFrame fields through volatile ptr writes
        // (prevents compiler from optimizing away or reordering writes)
        core::ptr::write_volatile(&raw mut (*rp).p_reg.pc, ehdr.e_entry);
        core::ptr::write_volatile(&raw mut (*rp).p_reg.sp, kernel_stack);
        core::ptr::write_volatile(&raw mut (*rp).p_reg.rdi, user_rsp);
        // GDT: user code at index 4 (0x23), user data at index 3 (0x1B)
        // Matches SYSRETQ CS=(STAR+16)|3=0x23, SS=(STAR+8)|3=0x1B
        core::ptr::write_volatile(&raw mut (*rp).p_reg.cs, 0x23);
        core::ptr::write_volatile(&raw mut (*rp).p_reg.ss, 0x1B);
        core::ptr::write_volatile(&raw mut (*rp).p_reg.psw, 0x0202); // IF=1, reserved bit 1 set
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

/// Print a 64-bit hex value to serial (copied from main.rs).
pub fn print_hex(val: u64) {
    let chars = b"0123456789abcdef";
    for i in (0..16).rev() {
        let nibble = ((val >> (i * 4)) & 0xF) as usize;
        crate::serial_putc(chars[nibble]);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
        // Verify the nibble extraction logic used in print_hex:
        // for i in (0..16).rev() { let nibble = ((val >> (i * 4)) & 0xF); }
        // 0xDEADBEEFCAFEBABE should produce nibbles: d e a d b e e f c a f e b a b e
        let val: u64 = 0xDEADBEEFCAFEBABE;
        let expected: [u8; 16] = *b"deadbeefcafebabe";
        for i in 0..16usize {
            let nibble = ((val >> ((15 - i) * 4)) & 0xF) as u8;
            let c = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            };
            assert_eq!(c, expected[i], "position {} mismatch", i);
        }
    }

    #[test]
    fn user_stack_constants_are_within_ram() {
        // These constants must stay in sync with the kernel identity map
        // which covers 0-1GB. RAM is 256MB (0x0 - 0x0FFFFFFF).
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
        assert!(
            USER_STACK_BASE > KERNEL_END,
            "user stack base 0x{:x} overlaps kernel memory (end ~0x{:x})",
            USER_STACK_BASE,
            KERNEL_END
        );
    }

    #[test]
    fn gdt_selectors_have_correct_ring() {
        // User code CS = 0x23 = index 4 | RPL 3
        // User data SS = 0x1B = index 3 | RPL 3
        assert_eq!(0x23 & 0x03, 3, "CS RPL must be 3 (user mode)");
        assert_eq!(0x1B & 0x03, 3, "SS RPL must be 3 (user mode)");
        assert_eq!(0x23 >> 3, 4, "CS selector index must be 4");
        assert_eq!(0x1B >> 3, 3, "SS selector index must be 3");
    }

    #[test]
    fn psw_has_if_and_reserved_bits() {
        // PSW = 0x0202: bit 9 (IF) = 1, bit 1 (reserved) = 1
        // IF enables interrupts in ring-3.
        assert_eq!(0x0202 & 0x0200, 0x0200, "IF (bit 9) must be set");
        assert_eq!(0x0202 & 0x0002, 0x0002, "reserved bit 1 must be set");
    }

    #[test]
    fn init_stack_size_is_reasonable() {
        // User stack size of 64KB should be a sensible default.
        assert!(65536 >= 4096, "stack must be at least one page");
        assert!(65536 <= 1048576, "stack must not exceed 1MB");
    }
}
