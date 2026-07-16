//! RISC-V64 kernel boot binary entry point.
//!
//! Build with: `cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-none-elf`

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
    # Set up a stack near the top of RAM (256MB QEMU virt).
    li      sp, 0x8FC00000

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
        // Register only the kernel call handlers needed for boot.
        // Full system_init() causes a hang on RISC-V (investigation needed).
        // Register SYS_SETGRANT (34) so VFS can register its grant table.
        kernel::system::map_call(34, kernel::system::do_setgrant_handler);
        // Register SYS_VM_PAGING (62) for VM's physical page management.
        kernel::system::map_call(62, kernel::system::do_vm_paging_handler);
        // IPC syscalls are already registered by init_basic_syscalls below.
    }

    // Initialize basic userspace syscall handlers
    unsafe {
        kernel::syscall::init_basic_syscalls();
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
            if rts != 0 {
                // Current process blocked or preempted — pick a new one.
                // FIRST: save current process's registers from the trap frame
                // to its p_reg.  The trap frame holds the register state at
                // the time of the ecall; without saving it, the current
                // process loses its register state (including syscall args
                // like a1=buffer pointer) when we overwrite the frame below.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        frame.as_ptr(),
                        &raw mut (*caller).p_reg as *mut u8,
                        256,
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
                        // Copy sepc from p_reg[0..8] to frame[256..264]
                        let p_reg = &raw const (*next_proc).p_reg;
                        let sepc_bytes = core::ptr::read(p_reg as *const [u8; 8]);
                        frame[256..264].copy_from_slice(&sepc_bytes);
                        // Copy sstatus from p_reg[248..256] to frame[264..272]
                        let sst_bytes = core::ptr::read(p_reg.add(248) as *const [u8; 8]);
                        frame[264..272].copy_from_slice(&sst_bytes);
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
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            frame.as_ptr(),
                            &raw mut (*caller).p_reg as *mut u8,
                            256,
                        );
                    }

                    // Send a notification to PM to kickstart scheduling.
                    let pm_proc = kernel::table::proc_addr(arch_common::com::PM_PROC_NR);
                    if !pm_proc.is_null() {
                        unsafe {
                            let _ = kernel::ipc::mini_notify(
                                arch_common::com::RS_PROC_NR,
                                arch_common::com::PM_PROC_NR,
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
                                let p_reg = &raw const (*next_proc).p_reg;
                                let sepc_bytes = core::ptr::read(p_reg as *const [u8; 8]);
                                frame[256..264].copy_from_slice(&sepc_bytes);
                                let sst_bytes = core::ptr::read(p_reg.add(248) as *const [u8; 8]);
                                frame[264..272].copy_from_slice(&sst_bytes);
                                let new_cr3 = (*next_proc).p_seg.p_cr3;
                                if new_cr3 != 0 {
                                    kernel::hal::write_cr3(new_cr3);
                                }
                                arch_riscv64::cpulocals::set_current_proc(next_proc as u64);
                            }
                        }
                    }
                }
            }
        }
        arch_riscv64::trap::register_post_syscall_hook(riscv_post_syscall);
    }
    unsafe {
        // Register UART input callback: pushes received bytes to ser_input.
        unsafe fn uart_input_cb(byte: u8) {
            unsafe { kernel::ser_input::push_byte(byte) };
        }
        arch_riscv64::trap::register_uart_input_callback(uart_input_cb);
    }

    #[cfg(feature = "integration-tests")]
    {
        serial_write("Running RISC-V integration tests...\r\n");
        let failures = kernel::tests::run_all();
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
                kernel::hal::write_cr3(boot_pt);
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
        let boot_procs: &[(&str, i32)] = &[
            ("/sbin/pm", PM_PROC_NR),           // Process Manager
            ("/sbin/rs", RS_PROC_NR),           // Reincarnation Server
            ("/sbin/vfs", VFS_PROC_NR),         // Virtual File System
            ("/sbin/ramdisk", RAMDISK_PROC_NR), // RAM disk block driver
            ("/sbin/mfs", MFS_PROC_NR),         // Memory File System
            ("/sbin/init", INIT_PROC_NR),       // init
        ];

        // Load each boot process from initramfs, storing InitInfo for
        // per-process page table creation.
        // Note: boot_init's internal error messages use `print!` which is
        // a no-op on RISC-V (x86_64 COM1 only), so we add our own diagnostics.
        let mut boot_infos: [core::mem::MaybeUninit<kernel_boot::boot_init::InitInfo>; 8] =
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
                | 0x08
                | 0xC0; // R|X|A|D
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
                    // Map the RAM disk pages in MFS's page table.
                    let user_flags = kernel::pagetable::PG_P
                        | kernel::pagetable::PG_RW
                        | kernel::pagetable::PG_U
                        | 0x02
                        | 0x08
                        | 0xC0; // R|X|A|D
                    for j in 0..pages {
                        let va = arch_common::com::MFS_RAMDISK_VA + (j as u64) * 4096;
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
                    serial_write("  RAM disk mapped for MFS\r\n");
                }
            }
        }

        if first_proc.is_null() {
            serial_write("  FAILED: no boot processes found\r\n");
            loop {
                unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
            }
        }

        // Send a boot notification to PM before starting the scheduler.
        // This notification will be pending when PM calls RECEIVE, causing
        // mini_receive to build a notification message and deliver it.
        unsafe {
            kernel::ipc::mini_notify(arch_common::com::RS_PROC_NR, arch_common::com::PM_PROC_NR);
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
                        | kernel::proc::RtsFlags::SLOT_FREE.bits());
                if cleared == 0 {
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
#[cfg(all(target_arch = "riscv64", not(feature = "integration-tests")))]
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

        // Map using 1GB huge pages at L2 level:
        // - L2[0]: VA 0x00000000-0x3FFFFFFF → PA 0x00000000 (covers devices, CLINT)
        // - L2[1]: VA 0x40000000-0x7FFFFFFF → PA 0x40000000
        // - L2[2]: VA 0x80000000-0xBFFFFFFF → PA 0x80000000 (covers RAM, kernel)
        // - L2[3]: VA 0xC0000000-0xFFFFFFFF → PA 0xC0000000
        let root = root_phys as *mut u64;
        for (i, base) in [0x00000000u64, 0x40000000, 0x80000000, 0xC0000000]
            .iter()
            .enumerate()
        {
            // build_pte encodes PPN = pa >> 12 correctly for SV39
            let pte = arch_riscv64::hal::build_pte(*base, flags);
            core::ptr::write(root.add(i), pte);
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
