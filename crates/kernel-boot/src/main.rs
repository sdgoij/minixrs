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
    kmain()
}

// Linker symbol: byte just past the end of the kernel binary
// (after the `.initramfs` section, page-aligned in minix-raw.ld).
#[cfg(not(test))]
unsafe extern "C" {
    static __kernel_end: u8;
}

/// Kernel main entry point — called from the multiboot trampoline.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
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

        // 2. Program the PIT at 100 Hz, mode 3 (square wave).
        arch_x86_64::apic::init_pit(100);

        // 3. Register the timer ISR handler.
        unsafe extern "C" fn timer_callback() {
            unsafe { kernel::clock::timer_int_handler() };
        }
        arch_x86_64::apic::set_timer_isr_handler(timer_callback);

        // 4. Install the assembly trampoline in the IDT.
        let handler_addr = arch_x86_64::apic::timer_isr_entry as *const () as u64;
        (*arch_x86_64::idt::IDT.get()).set_handler(
            arch_x86_64::interrupt::VECTOR_TIMER as usize,
            handler_addr,
            0, // IST
            0, // DPL (kernel only)
        );

        // 5. Unmask the timer IRQ on the master PIC.
        arch_x86_64::apic::unmask_timer_irq();

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
        }

        // Define all boot processes: (path, proc_nr, endpoint_name)
        // VFS must come before MFS so VFS's SENDREC is queued and
        // processed when MFS later runs.
        // When boot-test is active, INIT is excluded so the test
        // completes before any user process starts.
        #[cfg(not(feature = "boot-test"))]
        let boot_procs: &[(&str, i32)] = &[
            ("/sbin/pm", PM_PROC_NR),           // Process Manager
            ("/sbin/rs", RS_PROC_NR),           // Reincarnation Server
            ("/sbin/vfs", VFS_PROC_NR),         // Virtual File System
            ("/sbin/ramdisk", RAMDISK_PROC_NR), // RAM disk block driver
            ("/sbin/vm", VM_PROC_NR),           // Virtual Memory
            ("/sbin/mfs", MFS_PROC_NR),         // Memory File System
            ("/sbin/init", INIT_PROC_NR),       // init
        ];
        #[cfg(feature = "boot-test")]
        let boot_procs: &[(&str, i32)] = &[
            ("/sbin/pm", PM_PROC_NR),           // Process Manager
            ("/sbin/rs", RS_PROC_NR),           // Reincarnation Server
            ("/sbin/vfs", VFS_PROC_NR),         // Virtual File System
            ("/sbin/ramdisk", RAMDISK_PROC_NR), // RAM disk block driver
            ("/sbin/vm", VM_PROC_NR),           // Virtual Memory
            ("/sbin/mfs", MFS_PROC_NR),         // Memory File System
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
                core::ptr::write_volatile(&raw mut (*rp).p_priority, 5i8);
                core::ptr::write_volatile(&raw mut (*rp).p_quantum_size_ms, 50u32);
                core::ptr::write_volatile(&raw mut (*rp).p_cpu_time_left, 50_000_000);
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

        // Send a boot notification to PM before starting the scheduler.
        // This notification will be pending when PM calls RECEIVE, causing
        // mini_receive to build a notification message and deliver it.
        unsafe {
            kernel::ipc::mini_notify(arch_common::com::RS_PROC_NR, arch_common::com::PM_PROC_NR);
        }

        // Enqueue each process that is runnable.
        for &(_, proc_nr) in boot_procs {
            let rp = kernel::table::proc_addr(proc_nr);
            unsafe {
                let old_flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                let cleared = old_flags
                    & !(kernel::proc::RtsFlags::BOOTINHIBIT.bits()
                        | kernel::proc::RtsFlags::SLOT_FREE.bits());
                if cleared == 0 {
                    kernel::sched::enqueue(rp);
                }
            }
        }

        // Set the current process pointer to the first one.
        unsafe {
            arch_x86_64::cpulocals::set_cpulocal_proc_ptr(first_proc as *mut core::ffi::c_void);
        }

        serial_write("  scheduler starting...\r\n");

        // Mask the timer IRQ and serial IRQ to prevent crashes when the
        // timer interrupt fires in ring 3. The timer ISR (IDT vector 32)
        // and PIT init are installed above, but the ISR path causes a #GP
        // when the timer actually fires. This is a workaround — enabling
        // preemptive multitasking requires debugging the timer ISR path
        // (likely a missing swapgs or GS-relative memory access issue in
        // clock::timer_int_handler or the apic timer_isr_entry asm).
        unsafe {
            // Mask IRQ 0 (timer) and IRQ 4 (serial) on master PIC (port 0x21).
            let mask: u8;
            core::arch::asm!(
                "in al, dx",
                out("al") mask,
                in("dx") 0x21u16,
                options(nomem, nostack),
            );
            core::arch::asm!(
                "out dx, al",
                in("al") mask | 0x11u8,
                in("dx") 0x21u16,
                options(nomem, nostack),
            );
        }

        // Jump to the first process via restore().
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
/// The original user RSP = saved_ptr + 112 (14 pushes × 8 bytes).
///
/// # Safety
///
/// `saved` must point to a valid kernel-stack save area pushed by
/// `syscall_entry`. `rp` must point to a valid `Proc`.
unsafe fn save_proc_regs(rp: *mut kernel::proc::Proc, saved: *const u64) {
    // x86_64 TrapFrame byte offsets (each field is 8 bytes):
    //   0: rax,   8: rbx,  16: rcx,  24: rdx,  32: rsi,  40: rdi
    //  48: r8,   56: r9,   64: r10,  72: r11,  80: r12,  88: r13
    //  96: r14, 104: r15, 160: rip, 168: rsp, 176: rflags
    let frame = unsafe { &mut (*rp).p_reg };
    unsafe {
        // Use volatile writes so the compiler cannot optimize away these
        // register saves. The restore() function reads them via naked asm
        // that the compiler cannot see, so without volatile the writes
        // would appear dead and get eliminated.
        let regs = [
            (0usize, *saved.add(0)),    // rax
            (8usize, *saved.add(1)),    // rbx
            (16usize, *saved.add(2)),   // rcx (RIP via sysretq)
            (24usize, *saved.add(3)),   // rdx
            (32usize, *saved.add(4)),   // rsi
            (40usize, *saved.add(5)),   // rdi
            (48usize, *saved.add(6)),   // r8
            (56usize, *saved.add(7)),   // r9
            (64usize, *saved.add(8)),   // r10 (RFLAGS via sysretq)
            (72usize, *saved.add(9)),   // r11
            (80usize, *saved.add(10)),  // r12
            (88usize, *saved.add(11)),  // r13
            (96usize, *saved.add(12)),  // r14
            (104usize, *saved.add(13)), // r15
        ];
        for (offset, val) in regs {
            let bytes = val.to_ne_bytes();
            for (i, b) in bytes.iter().enumerate() {
                core::ptr::write_volatile(frame.as_mut_ptr().add(offset + i), *b);
            }
        }
        // RSP = saved_ptr + 112 (14 pushes × 8 bytes)
        let rsp = (saved as u64) + 112;
        for (i, b) in rsp.to_ne_bytes().iter().enumerate() {
            core::ptr::write_volatile(frame.as_mut_ptr().add(168 + i), *b);
        }
        // RIP = RCX value (pushed as arg 2), RFLAGS = R11 value (pushed as arg 9)
        let rip = *saved.add(2);
        let rflags = *saved.add(9);
        for (i, b) in rip.to_ne_bytes().iter().enumerate() {
            core::ptr::write_volatile(frame.as_mut_ptr().add(160 + i), *b);
        }
        for (i, b) in rflags.to_ne_bytes().iter().enumerate() {
            core::ptr::write_volatile(frame.as_mut_ptr().add(176 + i), *b);
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
        let result = kernel::syscall::dispatch_basic_syscall(rp, nr, &args);
        core::ptr::write_volatile(saved as *mut u64, result as u64);

        // Save the current process's register state, UNLESS this was
        // SYS_EXEC_REPLACE (61), which replaces the CURRENT process. In
        // that case p_reg already has the new entry point and stack from
        // exec_initramfs_for_replace(), and save_proc_regs would overwrite
        // them with the OLD process state.
        //
        // SYS_EXEC_TARGET (62) is different — PM calls it on behalf of a
        // CHILD process. The current process (PM) is NOT being replaced,
        // so its registers must be saved normally.
        let is_exec = nr == 61;
        if !is_exec {
            save_proc_regs(rp, saved);
        }

        // Pick the next runnable process.
        if let Some(next) = kernel::sched::pick_proc() {
            if next != rp || is_exec {
                // Deliver any pending IPC message to the target process's
                // user buffer and set RAX to the source endpoint.
                unsafe { deliver_msg(next) };
                arch_x86_64::cpulocals::set_cpulocal_proc_ptr(next as *mut core::ffi::c_void);
                // Switch to the new process — never returns.
                arch_x86_64::asm::restore(next as *const u8);
            } else {
                // Same process: deliver pending message before returning.
                unsafe { deliver_msg(rp) };
            }
        } else {
            // No runnable processes — all blocked on IPC.
            let pm_proc = kernel::table::proc_addr(arch_common::com::PM_PROC_NR);
            if !pm_proc.is_null() {
                let _ = kernel::ipc::mini_notify(
                    arch_common::com::RS_PROC_NR,
                    arch_common::com::PM_PROC_NR,
                );
                if let Some(next) = kernel::sched::pick_proc() {
                    unsafe { deliver_msg(next) };
                    arch_x86_64::cpulocals::set_cpulocal_proc_ptr(next as *mut core::ffi::c_void);
                    arch_x86_64::asm::restore(next as *const u8);
                }
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
unsafe fn deliver_msg(rp: *mut kernel::proc::Proc) {
    unsafe {
        let has_deliver = (*rp)
            .p_misc_flags
            .load(core::sync::atomic::Ordering::Relaxed)
            & kernel::proc::MiscFlags::DELIVERMSG.bits()
            != 0;
        if has_deliver {
            kernel::ipc::delivermsg(rp);
            // Read source endpoint from delivered message header (bytes 0-3).
            let src_ep = i32::from_le_bytes([
                (*rp).p_delivermsg[0],
                (*rp).p_delivermsg[1],
                (*rp).p_delivermsg[2],
                (*rp).p_delivermsg[3],
            ]);
            kernel::hal::write_retval(&mut (*rp).p_reg, src_ep as u64);
            (*rp).p_misc_flags.fetch_and(
                !kernel::proc::MiscFlags::DELIVERMSG.bits(),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
    }
}

// Serial I/O — available in all build modes (no-op in test mode)

/// Halt the CPU forever (fallback if boot fails).
#[cfg(not(test))]
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
        core::arch::asm!("out dx, al", in("dx") port + 2, in("al") 0xC7u8, options(nomem, nostack));
        core::arch::asm!("out dx, al", in("dx") port + 4, in("al") 0x0Bu8, options(nomem, nostack));
    }
}

// serial_write, serial_putc, and print! macro are now in kernel_boot lib crate

/// Panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    hlt_loop()
}
