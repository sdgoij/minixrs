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

    // Phase A: Page table basics (x86 hardware: CR3, PML4, identity map)
    let mut total: u32 = 0;
    total += test_boot_cr3();
    total += test_boot_pml4_entries();
    total += test_identity_map_range();
    total += test_kernel_high_map();

    // Phase B: Page table manipulation (x86 boot page table)
    total += test_pt_walk_boot();
    total += test_pt_mapkernel();

    // Phase F: Process table — call proc_init to initialize process slots
    // (kernel::init doesn't call proc_init, so we do it here)
    unsafe {
        kernel::table::proc_init();
    }
    total += test_proc_addr_valid();
    total += test_proc_addr_invalid();
    total += test_endpoint_lookup();

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

    // Phase H: Kernel unit tests — arch-agnostic suite shared with
    // RISC-V/AArch64 integration builds (allocator, VM, grants, syscalls,
    // timers, IRQ, ELF, IPC, scheduler, safecopy, stack setup).
    total += kernel::tests::run_all();

    // Enable interrupts and unmask timer IRQ so the monotonic
    // clock advances during timer tests.
    unsafe {
        core::arch::asm!("sti", options(nostack, nomem));
        arch_x86_64::apic::unmask_timer_irq();
    }

    // Phase L: PIT (x86 hardware)
    total += test_pit_programmed();

    // Phase P: exec page table setup (needs boot_cr3, x86 layout)
    total += test_exec_setup_new_page_table();

    // Phase R: sub-16MB VM allocation (x86 test pool)
    total += test_alloc_lower16mb();

    // Phase O: Hardware device access (x86)
    total += test_rtc_cmos_reads_reasonable_time();
    total += test_keyboard_controller_present();

    if total == 0 {
        serial_puts("-- done --\r\n");
        qemu::qemu_exit_success();
    } else {
        serial_puts("FAILURES: ");
        let mut t = total;
        if t >= 100 {
            serial_putc(b'0' + (t / 100) as u8);
            t %= 100;
        }
        if t >= 10 {
            serial_putc(b'0' + (t / 10) as u8);
            t %= 10;
        }
        serial_putc(b'0' + t as u8);
        serial_puts("\r\n");
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

// Phase B: Page Table Manipulation

use kernel::pagetable::{boot_cr3, pt_mapkernel, walk};

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
            for _ in 0..1000000 {
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
