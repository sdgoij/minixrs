//! RISC-V64 kernel boot binary entry point.
//!
//! Build with: `cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![allow(static_mut_refs)]
#![cfg(target_arch = "riscv64")]

#[cfg(not(test))]
use core::panic::PanicInfo;

use core::arch::global_asm;

// _start entry point — called by QEMU/OpenSBI.
// a0 = hart ID, a1 = DTB pointer.
global_asm!(
    r#"
.section .text.boot, "ax"
.globl _start

_start:
    # Set up the boot/trap kernel stack (64 KiB reserved at the start of
    # .bss, so it is in RAM for any guest size). The old fixed 0x8FC00000
    # sat at 252 MiB and faulted before any output below 256 MiB RAM.
    la      sp, __boot_stack_top

    # Clear BSS
    la      t0, __bss_start
    la      t1, __bss_end
    bge     t0, t1, 2f
1:
    sd      zero, 0(t0)
    addi    t0, t0, 8
    blt     t0, t1, 1b
2:

    # Call kmain(hart_id, dtb_ptr)
    mv      a0, a0
    mv      a1, a1
    call    kmain

    # Should never reach here
    wfi
    j       _start
"#
);

// BSS and initramfs symbols are defined by the custom linker script
// (tools/minix-raw-riscv64.ld).

/// Serial output helper.
fn serial_write(s: &str) {
    for &b in s.as_bytes() {
        arch_riscv64::sbi::console_putchar(b);
    }
}

#[cfg(feature = "integration-tests")]
fn serial_putc(c: u8) {
    arch_riscv64::sbi::console_putchar(c);
}

/// RISC-V64 kernel main entry.
///
/// # Safety
///
/// Must be called once on the boot hart in S-mode, with a0=hart_id and a1=dtb_ptr.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmain(hart_id: u64, dtb_ptr: u64) -> ! {
    // Only hart 0 proceeds
    if hart_id != 0 {
        loop {
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack));
            }
        }
    }

    #[cfg(feature = "integration-tests")]
    let _ = dtb_ptr;

    // Parse FDT for memory information
    // (skip FDT parsing for integration tests — uses fixed 256MB fallback)
    #[cfg(not(feature = "integration-tests"))]
    let (mem_base, mem_size) =
        if let Some(info) = unsafe { arch_riscv64::boot::parse_fdt_memory(dtb_ptr as *const u8) } {
            info
        } else {
            // Fallback: assume standard QEMU virt layout with 256MB RAM
            (0x80000000u64, 256 * 1024 * 1024)
        };
    #[cfg(feature = "integration-tests")]
    let (mem_base, mem_size) = (0x80000000u64, 256 * 1024 * 1024);

    // Cap at the boot identity map (32 GiB of 1 GiB huge pages); anything the
    // FDT reports above that isn't mapped and would fault on access.
    let mem_size = mem_size.min(0x8_0000_0000 - mem_base);

    // Page-aligned end-of-kernel estimate.
    // The kernel binary with embedded initramfs and minixfs is ~11 MB.
    // Pad to 14 MB for safety (avoids overlapping the allocator with the
    // kernel image).
    let kernel_end = 0x80200000u64 + 0xE00000u64;

    let mut mmap = arch_riscv64::alloc::PhysicalMemoryMap::new();
    if kernel_end < mem_base + mem_size {
        mmap.add(kernel_end, mem_base + mem_size);
    }
    unsafe {
        arch_riscv64::alloc::init_allocator(&mmap);
    }

    kernel_boot::print_memory_banner(mem_size, mmap.total_available());

    // Set up STVEC to point to the trap vector
    let trap_vec = arch_riscv64::trap_asm::trap_vector_addr();
    unsafe {
        core::arch::asm!("csrw stvec, {addr}", addr = in(reg) trap_vec, options(nomem, nostack));
    }

    // Initialize sscratch to the current stack pointer BEFORE enabling any
    // interrupts.  The trap handler swaps SP with sscratch on EVERY trap;
    // if sscratch holds garbage, the first timer interrupt corrupts the stack.
    unsafe {
        core::arch::asm!("csrw sscratch, sp", options(nomem, nostack));
    }

    // Initialize per-CPU data (tp register)
    unsafe {
        arch_riscv64::cpulocals::init_cpulocals();
        kernel::panic::mark_cpulocals_ready();
    }

    serial_write("\r\nHello MINIX/RISC-V!\r\n");

    // Initialize kernel subsystems
    kernel::init();

    // Initialize the process table, kernel call handlers, and IPC syscalls.
    // These mirror x86_64's boot_init sequence (main.rs).
    unsafe {
        kernel::table::proc_init();
        kernel::system::system_init();
        // IPC syscalls are already registered by init_basic_syscalls below.
    }

    // Initialize basic userspace syscall handlers
    unsafe {
        kernel::syscall::init_basic_syscalls();
    }
    #[cfg(feature = "boot-test")]
    unsafe {
        kernel::syscall::register_basic_syscall(60, kernel_boot::boot_test_syscall_handler);
    }
    unsafe {
        // Wrap the kernel dispatcher to supply the caller from CPU locals.
        unsafe fn riscv_syscall_handler(nr: usize, args: &[u64; 6]) -> i64 {
            let caller = arch_riscv64::hal::current_proc();
            unsafe {
                kernel::syscall::dispatch_basic_syscall(caller as *mut kernel::proc::Proc, nr, args)
            }
        }
        arch_riscv64::trap::register_syscall_handler(riscv_syscall_handler);
    }
    unsafe {
        // Post-syscall hook: if current process is blocked (e.g., on IPC),
        // pick a new runnable process and overwrite the trap frame.
        unsafe fn riscv_post_syscall(frame: &mut [u8; 296]) {
            let caller = arch_riscv64::hal::current_proc() as *mut kernel::proc::Proc;
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
                    // Copy caller's (new) p_reg into the trap frame.
                    core::ptr::copy_nonoverlapping(
                        &raw const (*caller).p_reg as *const u8,
                        frame.as_mut_ptr(),
                        256,
                    );
                    // Restore t6 (x31) from its dedicated slot.
                    frame[248..256].copy_from_slice(&(*caller).p_t6.to_ne_bytes());
                    let p_reg = &raw const (*caller).p_reg;
                    let sepc_bytes = core::ptr::read(p_reg as *const [u8; 8]);
                    frame[256..264].copy_from_slice(&sepc_bytes);
                    let sst_bytes = core::ptr::read(p_reg.add(248) as *const [u8; 8]);
                    frame[264..272].copy_from_slice(&sst_bytes);
                    // Load new page table
                    let new_cr3 = (*caller).p_seg.p_cr3;
                    if new_cr3 != 0 {
                        kernel::hal::write_cr3(new_cr3);
                    }
                    // Clear the flag
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
            // Handle PREEMPTED: clear the flag and re-enqueue if no other
            // blocking flags remain (matching x86-64 syscall return path).
            if rts & kernel::proc::RtsFlags::PREEMPTED.bits() != 0 {
                let cleared = rts & !kernel::proc::RtsFlags::PREEMPTED.bits();
                unsafe {
                    (*caller)
                        .p_rts_flags
                        .store(cleared, core::sync::atomic::Ordering::Relaxed);
                }
                if cleared == 0 {
                    // Use remove_from_queue first to avoid double-enqueue
                    // when the process was already in the run queue (e.g.,
                    // PREEMPTED set by proc_no_time while the process was
                    // still in the queue).
                    unsafe {
                        kernel::sched::remove_from_queue(caller);
                        kernel::sched::enqueue(caller);
                    };
                }
                // Fall through to pick a new process.
            }
            if rts != 0 {
                // Current process blocked or preempted — pick a new one.
                // FIRST: save current process's registers from the trap frame
                // to its p_reg.  The trap frame holds the register state at
                // the time of the ecall; without saving it, the current
                // process loses its register state (including syscall args
                // like a1=buffer pointer) when we overwrite the frame below.
                //
                // CRITICAL: p_reg layout differs from trap frame layout:
                //   frame[x]   = RISC-V GPRs x0..x31 (offsets 0..248)
                //                + sepc (256) + sstatus (264) + scause (272)
                //   p_reg[x]   = sepc (0), ra (8), ..., sstatus (248)
                //
                // We must save each field carefully:
                // 1. frame[0..256] (GPRs x0..x31) → p_reg[0..256]
                //    But frame[0..8] = x0 (0), which overwrites p_reg sepc!
                //    Fixed below.
                // 2. frame[248..256] = t6 (x31) overwrites p_reg[248..256] (sstatus!)
                //    Fixed below — t6 is kept in the dedicated p_t6 slot.
                // 3. frame[256..264] = sepc → p_reg[0..8]
                // 4. frame[264..272] = sstatus → p_reg[248..256]
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        frame.as_ptr(),
                        &raw mut (*caller).p_reg as *mut u8,
                        256,
                    );
                    // Save t6 (x31) — p_reg[248..256] holds sstatus, not t6.
                    (*caller).p_t6 = u64::from_ne_bytes(frame[248..256].try_into().unwrap());
                    // Save sepc from frame[256..264] into p_reg[0..8]
                    core::ptr::copy_nonoverlapping(
                        frame.as_ptr().add(256),
                        &raw mut (*caller).p_reg as *mut u8,
                        8,
                    );
                    // Save sstatus from frame[264..272] into p_reg[248..256]
                    core::ptr::copy_nonoverlapping(
                        frame.as_ptr().add(264),
                        (&raw mut (*caller).p_reg as *mut u8).add(248),
                        8,
                    );
                }

                if let Some(next_proc) = unsafe { kernel::sched::pick_proc() } {
                    unsafe {
                        // Deliver any pending IPC message to the target
                        // process's user buffer before switching to it.
                        let mf = (*next_proc)
                            .p_misc_flags
                            .load(core::sync::atomic::Ordering::Relaxed);
                        if mf & kernel::proc::MiscFlags::DELIVERMSG.bits() != 0 {
                            kernel::ipc::delivermsg(next_proc);
                            // Set a0 (return value) to source endpoint.
                            // Use hal::write_retval which knows the arch-
                            // specific offset (a0 at +80 on RISC-V, rax
                            // at +0 on x86_64).
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

                        // Copy new process's p_reg into frame[0..256]
                        core::ptr::copy_nonoverlapping(
                            &raw const (*next_proc).p_reg as *const u8,
                            frame.as_mut_ptr(),
                            256,
                        );
                        // Restore t6 (x31) from its dedicated slot.
                        frame[248..256].copy_from_slice(&(*next_proc).p_t6.to_ne_bytes());
                        // Copy sepc from p_reg[0..8] to frame[256..264]
                        let p_reg = &raw const (*next_proc).p_reg;
                        let sepc_bytes = core::ptr::read(p_reg as *const [u8; 8]);
                        frame[256..264].copy_from_slice(&sepc_bytes);
                        // Copy sstatus from p_reg[248..256] to frame[264..272]
                        let sst_bytes = core::ptr::read(p_reg.add(248) as *const [u8; 8]);
                        frame[264..272].copy_from_slice(&sst_bytes);
                        // Consume CONTEXT_SET: loading the exec'd p_reg into
                        // the frame fulfills it. If left set, the exec'd
                        // process's first syscall re-loads p_reg (sepc = entry)
                        // and restarts, re-running that syscall.
                        (*next_proc).p_misc_flags.fetch_and(
                            !kernel::proc::MiscFlags::CONTEXT_SET.bits(),
                            core::sync::atomic::Ordering::SeqCst,
                        );
                        // Load new process's page table
                        let new_cr3 = (*next_proc).p_seg.p_cr3;
                        if new_cr3 != 0 {
                            kernel::hal::write_cr3(new_cr3);
                        }
                        // Update current process pointer
                        arch_riscv64::cpulocals::set_current_proc(next_proc as u64);
                    }
                } else {
                    // No runnable processes — all blocked on IPC.
                    // First save current process's registers (same reason as above).
                    // Must also save sstatus from frame[264..272] to p_reg[248..256].
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            frame.as_ptr(),
                            &raw mut (*caller).p_reg as *mut u8,
                            256,
                        );
                        // Save t6 (x31) — p_reg[248..256] holds sstatus, not t6.
                        (*caller).p_t6 = u64::from_ne_bytes(frame[248..256].try_into().unwrap());
                        // Save sepc from frame[256..264] into p_reg[0..8] (x0 slot)
                        core::ptr::copy_nonoverlapping(
                            frame.as_ptr().add(256),
                            &raw mut (*caller).p_reg as *mut u8,
                            8,
                        );
                        // Save sstatus from frame[264..272] into p_reg[248..256]
                        core::ptr::copy_nonoverlapping(
                            frame.as_ptr().add(264),
                            (&raw mut (*caller).p_reg as *mut u8).add(248),
                            8,
                        );
                    }

                    // All processes are blocked — idle until an interrupt
                    // makes one runnable (matching x86's sti; hlt; cli and
                    // the AArch64 post-syscall loop). Do NOT notify PM here:
                    // that self-wakeup makes PM spin on GETKSIG notifications
                    // while real IPC messages (e.g. VFS_PM_FORK_REPLY) sit
                    // undelivered, livelocking the system.
                    loop {
                        arch_riscv64::hal::cpu_idle();
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
                                    kernel::hal::write_retval(
                                        &mut (*next_proc).p_reg,
                                        src_ep as u64,
                                    );
                                    (*next_proc).p_misc_flags.fetch_and(
                                        !kernel::proc::MiscFlags::DELIVERMSG.bits(),
                                        core::sync::atomic::Ordering::Relaxed,
                                    );
                                }
                                core::ptr::copy_nonoverlapping(
                                    &raw const (*next_proc).p_reg as *const u8,
                                    frame.as_mut_ptr(),
                                    256,
                                );
                                // Restore t6 (x31) from its dedicated slot.
                                frame[248..256].copy_from_slice(&(*next_proc).p_t6.to_ne_bytes());
                                let p_reg = &raw const (*next_proc).p_reg;
                                let sepc_bytes = core::ptr::read(p_reg as *const [u8; 8]);
                                frame[256..264].copy_from_slice(&sepc_bytes);
                                let sst_bytes = core::ptr::read(p_reg.add(248) as *const [u8; 8]);
                                frame[264..272].copy_from_slice(&sst_bytes);
                                (*next_proc).p_misc_flags.fetch_and(
                                    !kernel::proc::MiscFlags::CONTEXT_SET.bits(),
                                    core::sync::atomic::Ordering::SeqCst,
                                );
                                let new_cr3 = (*next_proc).p_seg.p_cr3;
                                if new_cr3 != 0 {
                                    kernel::hal::write_cr3(new_cr3);
                                }
                                arch_riscv64::cpulocals::set_current_proc(next_proc as u64);
                            }
                            break;
                        }
                    }
                }
            } else {
                // Caller stayed runnable — flush any message this syscall
                // staged in p_delivermsg (mini_receive's try_async path
                // sets DELIVERMSG and returns OK). x86's syscall_handler_c
                // delivers to a still-runnable caller; without this, the
                // message sits staged forever (observed: VFS never saw
                // VFS_PM_FORK and the shell's fork hung).
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
                        // Overwrite the syscall's OK return in a0 (x10 at
                        // frame offset 80) with the source endpoint.
                        let ret = src_ep as u64;
                        frame[80..88].copy_from_slice(&ret.to_ne_bytes());
                        (*caller).p_misc_flags.fetch_and(
                            !kernel::proc::MiscFlags::DELIVERMSG.bits(),
                            core::sync::atomic::Ordering::Relaxed,
                        );
                    }
                }
            }
        }
        arch_riscv64::trap::register_post_syscall_hook(riscv_post_syscall);
    }
    // Register UART input callback: pushes received bytes to ser_input.
    unsafe {
        unsafe fn uart_input_cb(byte: u8) {
            unsafe { kernel::ser_input::push_byte(byte) };
        }
        arch_riscv64::trap::register_uart_input_callback(uart_input_cb);
    }
    // Register timer callback for preemptive scheduling.
    unsafe {
        unsafe fn riscv_timer_callback(frame: &mut [u8; 296]) {
            // Full per-tick accounting (monotonic/realtime, virtual timers,
            // load average, quantum accounting via context_stop →
            // proc_no_time → notify_scheduler) in BOTH kernel and user
            // mode, matching x86's timer_int_handler. The SPP check below
            // then limits the save/pick/switch to user-mode interrupts.
            unsafe { kernel::clock::timer_int_handler() };
            // Preempt: if we interrupted user mode, save state and
            // potentially switch to another runnable process.
            let sstatus = u64::from_ne_bytes(frame[264..272].try_into().unwrap());
            if (sstatus >> 8) & 1 != 0 {
                return; // SPP=1: interrupted kernel mode, skip
            }
            let caller = arch_riscv64::hal::current_proc() as *mut kernel::proc::Proc;
            if caller.is_null() {
                return;
            }
            unsafe {
                core::ptr::copy_nonoverlapping(
                    frame.as_ptr(),
                    &raw mut (*caller).p_reg as *mut u8,
                    256,
                );
                // Save t6 (x31) — p_reg[248..256] holds sstatus, not t6.
                (*caller).p_t6 = u64::from_ne_bytes(frame[248..256].try_into().unwrap());
                core::ptr::copy_nonoverlapping(
                    frame.as_ptr().add(256),
                    &raw mut (*caller).p_reg as *mut u8,
                    8,
                );
                core::ptr::copy_nonoverlapping(
                    frame.as_ptr().add(264),
                    (&raw mut (*caller).p_reg as *mut u8).add(248),
                    8,
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
                            256,
                        );
                        // Restore t6 (x31) from its dedicated slot.
                        frame[248..256].copy_from_slice(&(*next_proc).p_t6.to_ne_bytes());
                        let p_reg = &raw const (*next_proc).p_reg;
                        let sepc_bytes = core::ptr::read(p_reg as *const [u8; 8]);
                        frame[256..264].copy_from_slice(&sepc_bytes);
                        let sst_bytes = core::ptr::read(p_reg.add(248) as *const [u8; 8]);
                        frame[264..272].copy_from_slice(&sst_bytes);
                        (*next_proc).p_misc_flags.fetch_and(
                            !kernel::proc::MiscFlags::CONTEXT_SET.bits(),
                            core::sync::atomic::Ordering::SeqCst,
                        );
                        let new_cr3 = (*next_proc).p_seg.p_cr3;
                        if new_cr3 != 0 {
                            kernel::hal::write_cr3(new_cr3);
                        }
                        arch_riscv64::cpulocals::set_current_proc(next_proc as u64);
                    }
                }
            }
        }
        arch_riscv64::trap::register_timer_callback(riscv_timer_callback);
    }
    // Register page fault handler for COW / demand paging forwarding to VM.
    unsafe {
        unsafe fn riscv_pf_handler(fault_addr: u64, error_code: u32) -> i32 {
            unsafe { kernel::vm::handle_page_fault(fault_addr, error_code) }
        }
        arch_riscv64::trap::register_page_fault_handler(riscv_pf_handler);
    }

    #[cfg(feature = "integration-tests")]
    {
        // Enable SV39 paging so copy_from_user / delivermsg perform real
        // page-table walks (BOOT_CR3 non-zero) instead of silently skipping
        // the copy. This unblocks the SENDREC payload assertions
        // (sendrec_direct, sendrec_reply_cycle) on RISC-V.
        serial_write("  enabling SV39 paging...\r\n");
        unsafe {
            if let Some(boot_pt) = create_boot_page_table() {
                arch_riscv64::BOOT_CR3.store(boot_pt, core::sync::atomic::Ordering::Relaxed);
                kernel::hal::write_cr3(boot_pt);
                serial_write("  SV39 enabled\r\n");
            } else {
                serial_write("  FAILED: boot page table\r\n");
                loop {
                    core::arch::asm!("wfi", options(nomem, nostack));
                }
            }
        }

        serial_write("Running RISC-V integration tests...\r\n");
        let mut failures = kernel::tests::run_all();
        failures += unsafe { test_clint_timer() };
        failures += unsafe { test_sbi_console() };
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
        // Shutdown QEMU via SBI
        arch_riscv64::sbi::system_reset(true);
    }

    #[cfg(not(feature = "integration-tests"))]
    {
        // Set CPU frequency so clock::ms_2_cpu_time converts ms to rdtime
        // cycles (QEMU virt CLINT timebase = 10 MHz). Without a non-zero
        // frequency, p_cpu_time_left stays 0 after SYS_SCHEDULE and the
        // first timer tick calls proc_no_time → notify_scheduler on every
        // tick.
        kernel::glo::cpu_set_freq(0, 10_000_000);

        // Initialize timer (100 Hz)
        unsafe {
            arch_riscv64::clint::init_timer(100);
        }

        // Enable S-mode interrupts (timer + external)
        unsafe {
            let mut sie_val: u64;
            core::arch::asm!("csrr {val}, sie", val = out(reg) sie_val, options(nomem, nostack));
            sie_val |= (1u64 << 5) | (1u64 << 9); // STIE | SEIE
            core::arch::asm!("csrw sie, {val}", val = in(reg) sie_val, options(nomem, nostack));
        }

        // Initialize PLIC
        unsafe {
            arch_riscv64::plic::init_plic();
        }

        serial_write("  enabling SV39 paging...\r\n");
        unsafe {
            if let Some(boot_pt) = create_boot_page_table() {
                // Save boot CR3 for delivermsg and other kernel code
                // that needs to switch to the identity-mapped page table.
                arch_riscv64::BOOT_CR3.store(boot_pt, core::sync::atomic::Ordering::Relaxed);
                kernel::hal::write_cr3(boot_pt);
                // Enable UART FIFO for piped input support
                arch_riscv64::uart::init_uart();
                // Enable UART RX interrupts: the 16550 must raise IRQ 10
                // on received data (IER bit 0) and the PLIC must forward
                // it, so a piped burst drains into the ser_input ring
                // promptly instead of overrunning the 16-byte FIFO between
                // timer ticks. Done after init_uart() (UART configured)
                // and write_cr3() (device MMIO mapped by the boot table).
                arch_riscv64::uart::enable_rx_interrupt();
                arch_riscv64::plic::enable_irq(arch_riscv64::plic::UART_IRQ);
                serial_write("  SV39 enabled\r\n");
            } else {
                serial_write("  FAILED: boot page table\r\n");
                loop {
                    core::arch::asm!("wfi", options(nomem, nostack));
                }
            }
        }

        use arch_common::com::*;

        serial_write("  loading boot processes...\r\n");

        // Define all boot processes: (path, proc_nr)
        // Order matches C MINIX kernel/table.c: ds first, then rs, pm, ...
        // When boot-test is active, INIT is excluded so the test completes
        // before any user process starts (same as x86 main.rs).
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
                    serial_write(
                        "  Check: initramfs contains binary? Allocator has free pages?\r\n",
                    );
                    // Dump allocator state
                    serial_write("  Allocator may be out of contiguous memory\r\n");
                    loop {
                        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                    }
                }
            };
            boot_infos[i] = core::mem::MaybeUninit::new(info);
        }

        serial_write("  creating per-process page tables...\r\n");

        // Create per-process (restricted) page tables and enqueue each process.
        let mut first_proc: *mut kernel::proc::Proc = core::ptr::null_mut();
        for (i, &(path, proc_nr)) in boot_procs.iter().enumerate() {
            let rp = kernel::table::proc_addr(proc_nr);
            if i == 0 {
                first_proc = rp;
            }

            let info = unsafe { boot_infos[i].assume_init_ref() };

            // Create a restricted page table that maps only this process's
            // code and stack, not the entire identity-mapped 1GB region.
            let pt_phys = unsafe {
                kernel_boot::boot_init::boot_create_restricted_page_table(
                    info.code_start,
                    info.code_end,
                    info.phys_code_base,
                    info.stack_start,
                    info.stack_end,
                    info.phys_stack_base,
                    false, // low-GB user device window: RISC-V maps it below
                )
            };
            let pt_phys = match pt_phys {
                Some(p) => p,
                None => {
                    serial_write("  FAILED: page table for ");
                    serial_write(path);
                    serial_write("\r\n");
                    loop {
                        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                    }
                }
            };

            unsafe {
                core::ptr::write_volatile(&raw mut (*rp).p_seg.p_cr3, pt_phys);
                // Set up privilege structure for this boot process.
                // proc_init already assigned a priv slot for every boot
                // image entry; get_priv is only a fallback for processes
                // without one. init keeps the shared USER slot.
                if (*rp).p_priv.is_null() {
                    let _ = kernel::system::get_priv(rp);
                }
                // Store physical delta for PA translation in verify_grant.
                // VFS's grant table is at VA grant_addr; s_phys_delta
                // converts VA to PA: PA = VA + s_phys_delta.
                if !(*rp).p_priv.is_null() {
                    (*(*rp).p_priv).s_phys_delta =
                        (info.phys_code_base as i64) - (info.code_start as i64);
                }
                // Set scheduling parameters.
                core::ptr::write_volatile(&raw mut (*rp).p_priority, 5i8);
                core::ptr::write_volatile(&raw mut (*rp).p_quantum_size_ms, 50u32);
                core::ptr::write_volatile(&raw mut (*rp).p_cpu_time_left, 50_000_000);
            }

            // Map the brk range (0x3FE00000..0x3FF00000 = 1 MB heap) with
            // allocated physical pages so the bump allocator has backing memory.
            // RISC-V requires V|R|W|U|X|A|D for user writable pages.
            // Without R (0x02), W=1 without R=1 is a reserved encoding.
            let user_flags = kernel::pagetable::PG_P
                | kernel::pagetable::PG_RW
                | kernel::pagetable::PG_U
                | 0x02
                | 0x04
                | 0x08
                | 0xC0; // R|W|X|A|D
            let brk_va_start = 0x3FE00000u64;
            let brk_va_end = 0x3FF00000u64;
            let brk_pages = ((brk_va_end - brk_va_start) / 4096) as usize;
            let brk_phys = match unsafe { kernel::hal::alloc_phys_contig(brk_pages) } {
                Some(base) => base,
                None => {
                    serial_write("  FAILED: out of memory for brk heap\r\n");
                    loop {
                        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                    }
                }
            };
            for j in 0..brk_pages {
                let va = brk_va_start + (j as u64) * 4096;
                let pa = brk_phys + (j as u64) * 4096;
                if unsafe { kernel::pagetable::map_page(pt_phys, va, pa, user_flags) }.is_err() {
                    serial_write("  FAILED: brk page mapping\r\n");
                    loop {
                        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                    }
                }
            }

            // If this is the ramdisk driver process, set up the boot image
            // mapping (served to filesystem servers via the BDEV protocol).
            if proc_nr == RAMDISK_PROC_NR {
                let image = kernel::minixfs::minixfs_image();
                let image_len = kernel::minixfs::minixfs_image_len();
                if image_len > 0 {
                    let pages = image_len.div_ceil(4096);
                    let ramdisk_phys = match unsafe { kernel::hal::alloc_phys_contig(pages) } {
                        Some(base) => base,
                        None => {
                            serial_write("  FAILED: out of memory for RAM disk\r\n");
                            loop {
                                unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                            }
                        }
                    };
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            image.as_ptr(),
                            ramdisk_phys as *mut u8,
                            image_len,
                        );
                    }
                    // Map the RAM disk pages in the ramdisk server's page table.
                    let user_flags = kernel::pagetable::PG_P
                        | kernel::pagetable::PG_RW
                        | kernel::pagetable::PG_U
                        | 0x02
                        | 0x04
                        | 0x08
                        | 0xC0; // R|W|X|A|D
                    for j in 0..pages {
                        let va = arch_common::com::RAMDISK_IMAGE_VA + (j as u64) * 4096;
                        let pa = ramdisk_phys + (j as u64) * 4096;
                        if unsafe { kernel::pagetable::map_page(pt_phys, va, pa, user_flags) }
                            .is_err()
                        {
                            serial_write("  FAILED: RAM disk page mapping\r\n");
                            loop {
                                unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                            }
                        }
                    }
                    serial_write("  RAM disk mapped for ramdisk server\r\n");
                }
            }

            // virtio drivers (blk/net): map the virtio-mmio device window
            // into this process's page table so it can probe the device
            // from user mode. RISC-V QEMU virt places the eight transports
            // at 0x10001000, 0x1000 apart (the device can be at any of
            // them), so map the whole 32KB region.
            if proc_nr == VIRTIO_BLK_PROC_NR || proc_nr == VIRTIO_NET_PROC_NR {
                const VIRTIO_MMIO_BASE: u64 = 0x1000_1000;
                for j in 0..8u64 {
                    let va = VIRTIO_MMIO_BASE + j * 0x1000;
                    if unsafe { kernel::pagetable::map_page(pt_phys, va, va, user_flags) }.is_err()
                    {
                        serial_write("  FAILED: virtio MMIO page mapping\r\n");
                        loop {
                            unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                        }
                    }
                }
            }
        }

        if first_proc.is_null() {
            serial_write("  FAILED: no boot processes found\r\n");
            loop {
                unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
            }
        }

        // Set a boot notification on PM directly (without mini_notify, which
        // would double-enqueue PM since it is runnable and already in the
        // queue). PM will discover the pending notification when it calls
        // RECEIVE.
        unsafe {
            let pm = kernel::table::proc_addr(arch_common::com::PM_PROC_NR);
            if !pm.is_null() && !(*pm).p_priv.is_null() {
                let rs_priv_id =
                    kernel::r#priv::priv_find_proc_id(arch_common::com::RS_PROC_NR).unwrap_or(0);
                (*(*pm).p_priv).s_notify_pending.set(rs_priv_id);
            }
        }

        serial_write("  enqueuing processes...\r\n");

        // Enqueue each process that is runnable.
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

        // Set the current process pointer to the first one.
        unsafe {
            arch_riscv64::cpulocals::set_current_proc(first_proc as u64);
        }

        serial_write("  scheduler starting...\r\n");

        // Pick the first process and switch to userspace.
        let next_proc = unsafe { kernel::sched::pick_proc() };
        let next_ptr = match next_proc {
            Some(p) => p,
            None => {
                serial_write("  FAILED: no runnable processes\r\n");
                loop {
                    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                }
            }
        };

        serial_write("  switching to userspace...\r\n");
        unsafe {
            arch_riscv64::switch::switch_to_user(next_ptr as *const u8);
        }
    }
}

/// RISC-V hardware tests: CLINT timer + SBI console.
///
/// Runs inside the integration build after the shared kernel suite. These
/// probe the actual device/console paths (via the SV39 identity map) rather
/// than the hal wrappers the shared tests use.

/// CLINT timer: rdtime (the `time` CSR — the architectural view of the CLINT
/// mtime) must advance, and the SSTC stimecmp CSR (0x14D) must accept and
/// return a programmed deadline. These are the S-mode timer interfaces the
/// port actually uses (clint::read_time / init_timer) — the hardware-level
/// equivalent of monotonic_advances / timer_set_and_expire.
#[cfg(feature = "integration-tests")]
unsafe fn test_clint_timer() -> u32 {
    unsafe {
        // rdtime (the `time` CSR) is the architectural view of the CLINT
        // mtime — it must advance. (The CLINT MMIO mtime at 0x0200BFF8 is
        // not accessible from S-mode on this QEMU build: it raises a load
        // access fault, cause 5, hence the port uses rdtime + SSTC.)
        let t1 = arch_riscv64::clint::read_time();
        let mut t2 = t1;
        let mut spins = 0usize;
        while t2 == t1 && spins < 1_000_000 {
            core::hint::spin_loop();
            t2 = arch_riscv64::clint::read_time();
            spins += 1;
        }
        if t2 <= t1 {
            serial_write("  FAIL: rdtime should advance\r\n");
            return 1;
        }

        // SSTC stimecmp (CSR 0x14D): write a next-tick deadline and read it
        // back — the S-mode timer-programming path clint::init_timer uses.
        let now = arch_riscv64::clint::read_time();
        let deadline = now + arch_riscv64::clint::DEFAULT_TIMER_INTERVAL;
        core::arch::asm!("csrw 0x14D, {v}", v = in(reg) deadline, options(nomem, nostack));
        let read_back: u64;
        core::arch::asm!("csrr {v}, 0x14D", v = out(reg) read_back, options(nomem, nostack));
        if read_back != deadline {
            serial_write("  FAIL: stimecmp readback mismatch\r\n");
            return 1;
        }
        serial_write("  OK CLINT timer (rdtime + SSTC stimecmp)\r\n");
    }
    0
}

/// SBI console: legacy putchar (used by every serial_write on RISC-V) and
/// getchar (must return cleanly with no input). The DBCN buffer-write
/// extension is NOT exercised: on this QEMU/OpenSBI build, console_write
/// faults inside OpenSBI reading address == num_bytes, suggesting its
/// handler consumes (len, addr) in the swapped order — the port correctly
/// avoids DBCN and uses legacy putchar everywhere.
#[cfg(feature = "integration-tests")]
unsafe fn test_sbi_console() -> u32 {
    arch_riscv64::sbi::console_putchar(b'~');
    // Must return cleanly; with no console input this is None.
    let _ = arch_riscv64::sbi::console_getchar();
    serial_write("  OK SBI console (legacy putchar/getchar)\r\n");
    0
}

/// Create an identity-mapped boot page table for SV39 paging.
///
/// Maps the full 4GB physical address space with 1GB huge pages.
/// This covers kernel code at 0x80200000, device memory (UART at 0x10000000,
/// PLIC at 0x0C000000, CLINT at 0x02000000), and all RAM.
///
/// Returns the physical address of the root page table.
///
/// # Safety
///
/// Must be called after the physical allocator is initialized, before any
/// virtual memory is active. The kernel must be running in Bare mode.
#[cfg(target_arch = "riscv64")]
unsafe fn create_boot_page_table() -> Option<u64> {
    unsafe {
        // Try to allocate from the physical allocator first.
        // If it fails (e.g., allocator not yet initialized), use a
        // hardcoded page in the gap between kernel data end and
        // the allocator-managed region.
        let root_phys = match arch_riscv64::alloc::alloc_phys_page() {
            Some(pa) => pa,
            None => {
                // Fallback: page at 0x8FF00000 is in RAM but outside
                // PMP protected ranges (0x80000000-0x8004FFFF).
                0x8FF00000u64
            }
        };
        core::ptr::write_bytes(root_phys as *mut u8, 0, 4096);

        // SV39 PTE flags for supervisor identity-mapped 1GB pages:
        // - V=1 (valid), R=1 (read), W=1 (write), X=1 (execute)
        // - No U bit (supervisor-only), no G bit
        let flags = arch_riscv64::pte::PTE_V
            | arch_riscv64::pte::PTE_R
            | arch_riscv64::pte::PTE_W
            | arch_riscv64::pte::PTE_X;

        // Map 1 GiB huge pages at L2, identity 0..32 GiB:
        // - L2[i] covers VA [i<<30, (i+1)<<30) → PA i<<30.
        // Per-process tables copy these supervisor-only entries, so the
        // kernel keeps identity access to all of RAM under any page table.
        let root = root_phys as *mut u64;
        for i in 0..32u64 {
            // build_pte encodes PPN = pa >> 12 correctly for SV39
            let pte = arch_riscv64::hal::build_pte(i << 30, flags);
            core::ptr::write(root.add(i as usize), pte);
        }

        Some(root_phys)
    }
}

/// Panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::panic::handle(info)
}
