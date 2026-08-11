//! AArch64 kernel boot binary entry point.
//!
//! Build with: `cargo build -p kernel-boot --bin kernel-boot-aarch64
//!   --target aarch64-unknown-minix`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![allow(static_mut_refs)]
#![cfg(target_arch = "aarch64")]

#[cfg(not(test))]
use core::panic::PanicInfo;

use core::arch::global_asm;

// _start entry point — called by QEMU on virt machine.
// The kernel image is loaded at the start of RAM (0x40000000) by QEMU's
// -kernel flag. The linker script places .text.boot first.
global_asm!(
    r#"
.section .text.boot, "ax"
.globl _start

_start:
    // Preserve the DTB pointer (x0) from QEMU; x0/x1 are clobbered below.
    mov     x19, x0

    // Set up a stack near the top of RAM (256MB QEMU virt, top at 0x50000000).
    ldr     x0, =__stack_top
    mov     sp, x0

    // Clear BSS
    ldr     x0, =__bss_start
    ldr     x1, =__bss_end
    cmp     x0, x1
    b.ge    2f
1:
    str     xzr, [x0], #8
    cmp     x0, x1
    b.lt    1b
2:

    // Call kmain(dtb_ptr)
    mov     x0, x19
    bl      kmain

    // Should never reach here
    wfi
    b       _start
"#
);

// Symbols defined by the linker script.
unsafe extern "C" {
    static __bss_start: u8;
    static __bss_end: u8;
    static __stack_top: u8;
}

/// Serial output helper — uses PL011 UART directly during early boot.
fn serial_write(s: &str) {
    for &b in s.as_bytes() {
        // PL011 UART at 0x09000000.
        const UART_DR: usize = 0x0900_0000;
        const UART_FR: usize = 0x0900_0000 + 0x18;
        const FR_TXFF: u32 = 1 << 5;
        unsafe {
            while (core::ptr::read_volatile(UART_FR as *const u32) & FR_TXFF) != 0 {
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(UART_DR as *mut u32, b as u32);
        }
    }
}

#[cfg(feature = "integration-tests")]
fn serial_putc(c: u8) {
    const UART_DR: usize = 0x0900_0000;
    const UART_FR: usize = 0x0900_0000 + 0x18;
    const FR_TXFF: u32 = 1 << 5;
    unsafe {
        while (core::ptr::read_volatile(UART_FR as *const u32) & FR_TXFF) != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UART_DR as *mut u32, c as u32);
    }
}

/// AArch64 kernel main entry.
///
/// # Safety
///
/// Must be called once on the boot CPU in EL1 (or EL2), with MMU disabled.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmain(arg_dtb: u64) -> ! {
    // QEMU virt may start in EL2. Drop to EL1 if needed, preserving the DTB
    // pointer (x0) across the drop.
    let dtb_ptr: u64;
    unsafe {
        core::arch::asm!(
            "mov {dtb}, x0",
            "mrs x0, CurrentEL",
            "cmp x0, #8",       // Check if EL2 (0b1000 = 8)
            "b.ne 1f",
            // Drop from EL2 to EL1.
            "mov x1, #0x3C5",   // EL1h, AArch64, DAIF masked
            "msr spsr_el2, x1",
            "adr x1, 1f",
            "msr elr_el2, x1",
            "eret",
            "1:",
            dtb = out(reg) dtb_ptr,
            inlateout("x0") arg_dtb => _,
            out("x1") _,
        );
    }

    // Enable FP/SIMD before any Rust code (compiler may use NEON for memcpy).
    unsafe {
        core::arch::asm!("msr cpacr_el1, {val}", val = in(reg) (3u64 << 20), options(nomem, nostack));
    }

    // Detect RAM from the DTB (QEMU virt passes it in x0); cap at the boot
    // identity map, which covers 0..32 GiB (32 one-GiB blocks in enable_mmu),
    // so RAM at 0x40000000 is detectable up to 31 GiB.
    #[cfg(not(feature = "integration-tests"))]
    let (mem_base, mem_size) =
        if let Some(info) = unsafe { arch_common::fdt::parse_fdt_memory(dtb_ptr as *const u8) } {
            info
        } else {
            // Fallback: assume standard QEMU virt layout with 256MB RAM
            (0x40000000u64, 256 * 1024 * 1024)
        };
    #[cfg(feature = "integration-tests")]
    let (mem_base, mem_size) = (0x40000000u64, 256 * 1024 * 1024);
    let _ = dtb_ptr;
    let mem_size = mem_size.min(0x8_0000_0000u64.saturating_sub(mem_base));

    // Page-aligned end-of-kernel estimate.
    // The linker script places the kernel at 0x40080000 (512KB into RAM)
    // The kernel binary with embedded initramfs and minixfs is ~12 MB.
    // Pad to 16 MB for safety.
    let kernel_end = 0x4008_0000u64 + 0x100_0000u64; // +16MB

    // Initialize physical memory allocator.
    serial_write("initializing allocator...\r\n");
    let mut mmap = arch_aarch64::alloc::PhysicalMemoryMap::new();
    if kernel_end < mem_base + mem_size {
        mmap.add(kernel_end, mem_base + mem_size);
    }
    unsafe {
        arch_aarch64::alloc::init_allocator(&mmap);
    }
    kernel_boot::print_memory_banner(mem_size, mmap.total_available());
    serial_write("allocator ready\r\n");

    // Set up VBAR_EL1 (exception vector table).
    let vbar = arch_aarch64::exception::vector_table_addr();
    unsafe {
        core::arch::asm!("msr vbar_el1, {vbar}", vbar = in(reg) vbar, options(nomem, nostack));
    }

    // Initialize per-CPU data.
    unsafe {
        arch_aarch64::cpulocals::init_cpulocals();
        kernel::panic::mark_cpulocals_ready();
    }

    serial_write("\r\nHello MINIX/AArch64!\r\n");

    // Initialize kernel subsystems.
    kernel::init();

    // Initialize the process table, kernel call handlers, and IPC syscalls.
    serial_write("  initializing boot processes...\r\n");
    unsafe {
        kernel::table::proc_init();
        kernel::system::system_init();
    }

    // Register the Proc.p_tls offset so switch_to_user loads each thread's
    // tpidr_el0 (TLS thread pointer) on context switch.
    unsafe {
        arch_aarch64::switch::set_tls_tpidr_offset(
            core::mem::offset_of!(kernel::proc::Proc, p_tls) as u64,
        );
    }

    // Initialize basic userspace syscall handlers.
    unsafe {
        kernel::syscall::init_basic_syscalls();
    }
    #[cfg(feature = "boot-test")]
    unsafe {
        kernel::syscall::register_basic_syscall(60, kernel_boot::boot_test_syscall_handler);
    }

    // Register syscall handler.
    unsafe fn aarch64_syscall_handler(nr: usize, args: &[u64; 6]) -> i64 {
        let caller = arch_aarch64::hal::current_proc();
        unsafe {
            kernel::syscall::dispatch_basic_syscall(caller as *mut kernel::proc::Proc, nr, args)
        }
    }
    arch_aarch64::exception::register_syscall_handler(aarch64_syscall_handler);

    // Register post-syscall hook for scheduler context switch.
    unsafe fn aarch64_post_syscall(frame: &mut [u8; 288]) {
        let caller = arch_aarch64::hal::current_proc() as *mut kernel::proc::Proc;
        if caller.is_null() {
            return;
        }
        // Check if process context was replaced (e.g., by exec).
        let mf = unsafe {
            (*caller)
                .p_misc_flags
                .load(core::sync::atomic::Ordering::Relaxed)
        };
        if mf & kernel::proc::MiscFlags::CONTEXT_SET.bits() != 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &raw const (*caller).p_reg as *const u8,
                    frame.as_mut_ptr(),
                    288,
                );
                let new_cr3 = (*caller).p_seg.p_cr3;
                if new_cr3 != 0 {
                    kernel::hal::write_cr3(new_cr3);
                }
                (*caller).p_misc_flags.fetch_and(
                    !kernel::proc::MiscFlags::CONTEXT_SET.bits(),
                    core::sync::atomic::Ordering::SeqCst,
                );
            }
            return;
        }

        let rts = unsafe {
            (*caller)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed)
        };
        // Handle PREEMPTED.
        if rts & kernel::proc::RtsFlags::PREEMPTED.bits() != 0 {
            let cleared = rts & !kernel::proc::RtsFlags::PREEMPTED.bits();
            unsafe {
                (*caller)
                    .p_rts_flags
                    .store(cleared, core::sync::atomic::Ordering::Relaxed);
            }
            if cleared == 0 {
                unsafe {
                    kernel::sched::remove_from_queue(caller);
                    kernel::sched::enqueue(caller);
                };
            }
        }

        if rts != 0 {
            // Save current process's registers from frame to p_reg.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    frame.as_ptr(),
                    &raw mut (*caller).p_reg as *mut u8,
                    288,
                );
            }

            if let Some(next_proc) = unsafe { kernel::sched::pick_proc() } {
                unsafe {
                    let mf = (*next_proc)
                        .p_misc_flags
                        .load(core::sync::atomic::Ordering::Relaxed);
                    if mf & kernel::proc::MiscFlags::DELIVERMSG.bits() != 0 {
                        kernel::ipc::delivermsg(next_proc);
                        let src_ep = i32::from_le_bytes([
                            (*next_proc).p_delivermsg[0],
                            (*next_proc).p_delivermsg[1],
                            (*next_proc).p_delivermsg[2],
                            (*next_proc).p_delivermsg[3],
                        ]);
                        kernel::hal::write_retval(&mut (*next_proc).p_reg, src_ep as u64);
                        (*next_proc).p_misc_flags.fetch_and(
                            !kernel::proc::MiscFlags::DELIVERMSG.bits(),
                            core::sync::atomic::Ordering::Relaxed,
                        );
                    }
                    core::ptr::copy_nonoverlapping(
                        &raw const (*next_proc).p_reg as *const u8,
                        frame.as_mut_ptr(),
                        288,
                    );
                    // tpidr_el0 is not part of the trap frame: reload the
                    // new thread's thread pointer on every switch so its
                    // TLS accesses land in its own block (matches x86's
                    // FS_BASE reload in restore() and RISC-V's tp in the
                    // frame). Skipped when unset, mirroring switch_to_user.
                    let tls = (*next_proc).p_tls;
                    if tls != 0 {
                        arch_aarch64::hal::set_tls_current(tls);
                    }
                    (*next_proc).p_misc_flags.fetch_and(
                        !kernel::proc::MiscFlags::CONTEXT_SET.bits(),
                        core::sync::atomic::Ordering::SeqCst,
                    );
                    let new_cr3 = (*next_proc).p_seg.p_cr3;
                    if new_cr3 != 0 {
                        kernel::hal::write_cr3(new_cr3);
                    }
                    arch_aarch64::cpulocals::set_current_proc(next_proc as u64);
                }
            } else {
                // All processes are blocked — idle until an interrupt makes
                // one runnable (matching x86's sti; hlt; cli; continue).
                // Do NOT notify PM here: that self-wakeup makes PM spin on
                // notifications (GETKSIG) while real IPC messages — such as
                // PM_FORK — sit undelivered, livelocking the system.
                loop {
                    // Unmask IRQ so wfi sleeps until the timer fires, then
                    // re-mask before resuming kernel work.
                    unsafe {
                        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
                        arch_aarch64::hal::hlt();
                        core::arch::asm!("msr daifset, #2", options(nomem, nostack));
                    }
                    if let Some(next_proc) = unsafe { kernel::sched::pick_proc() } {
                        unsafe {
                            let mf = (*next_proc)
                                .p_misc_flags
                                .load(core::sync::atomic::Ordering::Relaxed);
                            if mf & kernel::proc::MiscFlags::DELIVERMSG.bits() != 0 {
                                kernel::ipc::delivermsg(next_proc);
                                let src_ep = i32::from_le_bytes([
                                    (*next_proc).p_delivermsg[0],
                                    (*next_proc).p_delivermsg[1],
                                    (*next_proc).p_delivermsg[2],
                                    (*next_proc).p_delivermsg[3],
                                ]);
                                kernel::hal::write_retval(&mut (*next_proc).p_reg, src_ep as u64);
                                (*next_proc).p_misc_flags.fetch_and(
                                    !kernel::proc::MiscFlags::DELIVERMSG.bits(),
                                    core::sync::atomic::Ordering::Relaxed,
                                );
                            }
                            core::ptr::copy_nonoverlapping(
                                &raw const (*next_proc).p_reg as *const u8,
                                frame.as_mut_ptr(),
                                288,
                            );
                            let tls = (*next_proc).p_tls;
                            if tls != 0 {
                                arch_aarch64::hal::set_tls_current(tls);
                            }
                            (*next_proc).p_misc_flags.fetch_and(
                                !kernel::proc::MiscFlags::CONTEXT_SET.bits(),
                                core::sync::atomic::Ordering::SeqCst,
                            );
                            let new_cr3 = (*next_proc).p_seg.p_cr3;
                            if new_cr3 != 0 {
                                kernel::hal::write_cr3(new_cr3);
                            }
                            arch_aarch64::cpulocals::set_current_proc(next_proc as u64);
                        }
                        break;
                    }
                }
            }
        } else {
            // Caller stayed runnable — flush any message this syscall
            // staged in p_delivermsg (mini_receive's try_async path sets
            // DELIVERMSG and returns OK). x86's syscall_handler_c delivers
            // to a still-runnable caller; without this, the message sits
            // staged forever (observed: VFS never saw VFS_PM_FORK and the
            // shell's fork hung).
            let mf = unsafe {
                (*caller)
                    .p_misc_flags
                    .load(core::sync::atomic::Ordering::Relaxed)
            };
            if mf & kernel::proc::MiscFlags::DELIVERMSG.bits() != 0 {
                unsafe {
                    kernel::ipc::delivermsg(caller);
                    let src_ep = i32::from_le_bytes([
                        (*caller).p_delivermsg[0],
                        (*caller).p_delivermsg[1],
                        (*caller).p_delivermsg[2],
                        (*caller).p_delivermsg[3],
                    ]);
                    // Overwrite the syscall's OK return in x0 (offset 0)
                    // with the source endpoint.
                    let ret = src_ep as u64;
                    frame[0..8].copy_from_slice(&ret.to_ne_bytes());
                    (*caller).p_misc_flags.fetch_and(
                        !kernel::proc::MiscFlags::DELIVERMSG.bits(),
                        core::sync::atomic::Ordering::Relaxed,
                    );
                }
            }
        }
    }
    arch_aarch64::exception::register_post_syscall_hook(aarch64_post_syscall);

    // Register UART input callback.
    unsafe fn uart_input_cb(byte: u8) {
        unsafe { kernel::ser_input::push_byte(byte) };
    }
    arch_aarch64::exception::register_uart_input_callback(uart_input_cb);

    // Register timer callback for preemptive scheduling.
    unsafe fn aarch64_timer_callback(frame: &mut [u8; 288]) {
        // Full per-tick accounting (monotonic/realtime, virtual timers, load
        // average, quantum accounting via context_stop → proc_no_time →
        // notify_scheduler) in BOTH kernel and user mode, matching x86's
        // timer_int_handler. The SPSR check below then limits the
        // save/pick/switch to user-mode interrupts.
        unsafe { kernel::clock::timer_int_handler() };

        // Timer already acknowledged by IRQ handler via timer_irq_ack().

        // Skip if this interrupt fired in kernel mode (not user mode).
        // SPSR_EL1 M[3:0] = 0 means EL0t (user mode).
        let spsr = u64::from_ne_bytes(frame[264..272].try_into().unwrap());
        if (spsr & 0xF) != 0 {
            return;
        }

        let caller = arch_aarch64::hal::current_proc() as *mut kernel::proc::Proc;
        if caller.is_null() {
            return;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                &raw mut (*caller).p_reg as *mut u8,
                288,
            );
        }
        if let Some(next_proc) = unsafe { kernel::sched::pick_proc() } {
            if next_proc != caller {
                unsafe {
                    let mf = (*next_proc)
                        .p_misc_flags
                        .load(core::sync::atomic::Ordering::Relaxed);
                    if mf & kernel::proc::MiscFlags::DELIVERMSG.bits() != 0 {
                        kernel::ipc::delivermsg(next_proc);
                        let src_ep = i32::from_le_bytes([
                            (*next_proc).p_delivermsg[0],
                            (*next_proc).p_delivermsg[1],
                            (*next_proc).p_delivermsg[2],
                            (*next_proc).p_delivermsg[3],
                        ]);
                        kernel::hal::write_retval(&mut (*next_proc).p_reg, src_ep as u64);
                        (*next_proc).p_misc_flags.fetch_and(
                            !kernel::proc::MiscFlags::DELIVERMSG.bits(),
                            core::sync::atomic::Ordering::Relaxed,
                        );
                    }
                    core::ptr::copy_nonoverlapping(
                        &raw const (*next_proc).p_reg as *const u8,
                        frame.as_mut_ptr(),
                        288,
                    );
                    let tls = (*next_proc).p_tls;
                    if tls != 0 {
                        arch_aarch64::hal::set_tls_current(tls);
                    }
                    (*next_proc).p_misc_flags.fetch_and(
                        !kernel::proc::MiscFlags::CONTEXT_SET.bits(),
                        core::sync::atomic::Ordering::SeqCst,
                    );
                    let new_cr3 = (*next_proc).p_seg.p_cr3;
                    if new_cr3 != 0 {
                        kernel::hal::write_cr3(new_cr3);
                    }
                    arch_aarch64::cpulocals::set_current_proc(next_proc as u64);
                }
            }
        }
    }
    arch_aarch64::exception::register_timer_callback(aarch64_timer_callback);

    // Register page fault handler.
    unsafe fn aarch64_pf_handler(fault_addr: u64, error_code: u32) -> i32 {
        unsafe { kernel::vm::handle_page_fault(fault_addr, error_code) }
    }
    arch_aarch64::exception::register_page_fault_handler(aarch64_pf_handler);

    #[cfg(feature = "integration-tests")]
    {
        // Enable the MMU so copy_from_user / delivermsg perform real
        // page-table walks (BOOT_CR3 non-zero) instead of silently skipping
        // the copy. This unblocks the SENDREC payload assertions
        // (sendrec_direct, sendrec_reply_cycle) on AArch64.
        serial_write("  enabling MMU...\r\n");
        unsafe {
            enable_mmu();
            let boot_ttbr0: u64;
            core::arch::asm!("mrs {v}, ttbr0_el1", v = out(reg) boot_ttbr0, options(nomem, nostack));
            arch_aarch64::BOOT_CR3.store(
                boot_ttbr0 & 0x0000_FFFF_FFFF_F000,
                core::sync::atomic::Ordering::Relaxed,
            );
        }
        serial_write("  MMU enabled\r\n");

        serial_write("Running AArch64 integration tests...\r\n");
        let mut failures = kernel::tests::run_all();
        failures += unsafe { test_cntpct_advances() };
        failures += unsafe { test_pl011_output() };
        serial_write("\r\n");
        if failures == 0 {
            serial_write("ALL TESTS PASSED\r\n");
        } else {
            serial_write("FAILURES: ");
            // Print failure count as decimal digits
            let tens = failures / 10;
            let ones = failures % 10;
            if tens > 0 {
                serial_putc(b'0' + (tens as u8));
            }
            serial_putc(b'0' + (ones as u8));
            serial_write("\r\n");
        }
        // Exit QEMU via PSCI SYSTEM_OFF (HVC conduit); the exit code is
        // always 0, so the recipe checks the serial log for the result.
        arch_aarch64::hal::qemu_exit(failures);
    }

    #[cfg(not(feature = "integration-tests"))]
    {
        serial_write("  enabling MMU...\r\n");
        unsafe {
            enable_mmu();
            let boot_ttbr0: u64;
            core::arch::asm!("mrs {v}, ttbr0_el1", v = out(reg) boot_ttbr0, options(nomem, nostack));
            arch_aarch64::BOOT_CR3.store(
                boot_ttbr0 & 0x0000_FFFF_FFFF_F000,
                core::sync::atomic::Ordering::Relaxed,
            );
        }
        serial_write("  MMU enabled\r\n");

        // Set CPU frequency so clock::ms_2_cpu_time converts ms to cntpct_el0
        // cycles (QEMU virt generic timer = 62.5 MHz, matching
        // arch_aarch64::timer::TIMER_FREQ_HZ). Without a non-zero frequency,
        // p_cpu_time_left stays 0 after SYS_SCHEDULE and the first timer
        // tick calls proc_no_time → notify_scheduler on every tick.
        kernel::glo::cpu_set_freq(0, 62_500_000);

        // Initialize timer (uses CNTP system registers, no GIC needed).
        unsafe {
            arch_aarch64::timer::init_timer();
        }

        use arch_common::com::*;

        serial_write("  loading boot processes...\r\n");

        // When boot-test is active, INIT is excluded so the test completes
        // before any user process starts (same as x86 main.rs / riscv64.rs).
        #[cfg(not(feature = "boot-test"))]
        let boot_procs: &[(&str, i32)] = &[
            ("/sbin/ds", DS_PROC_NR),
            ("/sbin/rs", RS_PROC_NR),
            ("/sbin/pm", PM_PROC_NR),
            ("/sbin/sched", SCHED_PROC_NR),
            ("/sbin/vfs", VFS_PROC_NR),
            ("/sbin/vm", VM_PROC_NR),
            ("/sbin/ramdisk", RAMDISK_PROC_NR),
            ("/sbin/virtio_blk", VIRTIO_BLK_PROC_NR),
            ("/sbin/virtio_net", VIRTIO_NET_PROC_NR),
            ("/sbin/net", NET_PROC_NR),
            ("/sbin/mfs", MFS_PROC_NR),
            ("/sbin/pfs", PFS_PROC_NR),
            ("/sbin/tty", TTY_PROC_NR),
            ("/sbin/init", INIT_PROC_NR),
        ];
        #[cfg(feature = "boot-test")]
        let boot_procs: &[(&str, i32)] = &[
            ("/sbin/ds", DS_PROC_NR),
            ("/sbin/rs", RS_PROC_NR),
            ("/sbin/pm", PM_PROC_NR),
            ("/sbin/sched", SCHED_PROC_NR),
            ("/sbin/vfs", VFS_PROC_NR),
            ("/sbin/vm", VM_PROC_NR),
            ("/sbin/ramdisk", RAMDISK_PROC_NR),
            ("/sbin/virtio_blk", VIRTIO_BLK_PROC_NR),
            ("/sbin/virtio_net", VIRTIO_NET_PROC_NR),
            ("/sbin/net", NET_PROC_NR),
            ("/sbin/mfs", MFS_PROC_NR),
            ("/sbin/pfs", PFS_PROC_NR),
            ("/sbin/tty", TTY_PROC_NR),
        ];

        #[cfg(not(feature = "boot-test"))]
        let mut boot_infos: [core::mem::MaybeUninit<kernel_boot::boot_init::InitInfo>; 14] =
            unsafe { core::mem::zeroed() };
        #[cfg(feature = "boot-test")]
        let mut boot_infos: [core::mem::MaybeUninit<kernel_boot::boot_init::InitInfo>; 13] =
            unsafe { core::mem::zeroed() };
        for (i, &(path, proc_nr)) in boot_procs.iter().enumerate() {
            let info = match unsafe {
                kernel_boot::boot_init::load_and_prepare_proc(path, proc_nr, &[path])
            } {
                Some(info) => info,
                None => {
                    serial_write("  FAILED loading ");
                    serial_write(path);
                    serial_write("\r\n");
                    arch_aarch64::hal::halt();
                }
            };
            boot_infos[i] = core::mem::MaybeUninit::new(info);
        }

        serial_write("  creating per-process page tables...\r\n");

        let mut first_proc: *mut kernel::proc::Proc = core::ptr::null_mut();
        for (i, &(path, proc_nr)) in boot_procs.iter().enumerate() {
            let rp = kernel::table::proc_addr(proc_nr);
            if i == 0 {
                first_proc = rp;
            }

            let info = unsafe { boot_infos[i].assume_init_ref() };

            let pt_phys = unsafe {
                kernel_boot::boot_init::boot_create_restricted_page_table(
                    info.code_start,
                    info.code_end,
                    info.phys_code_base,
                    info.stack_start,
                    info.stack_end,
                    info.phys_stack_base,
                    proc_nr == VIRTIO_BLK_PROC_NR || proc_nr == VIRTIO_NET_PROC_NR,
                )
            };
            let pt_phys = match pt_phys {
                Some(p) => p,
                None => {
                    serial_write("  FAILED: page table for ");
                    serial_write(path);
                    serial_write("\r\n");
                    arch_aarch64::hal::halt();
                }
            };

            unsafe {
                core::ptr::write_volatile(&raw mut (*rp).p_seg.p_cr3, pt_phys);
                // proc_init already assigned a priv slot for every boot
                // image entry; get_priv is only a fallback for processes
                // without one. init keeps the shared USER slot.
                if (*rp).p_priv.is_null() {
                    let _ = kernel::system::get_priv(rp);
                }
                if !(*rp).p_priv.is_null() {
                    (*(*rp).p_priv).s_phys_delta =
                        (info.phys_code_base as i64) - (info.code_start as i64);
                }
                core::ptr::write_volatile(&raw mut (*rp).p_priority, 5i8);
                core::ptr::write_volatile(&raw mut (*rp).p_quantum_size_ms, 50u32);
                core::ptr::write_volatile(&raw mut (*rp).p_cpu_time_left, 50_000_000);
            }

            // Map the pre-allocated brk heap window (heap base .. +1 MiB) for
            // every boot process. Servers allocate from this range via the brk
            // syscall (minix_alloc_zeroed), so the pages must be present.
            {
                let user_flags = kernel::hal::pte_user_flags();
                let brk_va_start = kernel::hal::user_heap_base();
                let brk_va_end = brk_va_start + 0x100000u64;
                let brk_pages = ((brk_va_end - brk_va_start) / 4096) as usize;
                let brk_phys = match unsafe { kernel::hal::alloc_phys_contig(brk_pages) } {
                    Some(base) => base,
                    None => {
                        serial_write("  FAILED: out of memory for brk heap\r\n");
                        arch_aarch64::hal::halt();
                    }
                };
                for j in 0..brk_pages {
                    let va = brk_va_start + (j as u64) * 4096;
                    let pa = brk_phys + (j as u64) * 4096;
                    if unsafe { kernel::pagetable::map_page(pt_phys, va, pa, user_flags) }.is_err()
                    {
                        serial_write("  FAILED: brk page mapping\r\n");
                        arch_aarch64::hal::halt();
                    }
                }
            }

            // Boot image mapping for the ramdisk driver server (served to
            // filesystem servers via the BDEV protocol).
            if proc_nr == RAMDISK_PROC_NR {
                let image = kernel::minixfs::minixfs_image();
                let image_len = kernel::minixfs::minixfs_image_len();
                if image_len > 0 {
                    let pages = image_len.div_ceil(4096);
                    let ramdisk_phys = match unsafe { kernel::hal::alloc_phys_contig(pages) } {
                        Some(base) => base,
                        None => {
                            serial_write("  FAILED: out of memory for RAM disk\r\n");
                            arch_aarch64::hal::halt();
                        }
                    };
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            image.as_ptr(),
                            ramdisk_phys as *mut u8,
                            image_len,
                        );
                    }
                    let user_flags = kernel::hal::pte_user_flags();
                    for j in 0..pages {
                        let va = arch_common::com::RAMDISK_IMAGE_VA + (j as u64) * 4096;
                        let pa = ramdisk_phys + (j as u64) * 4096;
                        if unsafe { kernel::pagetable::map_page(pt_phys, va, pa, user_flags) }
                            .is_err()
                        {
                            serial_write("  FAILED: RAM disk page mapping\r\n");
                            arch_aarch64::hal::halt();
                        }
                    }
                    serial_write("  RAM disk mapped for ramdisk server\r\n");
                }
            }

            // Clean D-cache AFTER all mappings so MMU walker sees all PTEs.
            unsafe {
                kernel_boot::boot_init::clean_page_table_cache_aarch64(pt_phys);
            }
        }

        // Restore PUD[0] with proper BBM (Break-Before-Make):
        // The map_page calls in boot_create_restricted_page_table split
        // PUD[0] from a 1GB block into a table. We need to restore it
        // to a block so device memory (GIC at 0x08000000) is accessible.
        unsafe {
            let boot_pgd = kernel::hal::boot_cr3() as *const u64;
            let pud_phys = core::ptr::read_volatile(boot_pgd) & 0x0000_FFFF_FFFF_F000;
            const BLOCK_FLAGS: u64 = 0b01u64 | (0b11 << 8) | (1 << 10);
            // Step 1: invalidate
            core::ptr::write_volatile(pud_phys as *mut u64, 0);
            core::arch::asm!("dsb ish; tlbi vmalle1is; dsb ish; isb", options(nostack));
            // Step 2: write new block
            core::ptr::write_volatile(pud_phys as *mut u64, BLOCK_FLAGS);
            core::arch::asm!("dsb ish; isb", options(nostack));
        }

        // Enable GIC (needed for timer PPI routing, even though the
        // timer ISR acknowledges via system registers).
        unsafe {
            enable_gic();
        }

        // Enable the PL011 RX interrupt so piped bursts drain into the
        // ser_input ring promptly (via el1_irq_handler_c) instead of
        // overrunning the RX FIFO between timer ticks.
        arch_aarch64::hal::enable_rx_interrupt();

        if first_proc.is_null() {
            serial_write("  FAILED: no boot processes found\r\n");
            arch_aarch64::hal::halt();
        }

        // Set boot notification on PM.
        unsafe {
            let pm = kernel::table::proc_addr(arch_common::com::PM_PROC_NR);
            if !pm.is_null() && !(*pm).p_priv.is_null() {
                let rs_priv_id =
                    kernel::r#priv::priv_find_proc_id(arch_common::com::RS_PROC_NR).unwrap_or(0);
                (*(*pm).p_priv).s_notify_pending.set(rs_priv_id);
            }
        }

        serial_write("  enqueuing processes...\r\n");

        for &(_, proc_nr) in boot_procs {
            let rp = kernel::table::proc_addr(proc_nr);
            unsafe {
                let old_flags = (*rp)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed);
                let cleared = old_flags
                    & !(kernel::proc::RtsFlags::BOOTINHIBIT.bits()
                        | kernel::proc::RtsFlags::SLOT_FREE.bits()
                        | kernel::proc::RtsFlags::NO_PRIV.bits());
                if cleared == 0 {
                    // NO_PRIV (the user-proc marker proc_init sets on init)
                    // does not block scheduling; x86's boot enqueue clears
                    // it the same way. Clear before enqueue — this port's
                    // enqueue requires p_rts_flags == 0.
                    (*rp).p_rts_flags.store(
                        old_flags & !kernel::proc::RtsFlags::NO_PRIV.bits(),
                        core::sync::atomic::Ordering::Relaxed,
                    );
                    kernel::sched::enqueue(rp);
                }
            }
        }

        unsafe {
            arch_aarch64::cpulocals::set_current_proc(first_proc as u64);
        }

        serial_write("  scheduler starting...\r\n");

        let next_proc = unsafe { kernel::sched::pick_proc() };
        let next_ptr = match next_proc {
            Some(p) => p,
            None => {
                serial_write("  FAILED: no runnable processes\r\n");
                arch_aarch64::hal::halt();
            }
        };

        // Enable interrupt routing.
        unsafe {
            core::arch::asm!("msr daifclr, #2", options(nomem, nostack)); // Unmask IRQ
        }

        // Invalidate entire I-cache to handle VIPT aliasing between
        // identity VA used for code loading and runtime VA (0x1000000+).
        unsafe {
            core::arch::asm!("ic ialluis", options(nostack));
            core::arch::asm!("dsb ish", options(nostack));
            core::arch::asm!("isb", options(nostack));
        }

        serial_write("  switching to userspace...\r\n");
        unsafe {
            arch_aarch64::switch::switch_to_user(next_ptr as *const u8);
        }
        // switch_to_user never returns — we should never reach here.
        #[allow(unreachable_code)]
        {
            serial_write("  FAILED: switch_to_user returned!\r\n");
            arch_aarch64::hal::halt();
        }
    }
}

/// AArch64 hardware tests: generic timer (CNTP) + PL011 UART.
///
/// Runs inside the integration build after the shared kernel suite. These
/// probe the actual device/register paths (via the MMU identity map) rather
/// than the hal wrappers the shared tests use.

/// Generic timer: cntpct_el0 must advance and cntfrq_el0 must report a sane
/// frequency — the hardware-level equivalent of monotonic_advances.
#[cfg(feature = "integration-tests")]
unsafe fn test_cntpct_advances() -> u32 {
    unsafe {
        let freq: u64;
        core::arch::asm!("mrs {v}, cntfrq_el0", v = out(reg) freq, options(nomem, nostack));
        if freq < 1_000_000 || freq > 1_000_000_000 {
            serial_write("  FAIL: cntfrq_el0 unexpected frequency\r\n");
            return 1;
        }
        let t1 = arch_aarch64::hal::read_cycles(); // cntpct_el0
        let mut t2 = t1;
        let mut spins = 0usize;
        while t2 == t1 && spins < 1_000_000 {
            core::hint::spin_loop();
            t2 = arch_aarch64::hal::read_cycles();
            spins += 1;
        }
        if t2 <= t1 {
            serial_write("  FAIL: cntpct_el0 should advance\r\n");
            return 1;
        }
        serial_write("  OK generic timer cntpct_el0 advances\r\n");
    }
    0
}

/// PL011 UART: direct register access (bypassing hal) — the FR register must
/// read as a valid device (not all-ones), and the TX FIFO must accept a byte.
#[cfg(feature = "integration-tests")]
unsafe fn test_pl011_output() -> u32 {
    const PL011_BASE: usize = 0x0900_0000;
    const PL011_DR: usize = PL011_BASE + 0x00;
    const PL011_FR: usize = PL011_BASE + 0x18;
    const FR_TXFF: u32 = 1 << 5;
    unsafe {
        let fr = core::ptr::read_volatile(PL011_FR as *const u32);
        if fr == 0xFFFF_FFFF {
            serial_write("  FAIL: PL011 not present\r\n");
            return 1;
        }
        let mut spins = 0usize;
        while core::ptr::read_volatile(PL011_FR as *const u32) & FR_TXFF != 0 && spins < 100_000 {
            spins += 1;
        }
        core::ptr::write_volatile(PL011_DR as *mut u8, b'@');
        serial_write("  OK PL011 output\r\n");
    }
    0
}

/// Enable the MMU with a minimal identity-mapped page table.
/// Uses 1GB blocks at PUD level — 32 descriptors covering 0..32 GiB.
#[cfg(target_arch = "aarch64")]
unsafe fn enable_mmu() {
    const TABLE_DESC: u64 = 0x3;
    const BLOCK_FLAGS: u64 = 0b01u64 | (0b11 << 8) | (1 << 10);

    // Configure MAIR_EL1 so Attr0 is normal cacheable memory.
    unsafe {
        core::arch::asm!("msr mair_el1, {v}", v = in(reg) 0x44FFu64, options(nostack));
    }

    // Configure MAIR_EL1: memory attribute indirection register.
    // Attr0 = 0xFF: Normal memory, Inner/Outer WB/WA, non-transient, R/W allocate.
    // Attr1 = 0x44: Normal memory, Inner/Outer non-cacheable.
    // Attr2 = 0x00: Device-nGnRnE.
    unsafe {
        core::arch::asm!("msr mair_el1, {v}", v = in(reg) 0x44FFu64, options(nostack));
    }

    let pgd = arch_aarch64::alloc::alloc_phys_page().expect("PGD alloc");
    let pud = arch_aarch64::alloc::alloc_phys_page().expect("PUD alloc");

    // Zero both tables.
    for i in 0..512 {
        unsafe {
            core::ptr::write_volatile((pgd as *mut u64).add(i), 0);
            core::ptr::write_volatile((pud as *mut u64).add(i), 0);
        }
    }

    // PGD[0] → PUD.
    unsafe {
        core::ptr::write_volatile(pgd as *mut u64, pud | TABLE_DESC);
    }

    // PUD[i] = 1 GB normal-memory block at PA i<<30, covering 0..32 GiB.
    // The kernel runs identity-mapped, so this window bounds the physical
    // memory the allocator can hand out (pte_is_valid_phys checks it).
    for i in 0..32u64 {
        unsafe {
            core::ptr::write_volatile((pud as *mut u64).add(i as usize), (i << 30) | BLOCK_FLAGS);
        }
    }

    // Barriers + TLB invalidation + set TTBR0 + enable MMU.
    unsafe {
        core::arch::asm!(
            "dsb  sy",
            "msr  ttbr0_el1, {pgd}",
            "isb",
            "tlbi vmalle1is",
            "dsb  ish",
            "isb",
            pgd = in(reg) pgd,
        );
    }

    unsafe {
        core::arch::asm!(
            "mrs  {tmp}, sctlr_el1",
            "orr  {tmp}, {tmp}, #1",
            "msr  sctlr_el1, {tmp}",
            "isb",
            tmp = out(reg) _,
        );
    }

    unsafe {
        core::arch::asm!("ic   ialluis", "dsb  sy", "isb",);
    }
}

/// Enable GICv2 for timer (PPI 30) and UART (SPI 33) interrupts.
#[cfg(not(feature = "integration-tests"))]
unsafe fn enable_gic() {
    let gicd_base = 0x0800_0000usize;
    let gicc_base = 0x0801_0000usize;

    // Enable GIC distributor.
    unsafe {
        core::ptr::write_volatile(gicd_base as *mut u32, 1); // GICD_CTLR = Enable
    }

    // Enable CPU interface.
    unsafe {
        core::ptr::write_volatile(gicc_base as *mut u32, 1); // GICC_CTLR = Enable
    }

    // Set priority mask to allow all interrupts.
    unsafe {
        core::ptr::write_volatile((gicc_base + 0x04) as *mut u32, 0xFF); // GICC_PMR
    }

    // Enable PPI 30 (timer) in GICD_ISENABLER0.
    unsafe {
        core::ptr::write_volatile((gicd_base + 0x100) as *mut u32, 1 << 30);
    }

    // Enable SPI 33 (UART) in GICD_ISENABLER1.
    unsafe {
        core::ptr::write_volatile((gicd_base + 0x104) as *mut u32, 1 << 1); // SPI 33 = bit 1 of word 1
    }
}

/// Panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::panic::handle(info)
}
