//! QEMU integration tests — run inside the kernel at boot time.
//!
//! Each test runs as a bare-metal assertion inside the kernel. Tests are run
//! sequentially. If all pass, QEMU exits with code 1. On failure, the exit
//! code encodes which tests failed.
//!
//! Enabled with `--features integration-tests`.

use core::sync::atomic::Ordering;

use arch_x86_64::boot_pgtbl::{BOOT_PD, BOOT_PDP, BOOT_PML4};
use arch_x86_64::hw::{read_cr3, write_cr3};
use arch_x86_64::pagetable::*;

/// Page table flag constants for integration tests.
const PG_P: u64 = arch_x86_64::pte::PG_P;
const PG_RW: u64 = arch_x86_64::pte::PG_RW;
const PG_U: u64 = arch_x86_64::pte::PG_U;
const PG_PS: u64 = arch_x86_64::pte::PG_PS;
const PG_FRAME: u64 = arch_x86_64::pte::PG_FRAME;

/// Run all integration tests sequentially.
///
/// Returns the total failure count (0 = all passed).
pub fn run_integration_tests() -> ! {
    // Track total failures across all tests
    let mut total: u32 = 0;

    // Phase A: Page table basics
    total += test_boot_cr3();
    total += test_boot_pml4_entries();
    total += test_identity_map_range();
    total += test_kernel_high_map();
    total += test_serial_output();

    // Phase B: Page table manipulation
    total += test_pt_new_and_switch();
    total += test_pt_split_and_remap();

    // Phase C: IPC sanity checks
    total += test_do_sync_ipc_direct();

    // Phase D: Per-process page tables
    total += test_pt_new_for_init();

    if total == 0 {
        qemu::qemu_exit_success();
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
    unsafe { arch_x86_64::hw::ser_putc(arch_x86_64::hw::COM1_PORT, c) }
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

            // Check kernel high mapping slot (L4_SLOT_DIRECT = 509)
            let slot = arch_x86_64::pmap::L4_SLOT_DIRECT;
            let kern_entry = core::ptr::read(pml4.add(slot));
            t.assert(
                kern_entry & PG_P != 0,
                "kernel PML4 entry should be present",
            );

            // Verify no other PML4 entries are accidentally set
            for i in 1..512 {
                if i == slot {
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
            arch_x86_64::hw::ser_putc(arch_x86_64::hw::COM1_PORT, b'>');
            arch_x86_64::hw::ser_putc(arch_x86_64::hw::COM1_PORT, b'\n');
        }
        t.assert(true, "serial output should not crash");
    })
}

// ===========================================================================
// Phase B: Page Table Manipulation
// ===========================================================================

fn test_pt_new_and_switch() -> u32 {
    run("pt_new_and_switch", |t| {
        use servers::vm::proc::Vmproc;

        // Save original CR3 to restore later
        let original_cr3 = unsafe { read_cr3() };
        t.assert(original_cr3 != 0, "original CR3 should be valid");

        // Create a per-process page table
        let mut vmp = Vmproc::default();
        vmp.vm_endpoint = kernel::com::INIT_PROC_NR;
        vmp.vm_flags = servers::vm::proc::VmFlags::INUSE;
        t.assert(
            servers::vm::proc::pt_new(&mut vmp) == 0,
            "pt_new should succeed",
        );
        t.assert(
            vmp.vm_pt.dir_phys != 0,
            "new PT should have non-zero dir_phys",
        );
        t.assert(
            vmp.vm_pt.dir_phys != original_cr3,
            "new PT should differ from boot CR3",
        );

        // Switch to the new page table
        unsafe {
            write_cr3(vmp.vm_pt.dir_phys);
        }
        let new_cr3 = unsafe { read_cr3() };
        t.assert(
            new_cr3 == vmp.vm_pt.dir_phys,
            "CR3 should equal the new PML4 physical address",
        );

        // Verify kernel code still readable after CR3 switch
        unsafe {
            let word: u32 = core::ptr::read_volatile(0x200000 as *const u32);
            t.assert(word != 0, "kernel code readable after CR3 switch");
        }

        // Restore original CR3
        unsafe {
            write_cr3(original_cr3);
        }
        let restored_cr3 = unsafe { read_cr3() };
        t.assert(
            restored_cr3 == original_cr3,
            "CR3 should be restored to original",
        );
    })
}

fn test_pt_split_and_remap() -> u32 {
    run("pt_split_and_remap", |t| {
        use servers::vm::proc::Vmproc;

        let original_cr3 = unsafe { read_cr3() };

        // Create a new page table
        let mut vmp = Vmproc::default();
        vmp.vm_endpoint = kernel::com::INIT_PROC_NR;
        vmp.vm_flags = servers::vm::proc::VmFlags::INUSE;
        t.assert(servers::vm::proc::pt_new(&mut vmp) == 0, "pt_new");

        let test_va: u64 = 0x1000000; // init's load address

        // Split the 2MB PDE covering test_va
        let split_result = unsafe { servers::vm::pagetable::pt_split_pde(&mut vmp.vm_pt, test_va) };
        t.assert(split_result == 0, "pt_split_pde should succeed");

        // Allocate a new physical page
        let page_num = servers::physmem::allocator::allocator_mut()
            .alloc(1, servers::physmem::allocator::AllocFlags::empty());
        t.assert(
            page_num != servers::physmem::allocator::NO_MEM,
            "alloc should succeed",
        );
        let new_frame = servers::physmem::allocator::PhysMemAllocator::page2phys(page_num);

        // Remap test_va to the new frame
        let remap_result =
            unsafe { servers::vm::pagetable::pt_remap_page(&mut vmp.vm_pt, test_va, new_frame) };
        t.assert(remap_result == 0, "pt_remap_page should succeed");

        // Write a test pattern to the new frame (identity mapped)
        unsafe {
            core::ptr::write_volatile(new_frame as *mut u32, 0xDEADBEEF);
        }

        // Switch to the new PT and verify the test pattern
        unsafe {
            write_cr3(vmp.vm_pt.dir_phys);
        }
        unsafe {
            let val: u32 = core::ptr::read_volatile(test_va as *const u32);
            t.assert(val == 0xDEADBEEF, "remapped page should contain test value");
        }

        // Restore original CR3
        unsafe {
            write_cr3(original_cr3);
        }
    })
}

// ===========================================================================
// Phase C: IPC Sanity Checks
// ===========================================================================

fn test_do_sync_ipc_direct() -> u32 {
    run("do_sync_ipc_direct", |t| {
        // Set up a minimal Proc entry for the test
        let mut proc = kernel::sched::proc::Proc::default();
        proc.p_endpoint = kernel::com::INIT_PROC_NR;
        proc.p_nr = kernel::com::INIT_PROC_NR;
        proc.p_rts_flags = kernel::sched::proc::RtsFlags::empty();
        let stack = unsafe { arch_x86_64::kern_stack::new_kernel_stack() };
        if !stack.is_null() {
            proc.p_reg.sp = stack as u64;
        }

        let mut msg = kernel::msg::Message::default();
        msg.m_type = 0x02B; // PM_EXEC_NEW
        msg.m_payload.m_lc_pm_exec.name = b"/bin/sh\0" as *const u8 as u64;
        msg.m_payload.m_lc_pm_exec.namelen = 7;

        // Call do_sync_ipc the same way ipc_sendrec_handler would
        let result = unsafe {
            kernel::ipc::do_sync_ipc(
                &raw mut proc as *mut kernel::sched::proc::Proc,
                kernel::ipc::SENDREC as i32,
                kernel::com::PM_PROC_NR,
                &raw mut msg as *mut kernel::msg::Message,
            )
        };

        t.assert(result == 0, "do_sync_ipc should return OK");
        t.assert(msg.m_type <= 0, "msg.m_type should be success or error");
    })
}

// ===========================================================================
// Phase D: Per-Process Page Tables
// ===========================================================================

fn test_pt_new_for_init() -> u32 {
    run("pt_new_for_init", |t| {
        // Record original CR3
        let original_cr3 = unsafe { read_cr3() };

        // Create a per-process page table with private pages for init
        let result = servers::vm::proc::pt_new_for_init(
            0x1000000,  // code_start
            0x1014000,  // code_end (from boot log: ~80KB)
            0x0FE00000, // stack_start
            0x0FF00000, // stack_end (64KB)
        );
        t.assert(result == 0, "pt_new_for_init should return 0");

        // After pt_new_for_init, CR3 should have switched to the new PT
        let new_cr3 = unsafe { read_cr3() };
        t.assert(new_cr3 != 0, "CR3 should be non-zero after switch");
        t.assert(
            new_cr3 != original_cr3,
            "CR3 should differ from boot CR3 (per-process PT)",
        );

        // The kernel should still be readable at 0x200000
        unsafe {
            let word: u32 = core::ptr::read_volatile(0x200000 as *const u32);
            t.assert(word != 0, "kernel code still readable after CR3 switch");
        }

        // Init's code at 0x1000000 should be readable (private copy)
        unsafe {
            // Read the ELF magic from init's load address
            let magic: u32 = core::ptr::read_volatile(0x1000000 as *const u32);
            t.assert(magic & 0x7F == 0x7F, "init ELF magic should be readable"); // ELF magic starts with 0x7F
        }

        // Verify that CR3 != 0 on the kernel's Proc struct
        let init_proc = unsafe { kernel::sched::table::proc_addr(kernel::com::INIT_PROC_NR) };
        let p_cr3 = unsafe { (*init_proc).p_seg.p_cr3 };
        t.assert(p_cr3 != 0, "init's p_cr3 should be non-zero");
        t.assert(
            p_cr3 != original_cr3,
            "init's p_cr3 should differ from boot CR3",
        );
        t.assert(p_cr3 == new_cr3, "init's p_cr3 should match current CR3");

        // Restore original CR3 for subsequent tests
        unsafe {
            write_cr3(original_cr3);
        }
    })
}

// ===========================================================================
// QEMU exit helpers
// ===========================================================================

mod qemu {
    use core::sync::atomic::{AtomicU32, Ordering};

    /// QEMU isa-debug-exit device I/O port.
    const QEMU_EXIT_PORT: u16 = 0x501;

    /// Exit code meaning "all tests passed".
    const QEMU_EXIT_SUCCESS: u32 = 0x01;

    /// Exit code meaning "some tests failed".
    /// The kernel shifts `failures` left by 1 and sets bit 0.
    fn qemu_exit(code: u32) -> ! {
        unsafe {
            // Write the exit code to the isa-debug-exit I/O port.
            // QEMU reads this and exits with `(code << 1) | 1`.
            core::arch::asm!("out dx, eax", in("dx") QEMU_EXIT_PORT, in("eax") code);
        }
        loop {
            core::arch::asm!("hlt", options(nostack));
        }
    }

    pub fn qemu_exit_success() -> ! {
        qemu_exit(QEMU_EXIT_SUCCESS);
    }

    pub fn qemu_exit_failure(failures: u32) -> ! {
        // Shift left by 1 and set bit 0 to indicate failure.
        // Exit code = (failures << 1) | 1, but > 1 means failure.
        qemu_exit(failures << 1 | 1);
    }
}
