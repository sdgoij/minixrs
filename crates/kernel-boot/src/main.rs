//! Boot binary crate.
//! Breaks circular dependency between kernel and arch-x86_64.
//!
//! Build with: `cargo build -p kernel-boot --target x86_64-unknown-none`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

#[cfg(not(test))]
use core::panic::PanicInfo;

#[cfg(not(test))]
#[cfg(not(feature = "integration-tests"))]
use kernel_boot::boot_init;

#[cfg(not(test))]
use kernel_boot::serial_write;

/// Dummy entry point to prevent --gc-sections from discarding all code.
/// The actual entry is through the multiboot trampoline which jumps
/// directly to kmain.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    kmain_body()
}

// Linker symbol: byte just past the end of the kernel binary
// (after the `.initramfs` section, page-aligned in minix-raw.ld).
#[cfg(not(test))]
unsafe extern "C" {
    static __kernel_end: u8;
}

/// Asm entry point: adjust RSP by 8 for the jmp-entry ABI mismatch.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn kmain() -> ! {
    core::arch::naked_asm!("sub rsp, 8", "jmp kmain_body",);
}

/// Kernel main entry point.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn kmain_body() -> ! {
    // Enable SSE (required by compiler_builtins memset/memcpy which use
    // SSE instructions like movdqa). CR4.OSFXSR = bit 9, OSXMMEXCPT = bit 10.
    unsafe {
        let cr4 = arch_x86_64::asm::read_cr4();
        arch_x86_64::asm::write_cr4(cr4 | (1 << 9) | (1 << 10));
    }

    // Initialize subsystems
    kernel::init();

    // Initialize basic userspace syscall handlers
    unsafe {
        kernel::syscall::init_basic_syscalls();
    }

    #[cfg(feature = "boot-test")]
    unsafe {
        // Register the boot-complete syscall (60) that VFS calls after
        // mount_root succeeds.  The handler runs verification tests.
        kernel::syscall::register_basic_syscall(60, boot_test_syscall_handler);
    }

    fn dma_alloc(pages: usize) -> Option<(*mut u8, u64)> {
        let alloc = arch_x86_64::alloc::global_allocator();
        if alloc.is_null() {
            return None;
        }
        let phys = unsafe { (*alloc).alloc_contig(pages) }?;
        Some((phys as *mut u8, phys))
    }
    fn dma_free(virt: *mut u8, pages: usize) {
        let alloc = arch_x86_64::alloc::global_allocator();
        if alloc.is_null() {
            return;
        }
        unsafe { (*alloc).free_contig(virt as u64, pages) };
    }
    unsafe {
        drivers::storage::dma::register_allocator(dma_alloc, dma_free);
    }

    // Initialize serial FIRST before any serial_write calls.
    init_serial();

    // Initialize the physical memory allocator.
    serial_write("initializing allocator...\r\n");
    {
        // Use the linker-provided __kernel_end symbol to compute where the
        // kernel binary ends in physical memory. This is page-aligned in
        // minix-raw.ld (ALIGN(4096) after .initramfs).
        let kernel_end = core::ptr::addr_of!(__kernel_end) as u64;

        // Build the physical memory map following the MINIX pattern
        // (see pre_init.c get_parameters + kmain):
        //
        //   1. Add AVAILABLE regions as reported by the platform.
        //   2. Remove occupied regions (kernel + boot modules).
        //   3. (After bootstrap) Release bootstrap memory back to pool.
        //
        // Without a multiboot-provided memory map, we use the known
        // QEMU `-m 256M` physical layout. Everything BELOW kernel_end is
        // occupied by one of:
        //
        //   0x000000 - 0x09FFFF   Conventional (640 KB) — may contain
        //                          real-mode IVT/BDA/EBDA from SeaBIOS
        //   0x0A0000 - 0x0FFFFF   Reserved (VGA, BIOS, option ROMs)
        //   0x100000 - 0x10XXXX   32-bit trampoline (linked at 0x100000).
        //                          Its .bss holds the ACTIVE identity-
        //                          mapped page tables (PML4/PDP/PD at
        //                          ~0x101000) that CR3 still points to.
        //                          Overwriting these causes a triple fault.
        //   0x200000 - kernel_end  Kernel binary (loaded by -device loader).
        //
        // The free pool starts at kernel_end (after all occupied ranges).
        //
        const PHYS_MEM_TOP: u64 = 0x1000_0000; // 256 MB

        let mut mmap = arch_x86_64::alloc::PhysicalMemoryMap::new();

        // Step 1: Add memory from kernel binary end to top of RAM.
        // This is unequivocally free — the trampoline and kernel occupy
        // everything below, and QEMU provides contiguous RAM from 0.
        if kernel_end < PHYS_MEM_TOP {
            mmap.add(kernel_end, PHYS_MEM_TOP);
        }

        // Step 2: Remove platform-specific reserved region near top.
        // QEMU reserves a window for ACPI tables, PIIX4 PM registers,
        // and PCI config space at the top of the 256 MB range.
        mmap.cut(0x0FE0_0000, 0x0FF0_0000);

        arch_x86_64::alloc::init_allocator(&mmap);

        // Initialize the kernel::vm physical page allocator (separate bitmap
        // from arch_x86_64::alloc). This allocator is used by kernel call 62
        // (VM_PAGING_ALLOC) which VM servers use to allocate physical pages.
        // Without this, vm_alloc_pages() returns 0 and every fork fails.
        unsafe {
            let kernel_end = core::ptr::addr_of!(__kernel_end) as u64;
            let kernel_end_page = kernel_end.div_ceil(4096);
            let total_pages = 256 * 1024 * 1024 / 4096;
            if kernel_end_page < total_pages {
                let free_chunks = [kernel::vm::MemoryChunk {
                    base: kernel_end_page,
                    size: total_pages - kernel_end_page,
                }];
                kernel::vm::mem_init(&free_chunks);
            }
        }
    }
    serial_write("allocator ready\r\n");

    // Print banner via serial
    serial_write("Hello MINIX!\r\n");

    // Initialize the PIT timer interrupt.
    unsafe {
        // 1. Remap the PIC to avoid overlap with CPU exceptions.
        arch_x86_64::apic::remap_pic(
            arch_x86_64::interrupt::PIC_MASTER_BASE,
            arch_x86_64::interrupt::PIC_SLAVE_BASE,
        );

        // 2. Mask timer IRQ — SeaBIOS may leave IRQ0 unmasked.
        arch_x86_64::apic::mask_timer_irq();

        // 3. Register the timer ISR handler and install the IDT entry
        //    BEFORE programming the PIT, to avoid a window where a timer
        //    interrupt hits a null handler (init_idt sets all entries to
        //    handler=0, which would jump to address 0 on interrupt).
        unsafe extern "C" fn timer_callback() {
            unsafe { kernel::clock::timer_int_handler() };
        }
        arch_x86_64::apic::set_timer_isr_handler(timer_callback);

        let handler_addr = arch_x86_64::apic::timer_isr_entry as *const () as u64;
        (*arch_x86_64::idt::IDT.get()).set_handler(
            arch_x86_64::interrupt::VECTOR_TIMER as usize,
            handler_addr,
            0, // IST
            0, // DPL (kernel only)
        );

        // 4. Program the PIT at 100 Hz, mode 3 (square wave).
        //    The timer is still masked at the PIC; it will be unmasked
        //    just before the scheduler starts (after enqueuing).
        arch_x86_64::apic::init_pit(100);

        // 5. Timer IRQ is unmasked just before the scheduler starts,
        //    after all boot processes are initialized and running.

        // 6. Configure COM1 for interrupt-driven input.
        // C callback called on every serial interrupt.
        unsafe extern "C" fn serial_callback() {
            const COM1_DATA: u16 = 0x3F8;
            unsafe {
                // Read all available bytes from the serial port.
                loop {
                    let lsr: u8;
                    core::arch::asm!(
                        "in al, dx",
                        out("al") lsr,
                        in("dx") COM1_DATA + 5,
                        options(nomem, nostack)
                    );
                    if lsr & 0x01 == 0 {
                        break; // no data ready
                    }
                    let byte: u8;
                    core::arch::asm!(
                        "in al, dx",
                        out("al") byte,
                        in("dx") COM1_DATA,
                        options(nomem, nostack)
                    );
                    kernel::ser_input::push_byte(byte);
                }
            }
        }
        arch_x86_64::apic::set_serial_isr_handler(serial_callback);

        // 7. Install the serial ISR in the IDT (IRQ 4 → vector 0x24).
        let serial_handler_addr = arch_x86_64::apic::serial_isr_entry as *const () as u64;
        (*arch_x86_64::idt::IDT.get()).set_handler(
            arch_x86_64::interrupt::irq_vector(4) as usize,
            serial_handler_addr,
            0, // IST
            0, // DPL (kernel only)
        );

        // 8. Enable COM1 receive interrupts and unmask IRQ 4.
        // Serial interrupts are fine — the serial ISR doesn't use iretq
        // to return to user mode.
        arch_x86_64::apic::enable_com1_interrupts();
        arch_x86_64::apic::unmask_serial_irq();
    }

    #[cfg(feature = "integration-tests")]
    {
        serial_write("Running integration tests...\r\n");
        // This never returns — calls qemu_exit_success/failure
        kernel_boot::test_runner::run_integration_tests();
    }

    #[cfg(not(feature = "integration-tests"))]
    {
        use arch_common::com::*;

        serial_write("  initializing boot processes...\r\n");

        unsafe {
            kernel::table::proc_init();
            kernel::system::system_init();
            kernel::ipc::register_ipc_syscalls();

            // Set CPU frequency so clock::ms_2_cpu_time works for all
            // processes (including child processes created via fork).
            // Without this, cpu_get_freq(0) returns 0, ms_2_cpu_time
            // returns 0, and every SYS_SCHEDULE gives forked children
            // p_cpu_time_left = 0. That causes the first timer tick to
            // call proc_no_time → notify_scheduler → RTS_NO_QUANTUM,
            // and the child hangs forever because the SCHED server
            // has no message loop to handle SCHEDULING_NO_QUANTUM.
            kernel::glo::cpu_set_freq(0, 2_500_000_000);
        }

        // Define all boot processes: (path, proc_nr, endpoint_name)
        // VFS must come before MFS so VFS's SENDREC is queued and
        // processed when MFS later runs.
        // When boot-test is active, INIT is excluded so the test
        // completes before any user process starts.
        #[cfg(not(feature = "boot-test"))]
        let boot_procs: &[(&str, i32)] = &[
            ("/sbin/ds", DS_PROC_NR),           // Data Store (first, matches C order)
            ("/sbin/rs", RS_PROC_NR),           // Reincarnation Server
            ("/sbin/pm", PM_PROC_NR),           // Process Manager
            ("/sbin/sched", SCHED_PROC_NR),     // Scheduler
            ("/sbin/vfs", VFS_PROC_NR),         // Virtual File System
            ("/sbin/ramdisk", RAMDISK_PROC_NR), // RAM disk block driver
            ("/sbin/vm", VM_PROC_NR),           // Virtual Memory
            ("/sbin/mfs", MFS_PROC_NR),         // Minix File System
            ("/sbin/tty", TTY_PROC_NR),         // Terminal driver
            ("/sbin/init", INIT_PROC_NR),       // init
        ];
        #[cfg(feature = "boot-test")]
        let boot_procs: &[(&str, i32)] = &[
            ("/sbin/ds", DS_PROC_NR),           // Data Store (first, matches C order)
            ("/sbin/rs", RS_PROC_NR),           // Reincarnation Server
            ("/sbin/pm", PM_PROC_NR),           // Process Manager
            ("/sbin/sched", SCHED_PROC_NR),     // Scheduler
            ("/sbin/vfs", VFS_PROC_NR),         // Virtual File System
            ("/sbin/ramdisk", RAMDISK_PROC_NR), // RAM disk block driver
            ("/sbin/vm", VM_PROC_NR),           // Virtual Memory
            ("/sbin/mfs", MFS_PROC_NR),         // Minix File System
            ("/sbin/tty", TTY_PROC_NR),         // Terminal driver
        ];

        // Load each boot process from initramfs, storing InitInfo for
        // per-process page table creation.
        serial_write("  loading boot processes...\r\n");
        let mut first_proc: *mut kernel::proc::Proc = core::ptr::null_mut();
        for (i, &(path, proc_nr)) in boot_procs.iter().enumerate() {
            let info = match unsafe { boot_init::load_and_prepare_proc(path, proc_nr, &[path]) } {
                Some(info) => info,
                None => {
                    serial_write("  FAILED: ");
                    serial_write(path);
                    serial_write("\r\n");
                    hlt_loop();
                }
            };

            let rp = kernel::table::proc_addr(proc_nr);
            if i == 0 {
                first_proc = rp;
            }

            // Create a restricted page table that maps only this process's
            // code and stack, not the entire identity-mapped 1GB region.
            let pt_phys = unsafe {
                boot_init::boot_create_restricted_page_table(
                    info.code_start,
                    info.code_end,
                    info.phys_code_base,
                    info.stack_start,
                    info.stack_end,
                    info.phys_stack_base,
                )
            };
            let pt_phys = match pt_phys {
                Some(p) => p,
                None => {
                    serial_write("  FAILED: page table for ");
                    serial_write(path);
                    serial_write("\r\n");
                    hlt_loop();
                }
            };

            unsafe {
                core::ptr::write_volatile(&raw mut (*rp).p_seg.p_cr3, pt_phys);
                // Set up privilege structure for this boot process.
                // Without this, p_priv is null, which causes:
                // - Notifications to be silently dropped (mini_notify checks
                //   p_priv before setting the pending bit)
                // - kernel_call_dispatch to UB when dereferencing null for
                //   the k_call_mask permission check
                // get_priv allocates a Priv entry with all kernel calls allowed.
                let _ = kernel::system::get_priv(rp);
                // Set scheduling parameters matching C MINIX proc_init:
                // all user-space processes get USER_Q = 7 and USER_QUANTUM = 200ms.
                // Priority 0 (TASK_Q) is reserved for kernel tasks that run in
                // ring 0 — none of our boot processes are kernel tasks.
                // The SCHED server will later adjust these via SYS_SCHEDCTL.
                let priority: i8 = 7; // USER_Q
                let quantum_ms: u32 = 200; // USER_QUANTUM
                core::ptr::write_volatile(&raw mut (*rp).p_priority, priority);
                core::ptr::write_volatile(&raw mut (*rp).p_quantum_size_ms, quantum_ms);
                // p_cpu_time_left in cycles: 200ms * 2.5 GHz = 500M cycles
                core::ptr::write_volatile(
                    &raw mut (*rp).p_cpu_time_left,
                    (quantum_ms as u64) * 2_500_000,
                );
                // p_scheduler is already null (kernel scheduler) from Proc::default()
            }

            let user_flags =
                kernel::pagetable::PG_P | kernel::pagetable::PG_RW | kernel::pagetable::PG_U;

            // Map the brk range.
            // For processes other than VM, pre-allocate a 1MB heap so brk
            // calls work during boot before VM is fully initialized.
            // VM manages its own heap via kernel allocator calls.
            if proc_nr != VM_PROC_NR {
                let brk_va_start = 0x3FE00000u64;
                let brk_va_end = 0x3FF00000u64;
                let brk_pages = ((brk_va_end - brk_va_start) / 4096) as usize;
                let brk_phys = match unsafe { kernel::hal::alloc_phys_contig(brk_pages) } {
                    Some(base) => base,
                    None => {
                        serial_write("  FAILED: out of memory for brk heap\r\n");
                        hlt_loop();
                    }
                };
                for j in 0..brk_pages {
                    let va = brk_va_start + (j as u64) * 4096;
                    let pa = brk_phys + (j as u64) * 4096;
                    if unsafe { kernel::pagetable::map_page(pt_phys, va, pa, user_flags) }.is_err()
                    {
                        serial_write("  FAILED: brk page mapping\r\n");
                        hlt_loop();
                    }
                }
            }

            // If this is the MFS process, set up the RAM disk mapping.
            if proc_nr == MFS_PROC_NR {
                let image = kernel::minixfs::minixfs_image();
                let image_len = kernel::minixfs::minixfs_image_len();
                if image_len > 0 {
                    let pages = image_len.div_ceil(4096);
                    let ramdisk_phys = match unsafe { kernel::hal::alloc_phys_contig(pages) } {
                        Some(base) => base,
                        None => {
                            serial_write("  FAILED: out of memory for RAM disk\r\n");
                            hlt_loop();
                        }
                    };
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            image.as_ptr(),
                            ramdisk_phys as *mut u8,
                            image_len,
                        );
                    }
                    for j in 0..pages {
                        let va = arch_common::com::MFS_RAMDISK_VA + (j as u64) * 4096;
                        let pa = ramdisk_phys + (j as u64) * 4096;
                        if unsafe { kernel::pagetable::map_page(pt_phys, va, pa, user_flags) }
                            .is_err()
                        {
                            serial_write("  FAILED: RAM disk page mapping\r\n");
                            hlt_loop();
                        }
                    }
                    serial_write("  RAM disk mapped for MFS\r\n");
                }
            }
        }

        #[cfg(target_os = "none")]
        unsafe {
            arch_x86_64::asm::syscall_abi::set_syscall_handler(syscall_handler_c);
            let entry = arch_x86_64::asm::syscall_abi::syscall_entry as *const () as u64;
            arch_x86_64::arch_syscall::setup_syscall_msrs(entry);
            arch_x86_64::cpulocals::init_cpulocals();
            kernel::panic::mark_cpulocals_ready();
            // Set up TSS and GDT for ring-3 interrupts and exception handlers.
            arch_x86_64::init_tss_for_boot();

            // Install exception handlers: page fault, GPF, double fault.
            // These use IST stacks for reliability.
            let pf_entry = arch_x86_64::asm::exception_page_fault_entry as *const () as u64;
            (*arch_x86_64::idt::IDT.get()).set_handler(14, pf_entry, 1, 0);
            let gpf_entry = arch_x86_64::asm::exception_gpf_entry as *const () as u64;
            (*arch_x86_64::idt::IDT.get()).set_handler(13, gpf_entry, 0, 0);
            let df_entry = arch_x86_64::asm::exception_double_fault_entry as *const () as u64;
            (*arch_x86_64::idt::IDT.get()).set_handler(8, df_entry, 2, 0);
        }

        if first_proc.is_null() {
            serial_write("  FAILED: no boot processes found\r\n");
            hlt_loop();
        }

        serial_write("  enqueuing processes...\r\n");

        // Ensure all boot processes are runnable with clean flags.
        // In real MINIX, BOOTINHIBIT is cleared by VM via VMCTL_BOOTINHIBIT_CLEAR.
        // VM is a stub, so clear it here. Also clear any stale undefined bits.
        for &(_, proc_nr) in boot_procs {
            let rp = kernel::table::proc_addr(proc_nr);
            unsafe {
                (*rp)
                    .p_rts_flags
                    .store(0, core::sync::atomic::Ordering::Relaxed);
                kernel::sched::enqueue(rp);
            }
        }

        // Send a boot notification to PM. This must happen AFTER enqueuing
        // so PM isn't double-enqueued (mini_notify enqueues on direct delivery).
        // The notification will be pending when PM calls RECEIVE.
        unsafe {
            kernel::ipc::mini_notify(arch_common::com::RS_PROC_NR, arch_common::com::PM_PROC_NR);
        }

        // Set the current process pointer to the first one.
        unsafe {
            arch_x86_64::cpulocals::set_cpulocal_proc_ptr(first_proc as *mut core::ffi::c_void);
        }

        serial_write("  scheduler starting...\r\n");

        // Jump to the first process via restore().
        // The timer IRQ is masked at the START of restore() and unmasked
        // right before iretq, ensuring the entire swapgs → iretq sequence
        // is covered. The timer can only fire in user mode after iretq.
        unsafe {
            arch_x86_64::asm::restore(first_proc as *const u8);
        }
    }
}

/// Save the current process's register state from the kernel-stack save
/// area into its Proc::p_reg TrapFrame, so a later restore() can resume it.
///
/// The save area layout (14 × u64 pushed by syscall_entry naked asm):
///   [0]=rax, [1]=rbx, [2]=rcx(=RIP), [3]=rdx, [4]=rsi, [5]=rdi,
///   [6]=r8, [7]=r9, [8]=r10, [9]=r11(=RFLAGS), [10]=r12,
///   [11]=r13, [12]=r14, [13]=r15
///
/// The original user RSP is read from the SAVED_USER_RSP static
/// (written by syscall_entry before switching to the kernel stack).
///
/// # Safety
///
/// `saved` must point to a valid kernel-stack save area pushed by
/// `syscall_entry`. `rp` must point to a valid `Proc`.
unsafe fn save_proc_regs(rp: *mut kernel::proc::Proc, saved: *const u64) {
    // syscall_entry push order (bottom of stack = RSP after last push):
    //   push rbp → saved[14] (highest address, pushed first)
    //   push r15 → saved[13]
    //   push r14 → saved[12]
    //   push r13 → saved[11]
    //   push r12 → saved[10]
    //   push r11 → saved[9]   ← user RFLAGS from syscall/sysenter
    //   push r10 → saved[8]
    //   push r9  → saved[7]
    //   push r8  → saved[6]
    //   push rdi → saved[5]
    //   push rsi → saved[4]
    //   push rdx → saved[3]
    //   push rcx → saved[2]   ← user RIP from syscall/sysenter
    //   push rbx → saved[1]
    //   push rax → saved[0]   (RSP points here, pushed last)
    //
    // p_reg TrapFrame byte offsets (matching restore() iretq layout):
    //   0=rax, 8=rbx, 16=rcx(=RIP), 24=rdx, 32=rsi, 40=rdi,
    //   48=r8, 56=r9, 64=r10, 72=r11(=RFLAGS), 80=r12,
    //   88=r13, 96=r14, 104=r15, 112=rbp, 168=rsp
    let frame = unsafe { &mut (*rp).p_reg };
    unsafe {
        let regs = [
            (0usize, *saved.add(0)),    // rax = saved[0]  (pushed last, at RSP)
            (8usize, *saved.add(1)),    // rbx = saved[1]
            (16usize, *saved.add(2)),   // rcx = saved[2]  ← user RIP
            (24usize, *saved.add(3)),   // rdx = saved[3]
            (32usize, *saved.add(4)),   // rsi = saved[4]
            (40usize, *saved.add(5)),   // rdi = saved[5]
            (48usize, *saved.add(6)),   // r8  = saved[6]
            (56usize, *saved.add(7)),   // r9  = saved[7]
            (64usize, *saved.add(8)),   // r10 = saved[8]
            (72usize, *saved.add(9)),   // r11 = saved[9]  ← user RFLAGS
            (80usize, *saved.add(10)),  // r12 = saved[10]
            (88usize, *saved.add(11)),  // r13 = saved[11]
            (96usize, *saved.add(12)),  // r14 = saved[12]
            (104usize, *saved.add(13)), // r15 = saved[13] (pushed second, highest addr after rbp)
            (112usize, *saved.add(14)), // rbp = saved[14] (pushed first, highest addr)
        ];
        for (offset, val) in regs {
            let bytes = val.to_ne_bytes();
            for (i, b) in bytes.iter().enumerate() {
                core::ptr::write_volatile(frame.as_mut_ptr().add(offset + i), *b);
            }
        }
        // RSP = SAVED_USER_RSP (written by syscall_entry naked asm
        // before switching to kernel stack, so it reflects the exact
        // user RSP at the time of syscall).
        // Use cfg guard: syscall_abi module only exists on target_os = "none".
        #[cfg(target_os = "none")]
        let rsp = arch_x86_64::asm::syscall_abi::saved_user_rsp();
        #[cfg(not(target_os = "none"))]
        let rsp: u64 = 0;
        for (i, b) in rsp.to_ne_bytes().iter().enumerate() {
            core::ptr::write_volatile(frame.as_mut_ptr().add(168 + i), *b);
        }
    }
}

/// C handler for syscall entry — called from arch-x86_64's naked asm.
/// Dispatches the syscall, then attempts round-robin context switch
/// by saving the current process's state, re-enqueuing it, and picking
/// the next runnable process via the scheduler.
///
/// # Safety
///
/// `saved` must point to a valid register save area on the kernel stack.
/// The current CPU local storage must have a valid process pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syscall_handler_c(saved: *const u64) {
    #[allow(unused_unsafe)]
    unsafe {
        let nr = core::ptr::read_volatile(saved) as usize;
        let rp = arch_x86_64::cpulocals::get_cpulocal_proc_ptr() as *mut kernel::proc::Proc;
        if rp.is_null() {
            core::ptr::write_volatile(saved as *mut u64, 0);
            return;
        }

        let args = [
            core::ptr::read_volatile(saved.add(5)),
            core::ptr::read_volatile(saved.add(4)),
            core::ptr::read_volatile(saved.add(3)),
            core::ptr::read_volatile(saved.add(8)),
            core::ptr::read_volatile(saved.add(6)),
            core::ptr::read_volatile(saved.add(7)),
        ];
        // Save registers BEFORE dispatch for all syscalls. The timer ISR
        // can preempt any syscall and trigger a context switch. When the
        // process is restored, p_reg must have the CURRENT syscall's
        // registers (RIP pointing to the return from this syscall), not
        // stale state from a previous blocking syscall.
        // For SYS_EXEC_REPLACE (61), skip — the dispatch replaces the
        // process image and sets up new p_reg.
        if nr != 61 {
            save_proc_regs(rp, saved);
        }

        let result = kernel::syscall::dispatch_basic_syscall(rp, nr, &args);
        core::ptr::write_volatile(saved as *mut u64, result as u64);

        // Check if this was a successful exec — needed for scheduler logic.
        let is_exec = nr == 61 && result == 0;

        // If the current process is still runnable (not blocked) and not
        // preempted, continue running it — matching C MINIX switch_to_user.
        // Only pick a new process when the current process has blocked.
        let rts = (*rp)
            .p_rts_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        if rts & kernel::proc::RtsFlags::PREEMPTED.bits() != 0 {
            // Preempted: clear flag and re-enqueue, then pick another.
            let cleared = rts & !kernel::proc::RtsFlags::PREEMPTED.bits();
            (*rp)
                .p_rts_flags
                .store(cleared, core::sync::atomic::Ordering::Relaxed);
            // Always enqueue at tail after preemption (round-robin).
            // Matching C: preempted processes go to tail so other
            // processes at the same priority get a chance to run.
            if cleared == 0 {
                kernel::sched::enqueue(rp);
            }
        } else if rts == 0 && !is_exec {
            // Still runnable — continue with same process.
            let delivered = unsafe { deliver_msg(rp) };
            // Same-process return: syscall_entry pops RAX from saved[0],
            // NOT from p_reg[0].  If deliver_msg set a return value, write
            // it to the kernel stack so the pop chain returns it correctly.
            if delivered >= 0 {
                core::ptr::write_volatile(saved as *mut u64, delivered as u64);
            }
            return;
        }

        // Current process is blocked or preempted — pick a new one.
        let next = loop {
            if let Some(candidate) = kernel::sched::pick_proc() {
                if (*candidate).is_preempted() {
                    let old = (*candidate).p_rts_flags.fetch_and(
                        !kernel::proc::RtsFlags::PREEMPTED.bits(),
                        core::sync::atomic::Ordering::Relaxed,
                    );
                    // If no other blocking flags remain, re-enqueue
                    if (old & !kernel::proc::RtsFlags::PREEMPTED.bits()) == 0 {
                        if (*candidate).p_cpu_time_left > 0 {
                            kernel::sched::enqueue_head(candidate);
                        } else {
                            kernel::sched::enqueue(candidate);
                        }
                    }
                    continue;
                }
                break candidate;
            } else {
                // No runnable processes — halt CPU until interrupt.
                // To keep real hardware compatibility, enable interrupts
                // before hlt and disable after.
                core::arch::asm!("sti", "hlt", "cli", options(nomem, nostack));
                // Retry after interrupt
                continue;
            }
        };

        // D4: scheduler switch
        if next != rp || is_exec {
            let delivered_next = unsafe { deliver_msg(next) };
            let _ = delivered_next;
            arch_x86_64::cpulocals::set_cpulocal_proc_ptr(next as *mut core::ffi::c_void);
            arch_x86_64::asm::restore(next as *const u8);
        } else {
            // Same process: deliver pending message before returning.
            let delivered = unsafe { deliver_msg(rp) };
            // deliver_msg writes the source endpoint to p_reg[0] via
            // write_retval, but syscall_entry returns RAX by popping from
            // the kernel stack (saved[0]), NOT from p_reg[0].  The dispatch
            // result written to saved[0] at line 582 is the syscall return
            // value (0=OK), not the message source endpoint.  Overwrite
            // saved[0] so the pop chain returns the correct value.
            if delivered >= 0 {
                core::ptr::write_volatile(saved as *mut u64, delivered as u64);
            }
        }
    }
}

/// Deliver any pending IPC message to a process's user-space buffer.
///
/// If the `DELIVERMSG` flag is set on the process, this calls
/// `kernel::ipc::delivermsg` to copy the contents of `p_delivermsg`
/// to the user-space virtual address stored in `p_delivermsg_vir`.
///
/// After delivery, RAX is set to the source endpoint from the message
/// header (bytes 0-3).
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
/// Deliver any pending IPC message to a process's user-space buffer.
///
/// If the `DELIVERMSG` flag is set on the process, this calls
/// `kernel::ipc::delivermsg` to copy the contents of `p_delivermsg`
/// to the user-space virtual address stored in `p_delivermsg_vir`.
///
/// After delivery, RAX is set to the source endpoint from the message
/// header (bytes 0-3).  Returns the source endpoint if delivery was
/// performed, or -1 if there was no pending message.
///
/// # Safety
///
/// `rp` must point to a valid `Proc`.
unsafe fn deliver_msg(rp: *mut kernel::proc::Proc) -> i32 {
    unsafe {
        let has_deliver = (*rp)
            .p_misc_flags
            .load(core::sync::atomic::Ordering::Relaxed)
            & kernel::proc::MiscFlags::DELIVERMSG.bits()
            != 0;
        if has_deliver {
            let result = kernel::ipc::delivermsg(rp);
            let _ = result;
            let src_ep_bytes =
                core::ptr::read_unaligned((*rp).p_delivermsg.as_ptr() as *const [u8; 4]);
            let src_ep = i32::from_ne_bytes(src_ep_bytes);
            kernel::hal::write_retval(&mut (*rp).p_reg, src_ep as u64);
            (*rp).p_misc_flags.fetch_and(
                !kernel::proc::MiscFlags::DELIVERMSG.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
            return src_ep;
        }
    }
    -1
}

// Serial I/O — available in all build modes (no-op in test mode)

/// Halt the CPU forever (fallback if boot fails).
#[cfg(not(test))]
/// Halt the CPU forever (used on fatal boot errors).
#[allow(dead_code)]
fn hlt_loop() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
    }
}

/// Handler for SYS_BOOT_COMPLETE (syscall 60) — called by VFS after mount_root.
///
/// When the `boot-test` feature is enabled, VFS calls this syscall after
/// mount_root completes.  The handler runs the boot test suite and exits
/// QEMU via isa-debug-exit.
#[cfg(feature = "boot-test")]
unsafe fn boot_test_syscall_handler(_caller: *mut kernel::proc::Proc, _args: &[u64; 6]) -> i64 {
    unsafe {
        kernel_boot::boot_test::run_boot_tests();
    }
}

/// Initialize COM1 serial port (115200 baud, 8N1).
#[cfg(not(test))]
fn init_serial() {
    unsafe {
        let port = 0x3F8u16;
        core::arch::asm!("out dx, al", in("dx") port + 1, in("al") 0x00u8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") port + 3, in("al") 0x80u8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") port, in("al") 0x01u8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") port + 1, in("al") 0x00u8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") port + 3, in("al") 0x03u8, options(nomem, nostack));
        // FIFO trigger at 14 bytes (bits 7-6 = 11). Polling fallback in
        // read_blocking handles cases with fewer bytes than the trigger.
        core::arch::asm!("out dx, al", in("dx") port + 2, in("al") 0xC7u8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") port + 4, in("al") 0x0Bu8, options(nomem, nostack));
    }
}

// serial_write, serial_putc, and print! macro are now in kernel_boot lib crate

/// Panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::panic::handle(info)
}
