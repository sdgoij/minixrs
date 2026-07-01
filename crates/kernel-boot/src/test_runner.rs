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

fn print_hex(val: u64) {
    let hex = b"0123456789abcdef";
    for i in (0..16).rev() {
        let nibble = ((val >> (i * 4)) & 0xF) as usize;
        serial_putc(hex[nibble]);
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

// ===========================================================================
// Phase B: Page Table Manipulation
// ===========================================================================

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

// ===========================================================================
// Phase C: Physical Memory Allocator
// ===========================================================================

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

// ===========================================================================
// Phase D: VM Allocator (kernel::vm)
// ===========================================================================

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

// ===========================================================================
// Phase F: Process Table
// ===========================================================================

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

// ===========================================================================
// Phase G: IPC
// ===========================================================================

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

    // Step 1: Use fixed addresses in a safe range (3MB-4MB, above kernel
    // at 2MB, below the kernel stack at 8MB, within the boot identity map).
    // These pages must NOT be allocated by the arch allocator.
    let code_page: u64 = 0x0030_0000; // 3 MB: ring-3 code
    let stack_top: u64 = 0x0031_1000; // 3 MB + 4KB + 4KB: top of stack

    // Step 2: Write the ring-3 code blob (just isa-debug-exit, no serial).
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

    // Debug: print addresses
    serial_puts("  code_page=0x");
    print_hex(code_page);
    serial_puts(" stack=0x");
    print_hex(stack_top);
    serial_puts(" cr3=0x");
    print_hex(pt_phys);
    serial_puts("\r\n");

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
    const PORT: u16 = 0x501;
    fn exit(code: u32) -> ! {
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
        exit(1);
    }

    pub fn qemu_exit_failure(failures: u32) -> ! {
        exit(failures << 1 | 1);
    }
}
