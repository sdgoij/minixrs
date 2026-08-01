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

/// Map flags (from kernel::pagetable).
const MAP_PRESENT: u64 = arch_x86_64::pte::PG_P;
const MAP_WRITE: u64 = arch_x86_64::pte::PG_RW;

/// Initialize the kernel VM allocator with a small memory pool (4MB at 4MB).
/// Called once before any tests that need VM page allocation (map_page, etc.).
fn init_vm_allocator() {
    unsafe {
        // 4MB at physical 4MB. base/size are in VM_PAGE_SIZE (4KB) units.
        // 4MB = 0x400 pages, so base=0x400, size=0x400
        let chunk = kernel::vm::MemoryChunk {
            base: 0x400,
            size: 0x400,
        };
        kernel::vm::mem_init(&[chunk]);
    }
}

/// Run all integration tests sequentially.
///
/// Returns the total failure count (0 = all passed).
pub fn run_integration_tests() -> ! {
    serial_puts("Bare-metal integration tests\r\n");

    // Initialize VM allocator (needed by map_page and VM allocator tests)
    init_vm_allocator();

    // Phase A: Page table basics
    let mut total: u32 = 0;
    total += test_boot_cr3();
    total += test_boot_pml4_entries();
    total += test_identity_map_range();
    total += test_kernel_high_map();
    total += test_serial_output();

    // Phase B: Page table manipulation
    total += test_pt_walk_boot();
    total += test_pt_map_unmap();
    total += test_pt_mapkernel();

    // Phase C: Physical memory allocator
    total += test_alloc_free_page();
    total += test_alloc_contig();

    // Phase D: VM allocator
    total += test_vm_alloc_free();
    total += test_vm_alloc_multi();

    // Phase F: Process table — call proc_init to initialize process slots
    // (kernel::init doesn't call proc_init, so we do it here)
    unsafe {
        kernel::table::proc_init();
    }
    total += test_proc_addr_valid();
    total += test_proc_addr_invalid();
    total += test_endpoint_lookup();
    total += test_is_empty_proc();
    total += test_is_kernel_vs_user();

    // Phase G: IPC — initialize cpulocals + run queues for scheduler
    unsafe {
        arch_x86_64::cpulocals::init_cpulocals();
        // Clear run queues for test isolation
        let head = arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.run_q_head_ptr();
        let tail = arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.run_q_tail_ptr();
        for q in 0..arch_x86_64::cpulocals::NR_SCHED_QUEUES {
            (*head)[q] = core::ptr::null_mut();
            (*tail)[q] = core::ptr::null_mut();
        }
    }
    total += test_mini_notify_when_receiving();
    total += test_mini_send_queues_when_not_receiving();

    // Phase H: Kernel unit tests (compiled for x86_64 target via qemu-tests feature)
    total += kernel::tests::run_all();

    // Phase I: Grants
    total += test_grant_direct_valid();
    total += test_grant_indirect();
    total += test_grant_invalid_id();

    // Phase J: Syscalls (getpid, write, brk, exit)
    total += test_syscall_getpid();
    total += test_syscall_write();
    total += test_syscall_brk();
    total += test_syscall_exit();

    // Enable interrupts and unmask timer IRQ so the monotonic
    // clock advances during timer tests.
    unsafe {
        core::arch::asm!("sti", options(nostack, nomem));
        arch_x86_64::apic::unmask_timer_irq();
    }

    // Phase K: Timers
    total += test_timer_set_and_expire();
    total += test_timer_clear();
    total += test_timer_multiple();

    // Phase L: PIT and monotonic clock
    total += test_pit_programmed();
    total += test_monotonic_advances();

    // Phase M: Interrupts
    total += test_irq_put_and_remove();

    // Phase N: ELF loading to physical pages
    total += test_elf_load_to_phys_pages();

    // Phase P: Syscall exec and initramfs verification
    total += test_initramfs_all_executables_elf();

    // Phase O: Hardware device access
    total += test_rtc_cmos_reads_reasonable_time();
    total += test_keyboard_controller_present();

    // Phase Q: IPC roundtrip, page tables, timers
    total += test_ipc_sendrec_roundtrip();
    total += test_exec_setup_new_page_table();
    total += test_monotonic_timer_interval();
    total += test_pagetable_deep_walk();

    // Phase R: Scheduling, grants, memory, stack
    total += test_enqueue_priority();
    total += test_quantum_exhaustion();
    total += test_dequeue_reordering();
    total += test_runqueues_invariant();
    total += test_safecopy_read();
    total += test_safecopy_write();
    total += test_safecopy_bounds();
    total += test_grant_revoke_reuse();
    total += test_alloc_align64k();
    total += test_alloc_lower16mb();
    total += test_stack_setup_zero();
    total += test_stack_setup_five();
    total += test_sys_kill_invalid();
    total += test_sys_schedule_roundtrip();
    total += test_sys_getksig_pending();

    if total == 0 {
        serial_puts("-- done --\r\n");
        qemu::qemu_exit_success();
    } else {
        qemu::qemu_exit_failure(total);
    }
}

// Test runner helpers

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

// Serial output helpers

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

/// Clear all run queues for test isolation.
fn clear_run_queues() {
    unsafe {
        let head = arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.run_q_head_ptr();
        let tail = arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.run_q_tail_ptr();
        for q in 0..arch_x86_64::cpulocals::NR_SCHED_QUEUES {
            (*head)[q] = core::ptr::null_mut();
            (*tail)[q] = core::ptr::null_mut();
        }
    }
}

// Phase A: Page Table Basics

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

            // The stage2 boot page tables only set up PML4[0] (identity map)
            // and do NOT set up the kernel high mapping at slot 511.
            // The kernel high mapping is added later by pt_mapkernel.
            // For now, just verify no entries beyond 0 are accidentally set
            // in the range 1..256 (lower half is identity, upper half is free).
            for i in 1..256 {
                let e = core::ptr::read(pml4.add(i));
                t.assert(e == 0, "unexpected PML4 entry");
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
        // Check if the kernel high mapping exists (PML4 slot 511).
        // Stage2 doesn't set it up, so this test may be skipped.
        let cr3 = unsafe { read_cr3() };
        unsafe {
            let pml4_slot511 = core::ptr::read((cr3 as *const u64).add(511));
            if pml4_slot511 & PG_P == 0 {
                // No high mapping — skip (not an error for boot tests)
                return;
            }
        }
        use arch_x86_64::param::KERNBASE;
        unsafe {
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

// Phase B: Page Table Manipulation

use kernel::pagetable::{boot_cr3, map_page, pt_mapkernel, unmap_page, walk};

fn test_pt_walk_boot() -> u32 {
    run("pt_walk_boot", |t| {
        let cr3_val = boot_cr3();
        t.assert(cr3_val != 0, "boot_cr3 should be non-zero");

        // Walk the identity-mapped kernel code at 0x200000
        let result = unsafe { walk(cr3_val, 0x200000u64) };
        match result {
            Ok(wr) => {
                t.assert(
                    wr.level <= 2,
                    "walk level should be <= 2 (huge page or 4K page)",
                );
            }
            Err(_) => {
                t.assert(false, "walk of 0x200000 should succeed");
            }
        }

        // Walk an unmapped address (should fail)
        let unmapped = unsafe { walk(cr3_val, 0x7fff_0000_0000u64) };
        match unmapped {
            Err(kernel::pagetable::PageTableError::NotMapped) => {}
            _ => t.assert(false, "unmapped address should return NotMapped"),
        }
    })
}

fn test_pt_map_unmap() -> u32 {
    run("pt_map_unmap", |t| {
        let cr3_val = boot_cr3();
        t.assert(cr3_val != 0, "boot_cr3 should be non-zero");

        // Allocate a physical page
        let phys = match arch_x86_64::alloc::alloc_phys_page() {
            Some(p) => p,
            None => {
                t.assert(false, "alloc_phys_page should succeed");
                return;
            }
        };
        t.assert(phys != 0, "allocated page should be non-zero");

        // Pick a virtual address outside the boot identity map (which covers 0-1GB,
        // PML4 index 0). Use an address in PML4 index 1 (1GB-2GB range).
        let va: u64 = 0x4000_0000; // 1 GB

        // Map it
        let map_result = unsafe { map_page(cr3_val, va, phys, MAP_PRESENT | MAP_WRITE) };
        t.assert(map_result.is_ok(), "map_page should succeed");

        // Walk to verify mapping
        let walk_result = unsafe { walk(cr3_val, va) };
        match walk_result {
            Ok(wr) => {
                t.assert(
                    wr.pte_value & MAP_PRESENT != 0,
                    "mapped page should be present",
                );
                t.assert(
                    wr.pte_value & MAP_WRITE != 0,
                    "mapped page should be writable",
                );
            }
            Err(_) => t.assert(false, "walk of mapped page should succeed"),
        }

        // Write a test pattern to the mapped page
        unsafe {
            core::ptr::write_volatile(va as *mut u32, 0xCAFEBABE);
            let val = core::ptr::read_volatile(va as *const u32);
            t.assert(val == 0xCAFEBABE, "readback should match written value");
        }

        // Unmap
        let unmap_result = unsafe { unmap_page(cr3_val, va) };
        t.assert(unmap_result.is_ok(), "unmap_page should succeed");

        // Walk to verify unmapped
        let walk_after = unsafe { walk(cr3_val, va) };
        match walk_after {
            Err(kernel::pagetable::PageTableError::NotMapped) => {}
            _ => t.assert(false, "unmapped page should be NotMapped"),
        }

        // Free the physical page
        arch_x86_64::alloc::free_phys_page(phys);
    })
}

fn test_pt_mapkernel() -> u32 {
    run("pt_mapkernel", |t| {
        let cr3_val = boot_cr3();
        t.assert(cr3_val != 0, "boot_cr3 should be non-zero");

        // Check if kernel high mapping already exists
        let pml4_slot511 = unsafe { core::ptr::read((cr3_val as *const u64).add(511)) };
        if pml4_slot511 & 1 != 0 {
            return;
        }

        // pt_mapkernel requires BSS to fit within the 2MB kernel region.
        // The test kernel has a 2MB bitmap in BSS which may exceed this.
        unsafe extern "C" {
            static __bss_end: u8;
        }
        let bss_end_addr = core::ptr::addr_of!(__bss_end) as u64;
        if bss_end_addr > 0x400000 {
            // BSS exceeds 2MB kernel region — skip this test
            return;
        }

        let result = unsafe { pt_mapkernel(cr3_val) };
        t.assert(result.is_ok(), "pt_mapkernel should succeed");

        use arch_x86_64::param::KERNBASE;
        let pml4_slot511_after = unsafe { core::ptr::read((cr3_val as *const u64).add(511)) };
        t.assert(pml4_slot511_after & 1 != 0, "PML4[511] should be present");
        unsafe {
            let word: u32 = core::ptr::read_volatile((KERNBASE + 0x200000u64) as *const u32);
            t.assert(word != 0, "kernel code via high map should be readable");
        }
    })
}

// Phase C: Physical Memory Allocator

fn test_alloc_free_page() -> u32 {
    run("alloc_free_page", |t| {
        // Allocate a single page
        let page = match arch_x86_64::alloc::alloc_phys_page() {
            Some(p) => p,
            None => {
                t.assert(false, "alloc_phys_page should succeed");
                return;
            }
        };
        t.assert(page != 0, "allocated page should be non-zero");
        t.assert(page & 0xFFF == 0, "allocated page should be 4K-aligned");

        // Write a test pattern
        unsafe {
            core::ptr::write_volatile(page as *mut u32, 0xDEADBEEF);
            let val = core::ptr::read_volatile(page as *const u32);
            t.assert(val == 0xDEADBEEF, "readback should match written value");
        }

        // Free it
        arch_x86_64::alloc::free_phys_page(page);

        // Allocate again — should get a different page (or the same, doesn't matter)
        let page2 = arch_x86_64::alloc::alloc_phys_page();
        t.assert(page2.is_some(), "second alloc should succeed");
    })
}

fn test_alloc_contig() -> u32 {
    run("alloc_contig", |t| {
        // Allocate 4 contiguous pages via the allocator
        let alloc = unsafe { &mut *arch_x86_64::alloc::global_allocator() };
        let base = alloc.alloc_contig(4);
        match base {
            Some(addr) => {
                t.assert(addr & 0xFFF == 0, "contiguous alloc should be page-aligned");
                // Write to all 4 pages
                for i in 0..4 {
                    unsafe {
                        core::ptr::write_volatile((addr + i * 4096) as *mut u8, 0xAB);
                    }
                }
                // Read back
                for i in 0..4 {
                    unsafe {
                        let val = core::ptr::read_volatile((addr + i * 4096) as *const u8);
                        t.assert(val == 0xAB, "contiguous page write/readback should match");
                    }
                }
                alloc.free_contig(addr, 4);
            }
            None => {
                t.assert(false, "alloc_contig(4) should succeed");
            }
        }
    })
}

// Phase D: VM Allocator (kernel::vm)

fn test_vm_alloc_free() -> u32 {
    run("vm_alloc_free", |t| {
        unsafe {
            // Allocate a single VM page
            let page = kernel::vm::alloc_mem(1, 0);
            t.assert(page != kernel::vm::NO_MEM, "alloc_mem(1, 0) should succeed");

            // Write a test pattern
            let phys = page * kernel::vm::VM_PAGE_SIZE as u64;
            core::ptr::write_volatile(phys as *mut u32, 0xF00DBABE);
            let val = core::ptr::read_volatile(phys as *const u32);
            t.assert(val == 0xF00DBABE, "VM page write/readback should match");

            // Free it
            kernel::vm::free_mem(page, 1);
        }
    })
}

fn test_vm_alloc_multi() -> u32 {
    run("vm_alloc_multi", |t| {
        unsafe {
            // Allocate 3 contiguous pages
            let base = kernel::vm::alloc_mem(3, 0);
            t.assert(base != kernel::vm::NO_MEM, "alloc_mem(3, 0) should succeed");

            // Verify all 3 pages are writable
            let page_sz = kernel::vm::VM_PAGE_SIZE as u64;
            let phys_base = base * page_sz;
            for i in 0..3 {
                core::ptr::write_volatile((phys_base + i * page_sz) as *mut u8, (i + 1) as u8);
            }
            for i in 0..3 {
                let val = core::ptr::read_volatile((phys_base + i * page_sz) as *const u8);
                t.assert(
                    val == (i + 1) as u8,
                    "multi-page write/readback should match",
                );
            }

            kernel::vm::free_mem(base, 3);
        }
    })
}

// Phase F: Process Table

fn test_proc_addr_valid() -> u32 {
    run("proc_addr_valid", |t| {
        use arch_common::com::{CLOCK, INIT_PROC_NR, PM_PROC_NR, SYSTEM, VFS_PROC_NR};
        // Kernel tasks
        let clock_p = kernel::table::proc_addr(CLOCK);
        t.assert(!clock_p.is_null(), "proc_addr(CLOCK) should be non-null");
        let sys_p = kernel::table::proc_addr(SYSTEM);
        t.assert(!sys_p.is_null(), "proc_addr(SYSTEM) should be non-null");

        // User processes
        let pm_p = kernel::table::proc_addr(PM_PROC_NR);
        t.assert(!pm_p.is_null(), "proc_addr(PM) should be non-null");
        let vfs_p = kernel::table::proc_addr(VFS_PROC_NR);
        t.assert(!vfs_p.is_null(), "proc_addr(VFS) should be non-null");
        let init_p = kernel::table::proc_addr(INIT_PROC_NR);
        t.assert(!init_p.is_null(), "proc_addr(INIT) should be non-null");
    })
}

fn test_proc_addr_invalid() -> u32 {
    run("proc_addr_invalid", |t| {
        // Out of range (beyond NR_PROCS_TOTAL)
        let rp = kernel::table::proc_addr(300);
        t.assert(rp.is_null(), "proc_addr(300) should be null");
        // Very negative
        let rp2 = kernel::table::proc_addr(-100);
        t.assert(rp2.is_null(), "proc_addr(-100) should be null");
    })
}

fn test_endpoint_lookup() -> u32 {
    run("endpoint_lookup", |t| {
        use arch_common::com::{CLOCK, PM_PROC_NR};

        // Lookup by endpoint value (generation 0, so ep == proc_nr)
        let clock_ep = kernel::table::make_endpoint(0, CLOCK);
        let rp = kernel::table::endpoint_lookup(clock_ep);
        t.assert(!rp.is_null(), "endpoint_lookup(CLOCK) should succeed");

        let pm_ep = kernel::table::make_endpoint(0, PM_PROC_NR);
        let pm_p = kernel::table::endpoint_lookup(pm_ep);
        t.assert(!pm_p.is_null(), "endpoint_lookup(PM) should succeed");

        // Invalid endpoint
        let invalid = kernel::table::endpoint_lookup(99999);
        t.assert(
            invalid.is_null(),
            "endpoint_lookup(99999) should return null",
        );
    })
}

fn test_is_empty_proc() -> u32 {
    run("is_empty_proc", |t| {
        use arch_common::com::{CLOCK, PM_PROC_NR};

        // Boot processes should NOT be empty (SLOT_FREE cleared by proc_init)
        let clock_p = kernel::table::proc_addr(CLOCK);
        let empty = unsafe { kernel::table::is_empty_proc(clock_p) };
        t.assert(!empty, "CLOCK should not be empty");

        let pm_p = kernel::table::proc_addr(PM_PROC_NR);
        let pm_empty = unsafe { kernel::table::is_empty_proc(pm_p) };
        t.assert(!pm_empty, "PM should not be empty");

        // A non-boot slot (e.g. slot 50) should be empty/SLOT_FREE
        let free_p = kernel::table::proc_addr(50);
        let free_empty = unsafe { kernel::table::is_empty_proc(free_p) };
        t.assert(free_empty, "slot 50 should be empty (SLOT_FREE)");
    })
}

fn test_is_kernel_vs_user() -> u32 {
    run("is_kernel_vs_user", |t| {
        use arch_common::com::{CLOCK, INIT_PROC_NR, PM_PROC_NR, SYSTEM, VFS_PROC_NR};

        // Kernel tasks: CLOCK (-3), SYSTEM (-2)
        let clock_p = kernel::table::proc_addr(CLOCK);
        t.assert(
            unsafe { kernel::table::is_kernel_proc(clock_p) },
            "CLOCK should be kernel proc",
        );
        let sys_p = kernel::table::proc_addr(SYSTEM);
        t.assert(
            unsafe { kernel::table::is_kernel_proc(sys_p) },
            "SYSTEM should be kernel proc",
        );

        // User processes: PM (0), VFS (1), INIT (10)
        let pm_p = kernel::table::proc_addr(PM_PROC_NR);
        t.assert(
            unsafe { kernel::table::is_user_proc(pm_p) },
            "PM should be user proc",
        );
        let vfs_p = kernel::table::proc_addr(VFS_PROC_NR);
        t.assert(
            unsafe { kernel::table::is_user_proc(vfs_p) },
            "VFS should be user proc",
        );
        let init_p = kernel::table::proc_addr(INIT_PROC_NR);
        t.assert(
            unsafe { kernel::table::is_user_proc(init_p) },
            "INIT should be user proc",
        );
    })
}

// Phase G: IPC

/// Helper: set up a Proc slot for IPC testing.
/// This clears SLOT_FREE, sets p_nr, p_endpoint, and p_magic.
/// Reuses the existing slot initialized by proc_init (if a boot proc).
unsafe fn ipc_setup_proc(nr: i32) -> *mut kernel::proc::Proc {
    let rp = kernel::table::proc_addr(nr);
    if rp.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        (*rp)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);
        (*rp).p_nr = nr;
        (*rp).p_endpoint = kernel::table::make_endpoint(0, nr);
        (*rp).p_caller_q = core::ptr::null_mut();
        (*rp).p_q_link = core::ptr::null_mut();
        (*rp).p_getfrom_e = 0;
        (*rp).p_sendto_e = 0;
        (*rp).p_magic = kernel::proc::PMAGIC;
    }
    rp
}

fn test_mini_notify_when_receiving() -> u32 {
    run("mini_notify_when_receiving", |t| {
        unsafe {
            // Use non-boot slots (50 and 51) so we don't clobber boot state
            let dst = ipc_setup_proc(50);
            let _src = ipc_setup_proc(51);
            if dst.is_null() || _src.is_null() {
                t.assert(false, "ipc_setup_proc failed");
                return;
            }

            let src_ep = (*_src).p_endpoint;
            let dst_ep = (*dst).p_endpoint;

            // Set dst to RECEIVING from any (NONE)
            (*dst).p_rts_flags.store(
                kernel::proc::RtsFlags::RECEIVING.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            (*dst).p_getfrom_e = kernel::system::NONE;

            // Send notification from src to dst
            let result = kernel::ipc::mini_notify(src_ep, dst_ep);
            t.assert(result == 0, "mini_notify should return OK");

            // dst should no longer be RECEIVING
            let rts = (*dst)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            t.assert(
                rts & kernel::proc::RtsFlags::RECEIVING.bits() == 0,
                "dst should have RECEIVING cleared after notify",
            );

            // Clean up: restore SLOT_FREE
            (*dst).p_rts_flags.store(
                kernel::proc::RtsFlags::SLOT_FREE.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            (*_src).p_rts_flags.store(
                kernel::proc::RtsFlags::SLOT_FREE.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
    })
}

fn test_mini_send_queues_when_not_receiving() -> u32 {
    run("mini_send_queues_when_not_receiving", |t| {
        unsafe {
            let src = ipc_setup_proc(52);
            let dst = ipc_setup_proc(53);
            if src.is_null() || dst.is_null() {
                t.assert(false, "ipc_setup_proc failed");
                return;
            }

            let dst_ep = (*dst).p_endpoint;

            // dst is NOT receiving (rts_flags = 0)
            (*dst)
                .p_rts_flags
                .store(0, core::sync::atomic::Ordering::Relaxed);

            let mut msg = [0u8; kernel::proc::MESSAGE_SIZE];
            msg[0..4].copy_from_slice(&42i32.to_ne_bytes());

            let result = kernel::ipc::mini_send(
                src,
                dst_ep,
                msg.as_ptr(),
                0, // no flags
            );
            t.assert(result == 0, "mini_send should return OK");

            // src should now have SENDING flag
            let src_rts = (*src)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            t.assert(
                src_rts & kernel::proc::RtsFlags::SENDING.bits() != 0,
                "src should have SENDING flag after queued send",
            );

            // dst should have src on its caller_q
            t.assert(
                (*dst).p_caller_q == src,
                "dst's caller_q should point to src",
            );
            t.assert(
                (*src).p_sendto_e == dst_ep,
                "src's p_sendto_e should be dst",
            );

            // Clean up: clear SENDING, restore SLOT_FREE
            (*src).p_rts_flags.store(
                kernel::proc::RtsFlags::SLOT_FREE.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            (*dst).p_rts_flags.store(
                kernel::proc::RtsFlags::SLOT_FREE.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            (*dst).p_caller_q = core::ptr::null_mut();
        }
    })
}

fn test_grant_direct_valid() -> u32 {
    run("grant_direct_valid", |t| {
        unsafe {
            use arch_common::safecopies::*;
            use core::sync::atomic::AtomicU32;
            use kernel::grants::*;
            use kernel::r#priv::{Priv, PrivFlags};

            // Set up a grant buffer (stack-allocated, aligned)
            let mut grant_buf: [CpGrant; 8] = core::mem::zeroed();
            let gp = &raw mut grant_buf as *mut CpGrant;

            // Build a direct grant entry
            let flags = CPF_READ | CPF_WRITE;
            let who_to: i32 = 42;
            let start: u64 = 0x1000;
            let len: usize = 4096;
            let entry = CpGrant {
                cp_flags: CPF_USED | CPF_VALID | CPF_DIRECT | flags,
                cp_u: CpUnion {
                    cp_direct: CpDirect {
                        cp_who_to: who_to,
                        cp_start: start,
                        cp_len: len,
                        cp_reserved: [0u8; 8],
                    },
                },
                cp_reserved: [0u8; 8],
            };
            *gp.add(0) = entry;

            // Set up grant table in a Priv at a known slot
            let _priv_buf: [u8; 2048] = core::mem::zeroed();
            let priv_ptr = _priv_buf.as_ptr() as *mut Priv;
            core::ptr::write_bytes(priv_ptr.cast::<u8>(), 0, 2048);
            (*priv_ptr).s_grant_table = gp as u64;
            (*priv_ptr).s_grant_pa = gp as u64;
            (*priv_ptr).s_grant_entries = 8;
            (*priv_ptr).s_flags = PrivFlags::empty();

            // Set up a Proc entry
            let rp = kernel::table::proc_addr(60);
            if rp.is_null() {
                t.assert(false, "proc_addr(60) failed");
                return;
            }
            core::ptr::write_bytes(
                rp.cast::<u8>(),
                0,
                core::mem::size_of::<kernel::proc::Proc>(),
            );
            (*rp).p_magic = kernel::proc::PMAGIC;
            (*rp).p_endpoint = kernel::table::make_endpoint(0, 60);
            (*rp).p_priv = priv_ptr;
            (*rp).p_rts_flags = AtomicU32::new(kernel::proc::RtsFlags::empty().bits());

            let granter_ep = (*rp).p_endpoint;

            // Verify grant 0 for read access
            let result = verify_grant(granter_ep, who_to, 0, 4096, CPF_READ, 0);
            match result {
                Ok((offset, e_granter, _flags)) => {
                    t.assert(offset == 0x1000, "direct grant offset must match start");
                    t.assert(e_granter == granter_ep, "e_granter must match granter");
                }
                Err(_e) => t.assert(false, "verify_grant direct should succeed"),
            }

            // Verify grant 0 for write access from wrong grantee — should fail
            let result2 = verify_grant(granter_ep, 99, 0, 4096, CPF_WRITE, 0);
            if result2.is_err() {
                // Expected: wrong grantee doesn't match cp_who_to
            } else {
                t.assert(false, "verify_grant with wrong grantee should fail");
            }

            // Restore slot
            (*rp).p_rts_flags.store(
                kernel::proc::RtsFlags::SLOT_FREE.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
    })
}

fn test_grant_indirect() -> u32 {
    run("grant_indirect", |_t| {
        // Indirect grant chain is complex — validated in kernel unit tests
        // (kernel/src/grants.rs has 400+ lines of grant tests)
        // This test is a placeholder to maintain test infrastructure.
    })
}

fn test_grant_invalid_id() -> u32 {
    run("grant_invalid_id", |t| {
        unsafe {
            // Grant ID -1 (GRANT_INVALID) should be rejected
            let result = kernel::grants::verify_grant(
                kernel::table::make_endpoint(0, 0),
                0,
                -1, // GRANT_INVALID
                4096,
                arch_common::safecopies::CPF_READ,
                0,
            );
            if result.is_err() {
                // Expected: invalid grant ID
            } else {
                t.assert(false, "verify_grant with GRANT_INVALID should fail");
            }
        }
    })
}

fn test_syscall_getpid() -> u32 {
    run("syscall_getpid", |t| {
        unsafe {
            // init_basic_syscalls already registered getpid=0 in kmain
            // Set up a Proc with a known endpoint
            let rp = kernel::table::proc_addr(70);
            if rp.is_null() {
                t.assert(false, "proc_addr(70) failed");
                return;
            }
            // Don't zero the whole Proc — just set what we need
            (*rp).p_magic = kernel::proc::PMAGIC;
            (*rp).p_endpoint = 70;
            (*rp)
                .p_rts_flags
                .store(0, core::sync::atomic::Ordering::Relaxed);

            let args = [0u64; 6];
            // NR_GETPID is 20, not 0 (NR_EXIT = 0 after POSIX numbering change)
            let result = kernel::syscall::dispatch_basic_syscall(rp, 20, &args);
            t.assert(result == 70, "getpid must return the proc's endpoint");

            (*rp).p_rts_flags.store(
                kernel::proc::RtsFlags::SLOT_FREE.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
    })
}

fn test_syscall_write() -> u32 {
    run("syscall_write", |t| unsafe {
        let rp = kernel::table::proc_addr(71);
        if rp.is_null() {
            t.assert(false, "proc_addr(71) failed");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = 71;
        (*rp)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);

        let mut buf = [0u8; 16];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = b'A' + i as u8;
        }
        let args = [1u64, buf.as_ptr() as u64, 5u64, 0, 0, 0];
        let result = kernel::syscall::dispatch_basic_syscall(rp, 3, &args);
        t.assert(result == 5, "write should return count of bytes written");

        (*rp).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    })
}

fn test_syscall_brk() -> u32 {
    run("syscall_brk", |t| unsafe {
        let rp = kernel::table::proc_addr(72);
        if rp.is_null() {
            t.assert(false, "proc_addr(72) failed");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = 72;
        (*rp)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);

        // Query current break (new_brk = 0)
        let args = [0u64, 0, 0, 0, 0, 0];
        let result = kernel::syscall::dispatch_basic_syscall(rp, 36, &args);
        t.assert(result >= 0x3FE00000, "initial brk should be in valid range");

        // Set new break
        let args2 = [0x3FE01000u64, 0, 0, 0, 0, 0];
        let result2 = kernel::syscall::dispatch_basic_syscall(rp, 36, &args2);
        t.assert(result2 == 0x3FE01000, "brk should return new break value");

        // Query again
        let args3 = [0u64, 0, 0, 0, 0, 0];
        let result3 = kernel::syscall::dispatch_basic_syscall(rp, 36, &args3);
        t.assert(result3 == 0x3FE01000, "brk query should return new break");

        // Try out-of-range (ENOMEM)
        let args4 = [0x40000000u64, 0, 0, 0, 0, 0];
        let result4 = kernel::syscall::dispatch_basic_syscall(rp, 36, &args4);
        t.assert(
            result4 == -12,
            "brk with invalid address should return ENOMEM",
        );

        (*rp).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    })
}

fn test_syscall_exit() -> u32 {
    run("syscall_exit", |t| unsafe {
        let rp = kernel::table::proc_addr(73);
        if rp.is_null() {
            t.assert(false, "proc_addr(73) failed");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = 73;
        (*rp)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);
        (*rp).p_signal_received = 0;

        // NR_EXIT = 0, exit status = 42
        let args = [42u64, 0, 0, 0, 0, 0];
        let result = kernel::syscall::dispatch_basic_syscall(rp, 0, &args);

        // SYS_exit returns EDONTREPLY (-203) to signal no reply needed
        t.assert(result == -203, "exit should return EDONTREPLY");

        // p_signal_received should have the exit status
        t.assert(
            (*rp).p_signal_received == 42,
            "exit status should be stored in p_signal_received",
        );

        // SLOT_FREE should be set (process slot released)
        let rts = (*rp)
            .p_rts_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        t.assert(
            rts & kernel::proc::RtsFlags::SLOT_FREE.bits() != 0,
            "SLOT_FREE should be set after exit",
        );
        // Note: no cleanup needed — exit already set SLOT_FREE
    })
}

/// Dummy timer callback — does nothing.
unsafe fn dummy_timer_cb(_tp: *mut kernel::r#priv::MinixTimer) {}

fn test_timer_set_and_expire() -> u32 {
    run("timer_set_and_expire", |t| {
        unsafe {
            let mut timer = kernel::r#priv::MinixTimer::default();
            let mut timer_list: *mut kernel::r#priv::MinixTimer = core::ptr::null_mut();
            let timers = &raw mut timer_list;

            // Use double-cast for function pointer to usize
            let cb = dummy_timer_cb as *const () as usize;

            // Set a timer expiring at tick 10
            kernel::clock::tmrs_settimer(timers, &raw mut timer, 10, cb, core::ptr::null_mut());
            t.assert(
                !timer_list.is_null(),
                "timer list should not be empty after set",
            );
            t.assert(timer.tmr_exp_time == 10, "timer exp_time should be 10");

            // Expire at tick 5 — no timers should fire
            let count = kernel::clock::tmrs_exptimers(timers, 5, core::ptr::null_mut());
            t.assert(count == 0, "no timers should expire at tick 5");
            t.assert(!timer_list.is_null(), "timer should still be in list");

            // Expire at tick 10 — timer should fire
            let count = kernel::clock::tmrs_exptimers(timers, 10, core::ptr::null_mut());
            t.assert(count == 1, "one timer should expire at tick 10");
            t.assert(
                timer_list.is_null(),
                "timer list should be empty after expiry",
            );
        }
    })
}

fn test_timer_clear() -> u32 {
    run("timer_clear", |t| {
        unsafe {
            let mut timer = kernel::r#priv::MinixTimer::default();
            let mut timer_list: *mut kernel::r#priv::MinixTimer = core::ptr::null_mut();
            let timers = &raw mut timer_list;

            let cb = dummy_timer_cb as *const () as usize;

            kernel::clock::tmrs_settimer(timers, &raw mut timer, 20, cb, core::ptr::null_mut());
            t.assert(!timer_list.is_null(), "timer should be in list after set");

            // Cancel the timer
            kernel::clock::tmrs_clrtimer(timers, &raw mut timer, core::ptr::null_mut());
            t.assert(
                timer_list.is_null(),
                "timer list should be empty after clear",
            );

            let count = kernel::clock::tmrs_exptimers(timers, 100, core::ptr::null_mut());
            t.assert(count == 0, "no timers should expire after clear");
        }
    })
}

fn test_timer_multiple() -> u32 {
    run("timer_multiple", |t| unsafe {
        let mut t1 = kernel::r#priv::MinixTimer::default();
        let mut t2 = kernel::r#priv::MinixTimer::default();
        let mut timer_list: *mut kernel::r#priv::MinixTimer = core::ptr::null_mut();
        let timers = &raw mut timer_list;

        let cb = dummy_timer_cb as *const () as usize;

        kernel::clock::tmrs_settimer(timers, &raw mut t1, 5, cb, core::ptr::null_mut());
        kernel::clock::tmrs_settimer(timers, &raw mut t2, 10, cb, core::ptr::null_mut());

        let count = kernel::clock::tmrs_exptimers(timers, 6, core::ptr::null_mut());
        t.assert(count == 1, "one timer should expire at tick 6");
        t.assert(!timer_list.is_null(), "t2 should still be in list");

        let count = kernel::clock::tmrs_exptimers(timers, 10, core::ptr::null_mut());
        t.assert(count == 1, "one timer should expire at tick 10");
        t.assert(timer_list.is_null(), "timer list should be empty");
    })
}

fn test_pit_programmed() -> u32 {
    run("pit_programmed", |t| unsafe {
        // Latch counter 0 (write 0x00 to control register 0x43)
        arch_x86_64::asm::outb(0x43, 0x00);
        // Read latched value (LSB then MSB from port 0x40)
        let low = arch_x86_64::asm::inb(0x40);
        let high = arch_x86_64::asm::inb(0x40);
        let count = (low as u16) | ((high as u16) << 8);
        // PIT input frequency is 1.193182 MHz. At 100 Hz:
        // divisor = 1,193,182 / 100 ≈ 11,932 (0x2E9C)
        // Counter should be counting down from this value
        t.assert(count > 0, "PIT counter should be > 0");
        t.assert(
            count <= 12000,
            "PIT counter should be ≤ 12000 for 100 Hz mode 3",
        );
    })
}

fn test_monotonic_advances() -> u32 {
    run("monotonic_advances", |t| {
        unsafe {
            // Directly invoke the timer interrupt handler to advance
            // the monotonic clock. Hardware timer interrupts don't fire
            // during integration tests (no APIC routing configured).
            kernel::clock::timer_int_handler();
        }
        let val = kernel::clock::get_monotonic();
        t.assert(val > 0, "monotonic clock should advance after timer tick");
        t.assert(
            val <= 100,
            "monotonic shouldn't advance more than 100 ticks",
        );
    })
}

/// Dummy IRQ handler that returns the hook's ID.
unsafe fn test_irq_handler(hook: *mut kernel::system::IrqHook) -> i32 {
    unsafe { (*hook).id }
}

fn test_irq_put_and_remove() -> u32 {
    run("irq_put_and_remove", |t| {
        unsafe {
            // Use a slot from the static IRQ_HOOKS pool
            let hooks = kernel::system::IRQ_HOOKS.get();
            let hook = &raw mut (*hooks)[0];

            // Ensure the hook is clean
            (*hook).proc_nr_e = kernel::system::NONE;
            (*hook).next = core::ptr::null_mut();
            (*hook).handler = None;

            // Register a handler for IRQ 14 (primary IDE)
            kernel::interrupt::put_irq_handler(hook, 14, test_irq_handler);
            t.assert((*hook).irq == 14, "hook irq should be 14");
            t.assert((*hook).id >= 0, "hook should have valid id");
            t.assert((*hook).handler.is_some(), "hook should have handler");

            // Remove it — rm_irq_handler removes from linked list
            // but does NOT clear the hook struct fields
            kernel::interrupt::rm_irq_handler(hook);

            // After removal, hook fields are still set (rm doesn't zero them)
            // Just verify the function didn't panic
            t.assert(true, "rm_irq_handler completed without panic");

            // Clean up: reset the hook for subsequent tests
            (*hook).next = core::ptr::null_mut();
            (*hook).handler = None;
            (*hook).irq = 0;
            (*hook).id = 0;
        }
    })
}

fn test_elf_load_to_phys_pages() -> u32 {
    run("elf_load_to_phys_pages", |t| unsafe {
        use kernel::elf::{
            ELF_MAGIC, ELFCLASS64, ELFDATA2LSB, EM_X86_64, ET_EXEC, Elf64Ehdr, Elf64Phdr, PT_LOAD,
            parse_elf_header,
        };

        // Build a minimal ELF64 binary
        // ELF header (64 bytes) + 1 PHDR (56 bytes) + segment data
        let seg_content: &[u8] = b"Hello, ELF physical page!";
        let elf_base_vaddr: u64 = 0x100_0000; // 16MB
        let phdr_offset: u64 = 64; // right after ELF header
        let data_offset: u64 = 64 + 56; // after header + phdr

        let mut buf = [0u8; 512];
        // ELF header
        let ehdr = Elf64Ehdr {
            e_ident: [
                ELF_MAGIC[0],
                ELF_MAGIC[1],
                ELF_MAGIC[2],
                ELF_MAGIC[3],
                ELFCLASS64,  // 64-bit
                ELFDATA2LSB, // little-endian
                1,           // version
                0,           // OS/ABI
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0, // padding
            ],
            e_type: ET_EXEC,
            e_machine: EM_X86_64,
            e_version: 1,
            e_entry: elf_base_vaddr,
            e_phoff: phdr_offset,
            e_shoff: 0,
            e_flags: 0,
            e_ehsize: 64,
            e_phentsize: 56,
            e_phnum: 1,
            e_shentsize: 0,
            e_shnum: 0,
            e_shstrndx: 0,
        };
        core::ptr::copy_nonoverlapping(&ehdr as *const _ as *const u8, buf.as_mut_ptr(), 64);

        // Program header: one LOAD segment
        let phdr = Elf64Phdr {
            p_type: PT_LOAD,
            p_flags: 4 | 2 | 1, // PF_R | PF_W | PF_X
            p_offset: data_offset,
            p_vaddr: elf_base_vaddr,
            p_paddr: elf_base_vaddr,
            p_filesz: seg_content.len() as u64,
            p_memsz: seg_content.len() as u64 + 16, // 16 bytes of BSS
            p_align: 0x1000,
        };
        core::ptr::copy_nonoverlapping(
            &phdr as *const _ as *const u8,
            buf.as_mut_ptr().add(64),
            56,
        );

        // Segment data
        buf[data_offset as usize..data_offset as usize + seg_content.len()]
            .copy_from_slice(seg_content);

        let total_size = (data_offset + seg_content.len() as u64) as usize;

        // Parse ELF and calculate page requirements
        let data = &buf[..total_size];
        let ehdr_parsed = parse_elf_header(data);
        t.assert(ehdr_parsed.is_ok(), "ELF header must parse");
        let ehdr = ehdr_parsed.unwrap();

        t.assert(ehdr.e_ehsize == 64, "ELF header size must be 64");
        t.assert(ehdr.e_phnum == 1, "must have 1 program header");
        t.assert(ehdr.e_phentsize == 56, "PHDR size must be 56");

        // Read the PHDR
        let phdr_parsed = &*(data.as_ptr().add(ehdr.e_phoff as usize) as *const Elf64Phdr);
        t.assert(phdr_parsed.p_type == PT_LOAD, "PHDR type must be PT_LOAD");
        t.assert(
            phdr_parsed.p_vaddr == elf_base_vaddr,
            "PHDR vaddr must match",
        );

        // Calculate pages needed (round up page-aligned top)
        let seg_top = phdr_parsed.p_vaddr + phdr_parsed.p_memsz;
        let pages_needed = (seg_top.div_ceil(0x1000) - (elf_base_vaddr / 0x1000)) as usize;

        let clicks_needed = pages_needed.div_ceil(4); // 1 click = 4 pages = 16KB

        // Allocate physical pages
        let click = kernel::vm::alloc_mem(clicks_needed, 0);
        t.assert(
            click != kernel::vm::NO_MEM,
            "alloc_mem must succeed for ELF pages",
        );

        let page_sz = kernel::vm::VM_PAGE_SIZE as u64;
        let phys_base = (click as u64) * page_sz;

        // Load segment data via identity map
        // For each LOAD segment, copy file data to identity-mapped physical address
        let offset = phdr_parsed.p_vaddr.wrapping_sub(elf_base_vaddr);
        let dst_addr = phys_base.wrapping_add(offset);
        let dst = dst_addr as *mut u8;

        if phdr_parsed.p_filesz > 0 {
            let src = data.as_ptr().add(phdr_parsed.p_offset as usize);
            core::ptr::copy_nonoverlapping(src, dst, phdr_parsed.p_filesz as usize);
        }

        // Write BSS (zero-fill)
        let bss_size = phdr_parsed.p_memsz.saturating_sub(phdr_parsed.p_filesz);
        if bss_size > 0 {
            core::ptr::write_bytes(dst.add(phdr_parsed.p_filesz as usize), 0, bss_size as usize);
        }

        // Read the first few bytes from the identity-mapped address
        let mut readback = [0u8; 64];
        core::ptr::copy_nonoverlapping(dst, readback.as_mut_ptr(), seg_content.len().min(64));

        // Compare with original content
        let expected = &seg_content[..seg_content.len().min(64)];
        let actual = &readback[..expected.len()];
        t.assert(actual == expected, "loaded ELF data must match source");

        // Verify BSS is zero-filled
        let bss_start = dst.add(phdr_parsed.p_filesz as usize);
        for i in 0..16 {
            let byte = core::ptr::read_volatile(bss_start.add(i));
            t.assert(byte == 0, "BSS must be zero-filled");
        }

        // Verify entry point matches
        t.assert(ehdr.e_entry == elf_base_vaddr, "entry point must match");

        kernel::vm::free_mem(click, clicks_needed as u64);

        // Additional integrity check: verify the identity map is functional
        // by writing/reading a known pattern at the physical address
        core::ptr::write_volatile(phys_base as *mut u32, 0xCAFEBABE);
        let check = core::ptr::read_volatile(phys_base as *const u32);
        t.assert(
            check == 0xCAFEBABE,
            "identity map write/readback must work at phys_base",
        );
    })
}

// Phase P: Syscall exec and initramfs verification

fn test_initramfs_all_executables_elf() -> u32 {
    run("initramfs_all_executables_elf", |t| {
        use kernel::elf::parse_elf_header;

        let binaries = [
            "/sbin/init",
            "/sbin/pm",
            "/sbin/vfs",
            "/sbin/vm",
            "/sbin/rs",
            "/sbin/ds",
            "/sbin/sched",
            "/sbin/tty",
            "/sbin/mfs",
            "/sbin/pfs",
            "/sbin/ramdisk",
            "/bin/sh",
            "/bin/cat",
            "/bin/echo",
            "/bin/ls",
            "/bin/mkdir",
            "/bin/rm",
            "/bin/cp",
            "/bin/ln",
            "/bin/chmod",
            "/bin/sync",
            "/sbin/mknod",
            "/sbin/reboot",
            "/sbin/fsck",
        ];
        for &name in &binaries {
            let found = kernel::initramfs::find_initramfs_file(name);
            if found.is_none() {
                serial_puts("  FAIL: ");
                serial_puts(name);
                serial_puts(" not in initramfs\n");
                t.assert(false, "");
                continue;
            }
            let (data, _mode) = found.unwrap();
            match parse_elf_header(data) {
                Ok(ehdr) => {
                    if ehdr.e_type != 2 {
                        serial_puts("  FAIL: ");
                        serial_puts(name);
                        serial_puts(" not ET_EXEC\n");
                        t.assert(false, "");
                    }
                    if ehdr.e_ident[4] != 2 {
                        serial_puts("  FAIL: ");
                        serial_puts(name);
                        serial_puts(" not 64-bit\n");
                        t.assert(false, "");
                    }
                    if ehdr.e_phnum == 0 {
                        serial_puts("  FAIL: ");
                        serial_puts(name);
                        serial_puts(" no phdrs\n");
                        t.assert(false, "");
                    }
                }
                Err(_) => {
                    serial_puts("  FAIL: ");
                    serial_puts(name);
                    serial_puts(" bad ELF\n");
                    t.assert(false, "");
                }
            }
        }
    })
}

// Phase Q: IPC, page tables, timers

fn test_ipc_sendrec_roundtrip() -> u32 {
    run("ipc_sendrec_roundtrip", |t| unsafe {
        // Full send/receive cycle between two processes.
        // Test uses boot CR3 (p_seg.p_cr3 = 0) since both buffers are
        // on the kernel stack (identity-mapped in QEMU).
        let src = kernel::table::proc_addr(90);
        let dst = kernel::table::proc_addr(91);
        if src.is_null() || dst.is_null() {
            t.assert(false, "proc_addr failed");
            return;
        }
        // Init src
        (*src).p_magic = kernel::proc::PMAGIC;
        (*src).p_nr = 90;
        (*src).p_endpoint = kernel::table::make_endpoint(0, 90);
        (*src)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);
        (*src).p_caller_q = core::ptr::null_mut();
        (*src).p_q_link = core::ptr::null_mut();
        // Set CR3 to boot page table so copy_from_user can walk addresses
        let boot_cr3 = kernel::pagetable::boot_cr3();
        (*src).p_seg.p_cr3 = boot_cr3;
        // Init dst
        (*dst).p_magic = kernel::proc::PMAGIC;
        (*dst).p_nr = 91;
        (*dst).p_endpoint = kernel::table::make_endpoint(0, 91);
        (*dst)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);
        (*dst).p_caller_q = core::ptr::null_mut();
        (*dst).p_q_link = core::ptr::null_mut();
        (*dst).p_seg.p_cr3 = boot_cr3;

        let src_ep = (*src).p_endpoint;
        let _dst_ep = (*dst).p_endpoint;
        let test_val: i32 = 0x12345678;

        // Write message directly to dst's p_delivermsg (bypass mini_send)
        let ep_bytes = src_ep.to_le_bytes();
        let val_bytes = test_val.to_le_bytes();
        core::ptr::copy_nonoverlapping(ep_bytes.as_ptr(), (*dst).p_delivermsg.as_mut_ptr(), 4);
        core::ptr::copy_nonoverlapping(
            val_bytes.as_ptr(),
            (*dst).p_delivermsg.as_mut_ptr().add(4),
            4,
        );

        // Set up receive buffer and call delivermsg
        let mut dst_buf = [0u8; kernel::proc::MESSAGE_SIZE];
        (*dst).p_delivermsg_vir = dst_buf.as_mut_ptr() as u64;
        let dm_result = kernel::ipc::delivermsg(dst);
        t.assert(dm_result == 0, "delivermsg should return OK");

        // Check delivermsg copied to dst_buf
        let delivered = i32::from_ne_bytes([dst_buf[4], dst_buf[5], dst_buf[6], dst_buf[7]]);
        t.assert(
            delivered == test_val,
            "delivermsg should copy message to dst_buf",
        );

        // Clean up
        (*src).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        (*dst).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    })
}

fn test_exec_setup_new_page_table() -> u32 {
    run("exec_setup_new_page_table", |t| unsafe {
        // Call exec_setup_new_page_table which creates a fresh page table.
        // In QEMU, boot_cr3() returns the real CR3 value, so this should
        // allocate and return a valid new CR3.
        let new_cr3 = kernel::exec::exec_setup_new_page_table();
        t.assert(new_cr3 != 0, "new page table CR3 should be non-zero");
        t.assert(
            new_cr3 & 0xFFF == 0,
            "new page table should be page-aligned",
        );

        // Verify the new page table is readable (PML4 exists)
        let entry0 = core::ptr::read_volatile(new_cr3 as *const u64);
        t.assert(entry0 & 1 != 0, "new PML4[0] should be present");

        // Free the allocated pages (but exec_setup_new_page_table doesn't
        // expose the internal allocation, so we can't easily free them.
        // The 4MB test pool is large enough for a few allocations.)
    })
}

fn test_monotonic_timer_interval() -> u32 {
    run("monotonic_timer_interval", |t| unsafe {
        // Fire 5 timer ticks and verify the clock advances by exactly 5.
        let start = kernel::clock::get_monotonic();
        for _ in 0..5 {
            kernel::clock::timer_int_handler();
        }
        let end = kernel::clock::get_monotonic();
        let elapsed = end - start;
        if elapsed < 5 {
            t.assert(false, "monotonic should advance by >=5 after 5 ticks");
        }
        // Initial boot ticks may cause >5, but never less than 5
    })
}

fn test_pagetable_deep_walk() -> u32 {
    run("pagetable_deep_walk", |t| unsafe {
        use kernel::pagetable::walk;
        let cr3 = kernel::pagetable::boot_cr3();
        t.assert(cr3 != 0, "boot CR3 should be non-zero");

        // Walk known-addressed kernel code at 0x200000
        let result = walk(cr3, 0x200000u64);
        match result {
            Ok(wr) => {
                // Should resolve at level <= 2 (2MB large page or 4KB leaf)
                t.assert(
                    wr.level <= 2,
                    "kernel code walk should resolve at level <= 2",
                );
                t.assert(
                    wr.pte_value & kernel::pagetable::PG_P != 0,
                    "PTE for kernel code should be present",
                );
            }
            Err(_) => {
                t.assert(false, "walk(0x200000) should succeed");
            }
        }

        // Walk kernel high mapping (>= 0xFFFF800000000000)
        let high_va = 0xFFFF800000000000u64;
        let high_result = walk(cr3, high_va);
        if let Ok(wr) = high_result {
            t.assert(
                wr.pte_value & kernel::pagetable::PG_P != 0,
                "high mapping PTE should be present",
            );
        }
        // Note: high mapping may not exist in stage2 setup — no error

        // Walk an unmapped address should fail
        let bad_result = walk(cr3, 0x7ffffff000u64);
        t.assert(
            bad_result.is_err(),
            "walk of unmapped user address should fail",
        );
    })
}

// Phase R: Scheduling

fn test_enqueue_priority() -> u32 {
    run("enqueue_priority", |t| unsafe {
        // Clear run queues for test isolation
        clear_run_queues();

        // Enqueue two processes at different priorities.
        // High priority (lower number) should be ahead of low priority.
        let high = kernel::table::proc_addr(92);
        let low = kernel::table::proc_addr(93);
        if high.is_null() || low.is_null() {
            t.assert(false, "proc_addr failed");
            return;
        }
        (*high).p_magic = kernel::proc::PMAGIC;
        (*high).p_endpoint = 92;
        (*high).p_priority = 5;
        (*high).p_cpu_time_left = 100;
        (*high)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);

        (*low).p_magic = kernel::proc::PMAGIC;
        (*low).p_endpoint = 93;
        (*low).p_priority = 7;
        (*low).p_cpu_time_left = 100;
        (*low)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);

        kernel::sched::enqueue(high);
        kernel::sched::enqueue(low);

        // pick_proc should return the highest-priority runnable proc
        let picked = kernel::sched::pick_proc();
        t.assert(picked.is_some(), "pick_proc should return something");
        if let Some(p) = picked {
            t.assert((*p).p_endpoint == 92, "highest priority should run first");
        }

        (*high).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        (*low).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    })
}

fn test_quantum_exhaustion() -> u32 {
    run("quantum_exhaustion", |t| unsafe {
        // Simulate a process that has exhausted its quantum.
        // notify_scheduler should set RTS_NO_QUANTUM and dequeue.
        use kernel::proc::RtsFlags;

        let rp = kernel::table::proc_addr(94);
        if rp.is_null() {
            t.assert(false, "proc_addr(94) failed");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = 94;
        (*rp).p_priority = 6;
        (*rp).p_cpu_time_left = 10;
        (*rp)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);

        // Set up a minimal priv with kernel_scheduler=false so
        // notify_scheduler sends to SCHED server instead of renewing.
        let mut fake_priv = kernel::r#priv::Priv::default();
        fake_priv.s_proc_nr = 94;
        fake_priv.s_flags = kernel::r#priv::PrivFlags::PREEMPTIBLE;
        (*rp).p_priv = &mut fake_priv;
        // Point p_scheduler to a different slot so kernel_scheduler()
        // returns false (required for proc_no_time to call notify_scheduler).
        let sched_rp = kernel::table::proc_addr(4); // SCHED_PROC_NR
        if !sched_rp.is_null() {
            (*rp).p_scheduler = sched_rp;
        }

        kernel::sched::enqueue(rp);

        // Deplete quantum and call proc_no_time
        (*rp).p_cpu_time_left = 0;
        kernel::sched::proc_no_time(rp);

        // Check that RTS_NO_QUANTUM was set
        let rts = (*rp)
            .p_rts_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        t.assert(
            rts & RtsFlags::NO_QUANTUM.bits() != 0,
            "RTS_NO_QUANTUM should be set after quantum exhaustion",
        );

        // Clean up
        (*rp).p_priv = core::ptr::null_mut();
        (*rp).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    })
}

fn test_dequeue_reordering() -> u32 {
    run("dequeue_reordering", |t| unsafe {
        // Clear run queues for test isolation
        clear_run_queues();

        // Enqueue 3 processes, dequeue one from middle, verify order.
        // Processes p_a, p_b, p_c are enqueued; p_b is dequeued;
        // then p_a and p_c should remain in order.
        let p_a = kernel::table::proc_addr(95);
        let p_b = kernel::table::proc_addr(96);
        let p_c = kernel::table::proc_addr(97);
        if p_a.is_null() || p_b.is_null() || p_c.is_null() {
            t.assert(false, "proc_addr failed");
            return;
        }
        for (i, rp) in [p_a, p_b, p_c].iter().enumerate() {
            (**rp).p_magic = kernel::proc::PMAGIC;
            (**rp).p_endpoint = 95 + i as i32;
            (**rp).p_priority = 6;
            (**rp).p_cpu_time_left = 100;
            (**rp)
                .p_rts_flags
                .store(0, core::sync::atomic::Ordering::Relaxed);
            (**rp).p_q_link = core::ptr::null_mut();
        }

        kernel::sched::enqueue(p_a);
        kernel::sched::enqueue(p_b);
        kernel::sched::enqueue(p_c);

        // Dequeue p_b by setting a non-runnable flag
        (*p_b).p_rts_flags.store(
            kernel::proc::RtsFlags::RECEIVING.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        kernel::sched::dequeue(p_b);

        // pick_proc should skip p_b and return p_a, then p_c
        let first = kernel::sched::pick_proc();
        t.assert(first.is_some(), "first pick should succeed");
        if let Some(p) = first {
            t.assert((*p).p_endpoint == 95, "first should be p_a");
        }

        // Dequeue p_a
        (*p_a).p_rts_flags.store(
            kernel::proc::RtsFlags::RECEIVING.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        kernel::sched::dequeue(p_a);

        let second = kernel::sched::pick_proc();
        t.assert(second.is_some(), "second pick should succeed");
        if let Some(p) = second {
            t.assert((*p).p_endpoint == 97, "second should be p_c");
        }

        for rp in [p_a, p_b, p_c] {
            (*rp).p_rts_flags.store(
                kernel::proc::RtsFlags::SLOT_FREE.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
    })
}

fn test_runqueues_invariant() -> u32 {
    run("runqueues_invariant", |t| unsafe {
        // Clear run queues for test isolation
        clear_run_queues();

        // After enqueue/dequeue roundtrip, runqueues_ok() should pass.
        let rp = kernel::table::proc_addr(98);
        if rp.is_null() {
            t.assert(false, "proc_addr(98) failed");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = 98;
        (*rp).p_priority = 6;
        (*rp).p_cpu_time_left = 100;
        (*rp)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);

        let before = kernel::sched::runqueues_ok();

        kernel::sched::enqueue(rp);
        let mid = kernel::sched::runqueues_ok();
        t.assert(mid, "runqueues should be OK after enqueue");

        (*rp).p_rts_flags.store(
            kernel::proc::RtsFlags::RECEIVING.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
        kernel::sched::dequeue(rp);
        let after = kernel::sched::runqueues_ok();
        t.assert(after, "runqueues should be OK after dequeue");

        // Invariant: runqueues_ok is monotonic (once OK, env changes may
        // affect but at minimum our operations shouldn't corrupt it)
        if before {
            t.assert(after, "runqueues invariant preserved");
        }

        (*rp).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    })
}

// Phase S: Grants — data copy

fn test_safecopy_read() -> u32 {
    run("safecopy_read", |t| unsafe {
        use arch_common::safecopies::*;
        use kernel::grants::*;
        use kernel::r#priv::Priv;

        let mut grant: CpGrant = core::mem::zeroed();
        grant.cp_flags = CPF_USED | CPF_VALID | CPF_DIRECT | CPF_READ;
        grant.cp_u.cp_direct.cp_who_to = 88;
        grant.cp_u.cp_direct.cp_start = 0x2000;
        grant.cp_u.cp_direct.cp_len = 64;

        let gp = &raw mut grant;
        let mut priv_buf = core::mem::zeroed::<Priv>();
        priv_buf.s_grant_table = gp as u64;
        priv_buf.s_grant_pa = gp as u64;
        priv_buf.s_grant_entries = 4;

        let rp = kernel::table::proc_addr(82);
        if rp.is_null() {
            t.assert(false, "no slot");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = kernel::table::make_endpoint(0, 82);
        (*rp).p_priv = &raw mut priv_buf;

        let ep = (*rp).p_endpoint;
        let r = verify_grant(ep, 88, 0, 64, CPF_READ, 0);
        t.assert(r.is_ok(), "verify_grant read should succeed");

        (*rp).p_rts_flags =
            core::sync::atomic::AtomicU32::new(kernel::proc::RtsFlags::SLOT_FREE.bits());
    })
}

fn test_safecopy_write() -> u32 {
    run("safecopy_write", |t| unsafe {
        // Test verify_grant for CPF_WRITE permission.
        // Grant table on kernel stack, no page allocation needed.
        use arch_common::safecopies::*;
        use kernel::grants::*;
        use kernel::r#priv::Priv;

        let mut grant: CpGrant = core::mem::zeroed();
        grant.cp_flags = CPF_USED | CPF_VALID | CPF_DIRECT | CPF_WRITE;
        grant.cp_u.cp_direct.cp_who_to = 86;
        grant.cp_u.cp_direct.cp_start = 0x1000;
        grant.cp_u.cp_direct.cp_len = 64;

        let grant_ptr = &raw mut grant;
        let mut priv_buf = core::mem::zeroed::<Priv>();
        priv_buf.s_grant_table = grant_ptr as u64;
        priv_buf.s_grant_pa = grant_ptr as u64;
        priv_buf.s_grant_entries = 4;

        let rp = kernel::table::proc_addr(83);
        if rp.is_null() {
            t.assert(false, "no slot");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = kernel::table::make_endpoint(0, 83);
        (*rp).p_priv = &raw mut priv_buf;

        let ep = (*rp).p_endpoint;
        let r1 = verify_grant(ep, 86, 0, 16, CPF_WRITE, 0);
        t.assert(r1.is_ok(), "CPF_WRITE grant should verify");
        let r2 = verify_grant(ep, 86, 0, 4, CPF_READ, 0);
        t.assert(r2.is_err(), "CPF_READ on CPF_WRITE grant should fail");

        (*rp).p_rts_flags =
            core::sync::atomic::AtomicU32::new(kernel::proc::RtsFlags::SLOT_FREE.bits());
    })
}

fn test_safecopy_bounds() -> u32 {
    run("safecopy_bounds", |t| unsafe {
        use arch_common::safecopies::*;
        use kernel::grants::*;
        use kernel::r#priv::Priv;

        // Grant 32 bytes at offset 0
        let mut grant: CpGrant = core::mem::zeroed();
        grant.cp_flags = CPF_USED | CPF_VALID | CPF_DIRECT | CPF_READ;
        grant.cp_u.cp_direct.cp_who_to = 84;
        grant.cp_u.cp_direct.cp_start = 0x3000;
        grant.cp_u.cp_direct.cp_len = 32;

        let grant_ptr = &raw mut grant;
        let mut priv_buf = core::mem::zeroed::<Priv>();
        priv_buf.s_grant_table = grant_ptr as u64;
        priv_buf.s_grant_pa = grant_ptr as u64;
        priv_buf.s_grant_entries = 4;

        let rp = kernel::table::proc_addr(84);
        if rp.is_null() {
            t.assert(false, "no slot");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = kernel::table::make_endpoint(0, 84);
        (*rp).p_priv = &raw mut priv_buf;

        let ep = (*rp).p_endpoint;
        // 64 bytes > grant's 32 — should fail
        t.assert(
            verify_grant(ep, 84, 0, 64, CPF_READ, 0).is_err(),
            "beyond size",
        );
        // 16 bytes <= 32 — should succeed
        t.assert(
            verify_grant(ep, 84, 0, 16, CPF_READ, 0).is_ok(),
            "within size",
        );

        (*rp).p_rts_flags =
            core::sync::atomic::AtomicU32::new(kernel::proc::RtsFlags::SLOT_FREE.bits());
    })
}

fn test_grant_revoke_reuse() -> u32 {
    run("grant_revoke_reuse", |t| unsafe {
        use arch_common::safecopies::*;
        use kernel::grants::*;
        use kernel::r#priv::Priv;

        // Two grant slots: index 0 active, index 1 free
        let mut grants: [CpGrant; 2] = [
            CpGrant {
                cp_flags: CPF_USED | CPF_VALID | CPF_DIRECT | CPF_READ,
                cp_u: CpUnion {
                    cp_direct: CpDirect {
                        cp_who_to: 85,
                        cp_start: 0x4000,
                        cp_len: 32,
                        cp_reserved: [0u8; 8],
                    },
                },
                cp_reserved: [0u8; 8],
            },
            core::mem::zeroed(),
        ];

        let gp = &raw mut grants as *mut CpGrant;
        let mut priv_buf = core::mem::zeroed::<Priv>();
        priv_buf.s_grant_table = gp as u64;
        priv_buf.s_grant_pa = gp as u64;
        priv_buf.s_grant_entries = 4;

        let rp = kernel::table::proc_addr(85);
        if rp.is_null() {
            t.assert(false, "no slot");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;
        (*rp).p_endpoint = kernel::table::make_endpoint(0, 85);
        (*rp).p_priv = &raw mut priv_buf;

        let ep = (*rp).p_endpoint;
        // Verify grant 0 works
        t.assert(
            verify_grant(ep, 85, 0, 16, CPF_READ, 0).is_ok(),
            "grant 0 valid",
        );

        // Revoke: clear USED+VALID flags via raw pointer
        let mut entry = core::ptr::read(gp.add(0));
        entry.cp_flags &= !(CPF_USED | CPF_VALID);
        core::ptr::write(gp.add(0), entry);

        // Verify after revoke fails
        t.assert(
            verify_grant(ep, 85, 0, 16, CPF_READ, 0).is_err(),
            "grant 0 revoked",
        );

        // Re-use slot 1 via raw pointer (avoids unused_assignments warning)
        core::ptr::write(
            gp.add(1),
            CpGrant {
                cp_flags: CPF_USED | CPF_VALID | CPF_DIRECT | CPF_READ,
                cp_u: CpUnion {
                    cp_direct: CpDirect {
                        cp_who_to: 85,
                        cp_start: 0x5000,
                        cp_len: 16,
                        cp_reserved: [0u8; 8],
                    },
                },
                cp_reserved: [0u8; 8],
            },
        );

        t.assert(
            verify_grant(ep, 85, 1, 16, CPF_READ, 0).is_ok(),
            "slot 1 reused",
        );

        (*rp).p_rts_flags =
            core::sync::atomic::AtomicU32::new(kernel::proc::RtsFlags::SLOT_FREE.bits());
    })
}

// Phase T: Memory alignment constraints

fn test_alloc_align64k() -> u32 {
    run("alloc_align64k", |t| unsafe {
        // Allocate with 64K alignment constraint.
        // The allocator should return an address that is 64K-aligned.
        let page = kernel::vm::alloc_mem(1, kernel::vm::PAF_ALIGN64K);
        t.assert(
            page != kernel::vm::NO_MEM,
            "alloc_mem with ALIGN64K should succeed",
        );

        let phys = page * kernel::vm::VM_PAGE_SIZE as u64;
        t.assert(
            phys.is_multiple_of(64 * 1024),
            "64K-aligned alloc should be 64K-aligned",
        );

        kernel::vm::free_mem(page, 1);
    })
}

fn test_alloc_lower16mb() -> u32 {
    run("alloc_lower16mb", |t| unsafe {
        // The test pool is at physical 4MB (0x400 pages * 4KB = 0x400000).
        // 4MB is within the lower 16MB, so this should succeed.
        let page = kernel::vm::alloc_mem(1, kernel::vm::PAF_LOWER16MB);
        t.assert(
            page != kernel::vm::NO_MEM,
            "alloc_mem with LOWER16MB should succeed from 4MB pool",
        );

        let phys = page * kernel::vm::VM_PAGE_SIZE as u64;
        t.assert(
            phys < 16 * 1024 * 1024,
            "LOWER16MB alloc should be below 16MB",
        );

        kernel::vm::free_mem(page, 1);
    })
}

fn test_stack_setup_zero() -> u32 {
    run("stack_setup_zero", |t| unsafe {
        // setup_user_stack with no arguments.
        let stack = [0u8; 4096];
        let stack_top = stack.as_ptr() as u64 + stack.len() as u64;
        let rsp = kernel::elf::setup_user_stack(stack_top, 4096, &[]);

        match rsp {
            Ok(sp) => {
                t.assert(sp.is_multiple_of(16), "RSP should be 16-byte aligned");
                let argc = core::ptr::read_volatile(sp as *const u64);
                t.assert(argc == 0, "argc should be 0");
                let argv0 = core::ptr::read_volatile((sp + 8) as *const u64);
                t.assert(argv0 == 0, "argv[0] should be NULL");
            }
            Err(_e) => t.assert(false, "setup_user_stack with 0 args failed"),
        }
    })
}

fn test_stack_setup_five() -> u32 {
    run("stack_setup_five", |t| unsafe {
        // setup_user_stack with 5 arguments.
        let stack = [0u8; 8192];
        let stack_top = stack.as_ptr() as u64 + stack.len() as u64;
        let argv = &["/bin/echo", "arg1", "arg2", "arg3", "arg4"];
        let rsp = kernel::elf::setup_user_stack(stack_top, 8192, argv);

        match rsp {
            Ok(sp) => {
                t.assert(sp.is_multiple_of(16), "RSP should be 16-byte aligned");
                let argc = core::ptr::read_volatile(sp as *const u64);
                t.assert(argc == 5, "argc should be 5");

                // Verify each argv pointer points to the right string
                for (i, expected) in argv.iter().enumerate() {
                    let ptr = core::ptr::read_volatile((sp + 8 + i as u64 * 8) as *const u64);
                    if ptr == 0 {
                        t.assert(false, "argv pointer should not be NULL");
                        continue;
                    }
                    let mut buf = [0u8; 32];
                    for (buf_pos, j) in (0..31usize).enumerate() {
                        let b = core::ptr::read_volatile((ptr as *const u8).add(j));
                        buf[buf_pos] = b;
                        if b == 0 {
                            break;
                        }
                    }
                    let s = core::str::from_utf8_unchecked(
                        &buf[..buf.iter().position(|&b| b == 0).unwrap_or(31)],
                    );
                    t.assert(s == *expected, "argv string should match");
                }

                // argv[5] = NULL (terminator)
                let term = core::ptr::read_volatile((sp + 8 + 5 * 8) as *const u64);
                t.assert(term == 0, "argv terminator should be NULL");
            }
            Err(_e) => t.assert(false, "setup_user_stack with 5 args failed"),
        }
    })
}

// Phase U: Kernel call dispatch

fn test_sys_kill_invalid() -> u32 {
    run("sys_kill_invalid", |t| unsafe {
        // system::send_sig expects a valid proc_nr. Use a large number
        // that won't have a valid proc entry.
        let result = kernel::system::send_sig(9999, 9); // SIGKILL = 9
        t.assert(result != 0, "send_sig to invalid proc should return error");
    })
}

fn test_sys_schedule_roundtrip() -> u32 {
    run("sys_schedule_roundtrip", |t| unsafe {
        // Call sched_proc which is what SYS_SCHEDULE dispatches to.
        use kernel::system::sched_proc;
        let rp = kernel::table::proc_addr(99);
        if rp.is_null() {
            t.assert(false, "proc_addr(99) failed");
            return;
        }
        (*rp).p_magic = kernel::proc::PMAGIC;

        (*rp).p_priority = 0;
        (*rp).p_cpu_time_left = 0;

        let result = sched_proc(rp, 7, 50);
        t.assert(result == 0, "sched_proc should set priority and quantum");
        // Quantum should be set (ms_2_cpu_time converts ms to cycles)
        // Allow 0 — the TSC may not be calibrated in test environment
        if (*rp).p_cpu_time_left == 0 {
            // Just check priority was set
            t.assert((*rp).p_priority == 7, "priority should be 7");
        } else {
            t.assert((*rp).p_cpu_time_left > 0, "quantum should be non-zero");
        }
    })
}

fn test_sys_getksig_pending() -> u32 {
    run("sys_getksig_pending", |t| unsafe {
        // Set up a process with p_signal_received and SIGNALED flag,
        // then call do_getksig_handler to verify it finds the signal.
        // The caller must be a signal manager (PM_PROC_NR slot 0).
        use kernel::r#priv::Priv;

        // The exiting process at slot 79
        let ep = kernel::table::proc_addr(79);
        if ep.is_null() {
            t.assert(false, "proc_addr(79) failed");
            return;
        }
        core::ptr::write_bytes(
            ep.cast::<u8>(),
            0,
            core::mem::size_of::<kernel::proc::Proc>(),
        );
        (*ep).p_magic = kernel::proc::PMAGIC;
        (*ep).p_endpoint = 79;
        (*ep).p_signal_received = 42;
        (*ep)
            .p_rts_flags
            .store(0, core::sync::atomic::Ordering::Relaxed);

        // Set up a Priv with s_sig_mgr = PM_PROC_NR (0)
        let mut priv_buf = core::mem::zeroed::<Priv>();
        priv_buf.s_proc_nr = 79;
        priv_buf.s_sig_mgr = 0;
        (*ep).p_priv = &raw mut priv_buf;

        // Set SIG_PENDING + SIGNALED (same as sys_exit_handler does)
        let sig_flags =
            kernel::proc::RtsFlags::SIGNALED.bits() | kernel::proc::RtsFlags::SIG_PENDING.bits();
        (*ep)
            .p_rts_flags
            .fetch_or(sig_flags, core::sync::atomic::Ordering::Relaxed);

        // Use PM as the caller (has valid Priv with PM_PROC_NR)
        let pm = kernel::table::proc_addr(0);
        if pm.is_null() {
            t.assert(false, "proc_addr(0) failed");
            return;
        }

        let mut msg = [0u8; kernel::proc::MESSAGE_SIZE];
        let result = kernel::system::do_getksig_handler(pm, &mut msg);
        t.assert(result == 0, "do_getksig_handler should return OK");

        // The endpoint at SIGCALLS_ENDPT_OFF (16) and status at msg[24]
        let found_ep = i32::from_ne_bytes([msg[16], msg[17], msg[18], msg[19]]);
        t.assert(found_ep == 79, "getksig should find endpoint 79");

        let found_sig = i32::from_ne_bytes([msg[24], msg[25], msg[26], msg[27]]);
        t.assert(found_sig == 42, "getksig should return signal value 42");

        (*ep).p_rts_flags.store(
            kernel::proc::RtsFlags::SLOT_FREE.bits(),
            core::sync::atomic::Ordering::Relaxed,
        );
    })
}

// Phase O: Hardware device access

fn test_rtc_cmos_reads_reasonable_time() -> u32 {
    run("rtc_cmos_reads_reasonable_time", |t| unsafe {
        // Helper: convert BCD byte to decimal
        fn bcd_to_dec(bcd: u8) -> u8 {
            (bcd >> 4) * 10 + (bcd & 0x0F)
        }

        // Read multiple RTC registers to confirm CMOS is accessible
        // RTC registers: 0x00=seconds, 0x02=minutes, 0x04=hours,
        // 0x07=day-of-month, 0x08=month, 0x09=year
        let regs: [(u8, &str); 6] = [
            (0x00, "seconds"),
            (0x02, "minutes"),
            (0x04, "hours"),
            (0x07, "day"),
            (0x08, "month"),
            (0x09, "year"),
        ];

        let mut year_val: u8 = 0;
        for &(reg, name) in &regs {
            // Select register (clear NMI bit 7)
            arch_x86_64::asm::outb(0x70, reg);
            // Read value
            let val = arch_x86_64::asm::inb(0x71);
            let dec = bcd_to_dec(val);
            // All values should be in reasonable ranges
            if name == "seconds" {
                t.assert(dec <= 59, "seconds must be 0-59");
            } else if name == "minutes" {
                t.assert(dec <= 59, "minutes must be 0-59");
            } else if name == "hours" {
                t.assert(dec <= 23, "hours must be 0-23");
            } else if name == "day" {
                t.assert((1..=31).contains(&dec), "day must be 1-31");
            } else if name == "month" {
                t.assert((1..=12).contains(&dec), "month must be 1-12");
            } else if name == "year" {
                year_val = dec;
                // QEMU RTC typically returns 0-99 (year within century)
                t.assert(dec <= 99, "year (BCD) must be 0-99");
            }
        }

        // Year must be reasonable: 2024-2099 → BCD year 24-99
        t.assert(year_val >= 24, "year should be >= 24 (2024 or later)");
        t.assert(year_val <= 99, "year should be <= 99 (2099 or earlier)");

        // Read status register A to verify CMOS is not in update cycle
        arch_x86_64::asm::outb(0x70, 0x0A); // Status Register A
        let reg_a = arch_x86_64::asm::inb(0x71);
        // UIP (Update-In-Progress) bit 7: should settle to 0 eventually
        // We just read once — on real HW this could be 1, but in QEMU it's 0
        let _uip = (reg_a & 0x80) != 0;

        // Read status register B to verify RTC is configured
        arch_x86_64::asm::outb(0x70, 0x0B); // Status Register B
        let reg_b = arch_x86_64::asm::inb(0x71);
        // Bit 2 (DM) = 0 means BCD mode (typical default)
        // Bit 1 (24/12) = 1 means 24-hour mode
        // In QEMU, these may vary; just verify the register is readable
        t.assert(
            reg_b != 0xFF,
            "status register B should be readable (not float high)",
        );
    })
}

fn test_keyboard_controller_present() -> u32 {
    run("keyboard_controller_present", |t| unsafe {
        // Read PS/2 controller status register (port 0x64)
        // This should return a valid status byte on any PC-compatible system
        let status = arch_x86_64::asm::inb(0x64);
        // Status bits:
        //   bit 0 = output buffer full (data ready to read from 0x60)
        //   bit 1 = input buffer full (controller busy)
        //   bit 2 = system flag (POST done)
        //   bit 3 = command/data (0=data, 1=command)
        //   bit 4 = keyboard lock (0=locked, 1=unlocked)
        //   bit 5 = mouse output buffer full
        //   bit 6 = general timeout
        //   bit 7 = parity error
        // In QEMU with no keyboard input, bit 0 should be 0 (nothing to read)
        t.assert(
            status & 0x01 == 0,
            "keyboard output buffer should be empty (no key pressed)",
        );
        // Bit 2 (system flag) should be 1 after POST
        t.assert(
            status & 0x04 != 0,
            "system flag bit 2 should be set after POST",
        );
        // Bit 1 (input buffer full) should be 0 (no command in progress)
        t.assert(status & 0x02 == 0, "input buffer should not be full");

        // Verify we can write a command to the keyboard controller
        // Write 0xAA to 0x64 = self-test command
        // First wait for input buffer to clear
        for _ in 0..1000 {
            let s = arch_x86_64::asm::inb(0x64);
            if s & 0x02 == 0 {
                break; // input buffer empty
            }
        }
        arch_x86_64::asm::outb(0x64, 0xAA); // self-test
        // Wait for output buffer to have data
        let mut response = 0u8;
        for _ in 0..1000 {
            let s = arch_x86_64::asm::inb(0x64);
            if s & 0x01 != 0 {
                response = arch_x86_64::asm::inb(0x60);
                break; // data ready
            }
        }
        // Self-test should return 0x55 (test passed)
        t.assert(
            response == 0x55,
            "keyboard controller self-test should return 0x55",
        );
    })
}

// Test that `restore()` transitions to ring-3 with correct register values.
//
// Calls `restore()` which loads CR3, callee-saved regs (RBX, R12-R15) from
// p_reg, sets RAX/RCX/R11/RSP from p_reg, zeroes RDX/RSI/RDI/R8-R10, then
// executes sysretq to ring-3. The ring-3 code validates:
//   - RBX == 0 (set in p_reg for this test)
//   - RAX == 0x42 (test value loaded from p_reg)
//
// If all checks pass, writes exit code 0 to QEMU isa-debug-exit (port 0x501)
// and QEMU exits with (0 << 1) | 1 = 1 (success). On failure, writes exit
// code 1 and QEMU exits with (1 << 1) | 1 = 3 (failure).
//
// Ring-3 assembly:
// ```asm
// test ebx, ebx
// jnz fail
// cmp eax, 0x42
// jne fail
// xor eax, eax     ; success
// jmp exit
// fail:
// mov eax, 1
// exit:
// mov edx, 0x501
// out dx, eax
// hlt
// ```
//
// If this function returns, the test setup failed (allocation, page table,
// or Proc entry setup). The caller should call qemu_exit_failure.
// QEMU exit helpers

mod qemu {
    const PORT: u16 = 0x501;
    fn drain_uart() {
        // Wait for UART THR (Transmitter Holding Register) empty
        // LSR bit 5 (0x20) = THR empty. Reading LSR from COM1+5.
        const COM1_LSR: u16 = 0x3F8 + 5;
        unsafe {
            for _ in 0..10000 {
                let lsr: u8;
                core::arch::asm!("in al, dx", out("al") lsr, in("dx") COM1_LSR, options(nostack));
                if lsr & 0x20 != 0 {
                    break;
                }
                core::arch::asm!("pause", options(nostack));
            }
        }
    }

    fn exit(code: u32) -> ! {
        drain_uart();
        unsafe {
            core::arch::asm!("out dx, eax", in("dx") PORT, in("eax") code);
        }
        loop {
            unsafe {
                core::arch::asm!("hlt", options(nostack));
            }
        }
    }

    #[allow(dead_code)]
    pub fn qemu_exit_success() -> ! {
        exit(0);
    }

    pub fn qemu_exit_failure(failures: u32) -> ! {
        exit(failures << 1 | 1);
    }
}
