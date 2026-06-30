//! QEMU integration tests — run inside the kernel at boot time.
//!
//! Each test runs as a bare-metal assertion inside the kernel. Tests are run
//! sequentially. If all pass, QEMU exits with code 1. On failure, the exit
//! code encodes which tests failed.
//!
//! Enabled with `--features integration-tests`.

use arch_x86_64::hw::read_cr3;

/// Page table flag constants for integration tests.
const PG_P: u64 = arch_x86_64::pte::PG_P;
const PG_RW: u64 = arch_x86_64::pte::PG_RW;
const PG_U: u64 = arch_x86_64::pte::PG_U;
const PG_PS: u64 = arch_x86_64::pte::PG_PS;
const PG_FRAME: u64 = arch_x86_64::pte::PG_FRAME;

/// QEMU isa-debug-exit I/O port.
const QEMU_EXIT_PORT: u16 = 0x501;

/// Run all integration tests sequentially.
///
/// Returns the total failure count (0 = all passed).
pub fn run_integration_tests() -> ! {
    serial_puts("M1b integration tests\r\n");

    // Phase A: Page table basics
    let mut total: u32 = 0;
    total += test_boot_cr3();
    total += test_boot_pml4_entries();
    total += test_identity_map_range();
    total += test_kernel_high_map();
    total += test_serial_output();

    // Phase E: Ring-3 execution (M1b proof) — run LAST, never returns on success.
    // If all prior tests passed, attempt the ring-3 transition.
    // On success, the ring-3 code writes to the isa-debug-exit port and QEMU
    // exits with code 1. On failure, we call qemu_exit_failure.
    if total == 0 {
        test_sysretq_ring3();
        // If we get here, the ring-3 test setup failed
        qemu::qemu_exit_failure(1);
    } else {
        qemu::qemu_exit_failure(total);
    }
}

// ===========================================================================
// Test runner helpers
// ===========================================================================

/// Run a single test and return 0 (pass) or 1 (fail).
fn run(name: &str, f: fn(&mut TestCtx)) -> u32 {
    let mut ctx = TestCtx { failed: false };
    f(&mut ctx);
    if ctx.failed {
        serial_print_fail(name);
        1
    } else {
        serial_print_ok(name);
        0
    }
}

struct TestCtx {
    failed: bool,
}

impl TestCtx {
    fn assert(&mut self, cond: bool, msg: &str) {
        if !cond {
            self.failed = true;
            serial_print_fail_msg(msg);
        }
    }
}

// ===========================================================================
// Serial output helpers
// ===========================================================================

fn serial_putc(c: u8) {
    unsafe { arch_x86_64::hw::ser_putc(arch_x86_64::hw::COM1, c) }
}

fn serial_puts(s: &str) {
    for &b in s.as_bytes() {
        if b == b'\n' {
            serial_putc(b'\r');
        }
        serial_putc(b);
    }
}

fn serial_print_ok(name: &str) {
    serial_puts("  OK ");
    serial_puts(name);
    serial_putc(b'\n');
}

fn serial_print_fail(name: &str) {
    serial_puts("FAIL ");
    serial_puts(name);
    serial_putc(b'\n');
}

fn serial_print_fail_msg(msg: &str) {
    serial_puts("    ");
    serial_puts(msg);
    serial_putc(b'\n');
}

// ===========================================================================
// Phase A: Page Table Basics
// ===========================================================================

fn test_boot_cr3() -> u32 {
    run("boot_cr3", |t| {
        let cr3 = unsafe { read_cr3() };
        t.assert(cr3 != 0, "CR3 should not be zero");
        t.assert(cr3 & 0xFFF == 0, "CR3 should be page-aligned");

        let pml4 = cr3 as *const u64;
        unsafe {
            let entry0 = core::ptr::read(pml4.add(0));
            t.assert(entry0 & PG_P != 0, "PML4[0] should be present");
            t.assert(entry0 & PG_RW != 0, "PML4[0] should be writable");
            t.assert(entry0 & PG_U != 0, "PML4[0] should be user-accessible");
        }
    })
}

fn test_boot_pml4_entries() -> u32 {
    run("boot_pml4_entries", |t| {
        let cr3 = unsafe { read_cr3() };
        let pml4 = cr3 as *const u64;

        unsafe {
            // Entry 0 should be present (identity mapping)
            let entry0 = core::ptr::read(pml4.add(0));
            t.assert(entry0 & PG_P != 0, "PML4[0] should be present");
            t.assert(entry0 & PG_RW != 0, "PML4[0] should be writable");
            t.assert(entry0 & PG_U != 0, "PML4[0] should be user-accessible");
            t.assert(entry0 & PG_PS == 0, "PML4[0] should not be a huge page");
            let pdp_pa = entry0 & PG_FRAME;
            let pdp = pdp_pa as *const u64;
            let pdpe = core::ptr::read(pdp.add(0));
            t.assert(pdpe & PG_P != 0, "PDP[0] should be present");

            // Check kernel high mapping slot.
            // KERNBASE = 0xFFFF8000_00000000 → PML4 slot 511 (bits 47:39).
            let kern_slot: usize = 511;
            let kern_entry = core::ptr::read(pml4.add(kern_slot));
            t.assert(
                kern_entry & PG_P != 0,
                "kernel PML4 entry should be present",
            );

            // Verify no other PML4 entries are accidentally set
            for i in 1..512 {
                if i == kern_slot {
                    continue;
                }
                let e = core::ptr::read(pml4.add(i));
                t.assert(e == 0, "non-identity PML4 entries should be zero");
            }
        }
    })
}

fn test_identity_map_range() -> u32 {
    run("identity_map_range", |t| {
        unsafe {
            // The identity map should cover 0-1GB with 2MB large pages.
            // Verify a few key addresses are readable via identity mapping.
            let kernel_word: u32 = core::ptr::read_volatile(0x200000 as *const u32);
            t.assert(
                kernel_word != 0,
                "kernel code at 0x200000 should be readable",
            );
        }
    })
}

fn test_kernel_high_map() -> u32 {
    run("kernel_high_map", |t| {
        use arch_x86_64::param::KERNBASE;
        unsafe {
            // Verify the kernel is accessible via the high mapping
            let kernel_high_addr = KERNBASE + 0x200000u64;
            let word: u32 = core::ptr::read_volatile(kernel_high_addr as *const u32);
            t.assert(word != 0, "kernel code via high map should be readable");
        }
    })
}

fn test_serial_output() -> u32 {
    run("serial_output", |t| {
        unsafe {
            arch_x86_64::hw::ser_putc(arch_x86_64::hw::COM1, b'>');
            arch_x86_64::hw::ser_putc(arch_x86_64::hw::COM1, b'\n');
        }
        t.assert(true, "serial output should not crash");
    })
}

// ===========================================================================
// Phase E: Ring-3 Execution (M1b proof)
// ===========================================================================

/// Test that `sysretq` can transition to ring-3.
///
/// Sets up a tiny ring-3 code blob that writes directly to the QEMU
/// isa-debug-exit I/O port (with IOPL=3 in RFLAGS). The ring-3 code:
///
/// ```asm
/// mov dx, 0x501    ; QEMU isa-debug-exit port
/// mov eax, 0       ; exit code 0 → success
/// out dx, eax      ; QEMU exits with (0 << 1) | 1 = 1
/// hlt
/// jmp $-3
/// ```
///
/// If this function returns, the test setup failed (allocation, page table,
/// or Proc entry setup). The caller should call qemu_exit_failure.
fn test_sysretq_ring3() {
    serial_puts("  sysretq_ring3: allocating pages...\r\n");

    // Step 1: Allocate a page for ring-3 code at a known identity-mapped
    // address. Use the arch physical allocator (initialized by kmain).
    let code_page = match arch_x86_64::alloc::alloc_phys_page() {
        Some(p) => p,
        None => {
            serial_puts("FAIL: code page allocation\r\n");
            return;
        }
    };

    // Step 2: Write the ring-3 code blob.
    // x86-64 machine code:
    //   66 BA 01 05   mov dx, 0x501
    //   B8 00 00 00 00 mov eax, 0
    //   EF            out dx, eax
    //   F4            hlt
    //   EB FD         jmp $-3
    let code: [u8; 13] = [
        0x66, 0xBA, 0x01, 0x05, // mov dx, 0x501
        0xB8, 0x00, 0x00, 0x00, 0x00, // mov eax, 0
        0xEF, // out dx, eax
        0xF4, // hlt
        0xEB, 0xFD, // jmp $-3
    ];
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), code_page as *mut u8, code.len());
    }

    // Step 3: Allocate a user stack page.
    let stack_page = match arch_x86_64::alloc::alloc_phys_page() {
        Some(p) => p,
        None => {
            serial_puts("FAIL: stack page allocation\r\n");
            return;
        }
    };
    let stack_top = stack_page + arch_x86_64::param::NBPG;

    serial_puts("  sysretq_ring3: creating page table...\r\n");

    // Step 4: Create a per-process page table that deep-copies the boot
    // identity map (which has PG_U set on all 2MB pages) and shares
    // kernel high mappings.
    let pt_phys = unsafe { crate::boot_init::boot_create_page_table() };
    if pt_phys == 0 {
        serial_puts("FAIL: page table creation\r\n");
        return;
    }

    serial_puts("  sysretq_ring3: setting up Proc entry...\r\n");

    // Step 5: Set up init's Proc entry for sysretq.
    // Use the boot-allocated INIT_PROC_NR slot.
    let rp = kernel::table::proc_addr(arch_common::com::INIT_PROC_NR);
    if rp.is_null() {
        serial_puts("FAIL: null proc_addr\r\n");
        return;
    }

    unsafe {
        // Set per-process CR3
        (*rp).p_seg.p_cr3 = pt_phys;

        // RCX → RIP via sysretq: point at the ring-3 code
        (*rp).p_reg.rcx = code_page;

        // R11 → RFLAGS via sysretq:
        //   PSL_USERSET = 0x0202 (IF=1, MBO=1)
        //   IOPL=3 (bits 12-13): gives ring-3 I/O port access
        //   Combined: 0x3202
        (*rp).p_reg.r11 = 0x3202u64;

        // RSP = top of user stack
        (*rp).p_reg.rsp = stack_top;
    }

    serial_puts("  sysretq_ring3: jumping to ring-3...\r\n");

    // Step 6: Execute sysretq. On success, the ring-3 code runs and QEMU
    // exits via isa-debug-exit. This function never returns.
    unsafe {
        arch_x86_64::asm::sysretq_to_user(rp as *const u8);
    }
}

// ===========================================================================
// QEMU exit helpers
// ===========================================================================

mod qemu {
    /// QEMU isa-debug-exit device I/O port.
    const QEMU_EXIT_PORT: u16 = 0x501;

    /// Exit code meaning "all tests passed".
    const QEMU_EXIT_SUCCESS: u32 = 0x01;

    /// Exit code meaning "some tests failed".
    fn qemu_exit(code: u32) -> ! {
        unsafe {
            core::arch::asm!("out dx, eax", in("dx") QEMU_EXIT_PORT, in("eax") code);
        }
        loop {
            unsafe {
                core::arch::asm!("hlt", options(nostack));
            }
        }
    }

    pub fn qemu_exit_success() -> ! {
        qemu_exit(QEMU_EXIT_SUCCESS);
    }

    pub fn qemu_exit_failure(failures: u32) -> ! {
        qemu_exit(failures << 1 | 1);
    }
}
