//! Boot binary crate.
//! Breaks circular dependency between kernel and arch-x86_64.
//!
//! Build with: `cargo build -p kernel-boot --target x86_64-unknown-none`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

#[cfg(not(test))]
use core::panic::PanicInfo;

pub mod boot_init;

#[cfg(feature = "integration-tests")]
pub mod test_runner;

/// Dummy entry point to prevent --gc-sections from discarding all code.
/// The actual entry is through the multiboot trampoline which jumps
/// directly to kmain.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    kmain()
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
        let mut mmap = arch_x86_64::alloc::PhysicalMemoryMap::new();
        mmap.add(0x0030_0000, 0x1000_0000);
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

    // ── Integration tests or normal boot ────────────────────────────────
    #[cfg(feature = "integration-tests")]
    {
        serial_write("Running integration tests...\r\n");
        // This never returns — calls qemu_exit_success/failure
        test_runner::run_integration_tests();
    }

    #[cfg(not(feature = "integration-tests"))]
    {
        use arch_common::com::*;

        serial_write("  initializing boot processes...\r\n");

        unsafe {
            kernel::table::proc_init();
        }

        // Define all boot processes: (path, proc_nr, endpoint_name)
        let boot_procs: &[(&str, i32)] = &[
            ("/sbin/pm", PM_PROC_NR),
            ("/sbin/vfs", VFS_PROC_NR),
            ("/sbin/rs", RS_PROC_NR),
            ("/sbin/vm", VM_PROC_NR),
            ("/sbin/ds", DS_PROC_NR),
            ("/sbin/sched", SCHED_PROC_NR),
            ("/sbin/tty", TTY_PROC_NR),
            ("/sbin/init", INIT_PROC_NR),
        ];

        // Load each boot process from initramfs, storing InitInfo for
        // per-process page table creation.
        serial_write("  loading boot processes...\r\n");
        let mut boot_infos: [core::mem::MaybeUninit<boot_init::InitInfo>; 8] =
            unsafe { core::mem::zeroed() };
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
            boot_infos[i] = core::mem::MaybeUninit::new(info);
        }

        serial_write("  creating per-process page tables...\r\n");

        #[cfg(target_os = "none")]
        unsafe {
            arch_x86_64::asm::syscall_abi::set_syscall_handler(syscall_handler_c);
            let entry = arch_x86_64::asm::syscall_abi::syscall_entry as *const () as u64;
            arch_x86_64::arch_syscall::setup_syscall_msrs(entry);
            arch_x86_64::cpulocals::init_cpulocals();

            // Initialize the VM page allocator (used by map_page for
            // page table page allocations). Free memory from 0x300000
            // (end of kernel image) to 0xFE00000 (start of user stack),
            // and from 0xFF00000 (end of user stack) to 0x10000000.
            let m1 = kernel::vm::MemoryChunk {
                base: 0x300000 / kernel::vm::VM_PAGE_SIZE as u64,
                size: (0xFE00000 - 0x300000) / kernel::vm::VM_PAGE_SIZE as u64,
            };
            let m2 = kernel::vm::MemoryChunk {
                base: 0xFF00000 / kernel::vm::VM_PAGE_SIZE as u64,
                size: (0x10000000 - 0xFF00000) / kernel::vm::VM_PAGE_SIZE as u64,
            };
            kernel::vm::mem_init(&[m1, m2]);
        }

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
                boot_init::boot_create_restricted_page_table(
                    info.code_start,
                    info.code_end,
                    info.stack_start,
                    info.stack_end,
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
                // Set scheduling parameters.
                core::ptr::write_volatile(&raw mut (*rp).p_priority, 5i8);
                core::ptr::write_volatile(&raw mut (*rp).p_quantum_size_ms, 50u32);
                core::ptr::write_volatile(&raw mut (*rp).p_cpu_time_left, 50_000_000);
            }
        }

        if first_proc.is_null() {
            serial_write("  FAILED: no boot processes found\r\n");
            hlt_loop();
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
            arch_x86_64::cpulocals::set_cpulocal_proc_ptr(first_proc as *mut core::ffi::c_void);
        }

        serial_write("  scheduler starting...\r\n");

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
    unsafe {
        (*rp).p_reg.rax = *saved.add(0);
        (*rp).p_reg.rbx = *saved.add(1);
        (*rp).p_reg.rcx = *saved.add(2); // return RIP
        (*rp).p_reg.rdx = *saved.add(3);
        (*rp).p_reg.rsi = *saved.add(4);
        (*rp).p_reg.rdi = *saved.add(5);
        (*rp).p_reg.r8 = *saved.add(6);
        (*rp).p_reg.r9 = *saved.add(7);
        (*rp).p_reg.r10 = *saved.add(8);
        (*rp).p_reg.r11 = *saved.add(9); // return RFLAGS
        (*rp).p_reg.r12 = *saved.add(10);
        (*rp).p_reg.r13 = *saved.add(11);
        (*rp).p_reg.r14 = *saved.add(12);
        (*rp).p_reg.r15 = *saved.add(13);
        // Recover user RSP from stack position
        (*rp).p_reg.rsp = (saved as u64) + 112;
        // RIP and RFLAGS are stored in rcx/r11 positions
        (*rp).p_reg.rip = *saved.add(2);
        (*rp).p_reg.rflags = *saved.add(9);
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

        // ── Round-robin context switch ──────────────────────────────
        // Save the current process's register state.
        save_proc_regs(rp, saved);

        // Re-enqueue if still runnable.
        if (*rp).is_runnable() {
            kernel::sched::enqueue(rp);
        }

        // Pick the next runnable process.
        if let Some(next) = kernel::sched::pick_proc()
            && next != rp
        {
            arch_x86_64::cpulocals::set_cpulocal_proc_ptr(next as *mut core::ffi::c_void);
            // Switch to the new process — never returns.
            arch_x86_64::asm::restore(next as *const u8);
        }
        // Otherwise: return normally and syscall_entry will pop the
        // (unchanged) register save area and sysretq to the same process.
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Serial I/O — available in all build modes (no-op in test mode)
// ═════════════════════════════════════════════════════════════════════════

/// Halt the CPU forever (fallback if boot fails).
#[cfg(not(test))]
fn hlt_loop() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
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

/// Write a string to COM1 serial port.  No-op in test mode.
pub fn serial_write(s: &str) {
    #[cfg(not(test))]
    {
        let port = 0x3F8u16;
        for &b in s.as_bytes() {
            unsafe {
                loop {
                    let lsr: u8;
                    core::arch::asm!("in al, dx", out("al") lsr, in("dx") port + 5, options(nomem, nostack));
                    if lsr & 0x20 != 0 {
                        break;
                    }
                }
                core::arch::asm!("out dx, al", in("dx") port, in("al") b, options(nomem, nostack));
            }
        }
    }
    #[cfg(test)]
    let _ = s;
}

/// Write a single byte to COM1 serial port.  No-op in test mode.
pub fn serial_putc(c: u8) {
    #[cfg(not(test))]
    {
        let port = 0x3F8u16;
        unsafe {
            loop {
                let lsr: u8;
                core::arch::asm!("in al, dx", out("al") lsr, in("dx") port + 5, options(nomem, nostack));
                if lsr & 0x20 != 0 {
                    break;
                }
            }
            core::arch::asm!("out dx, al", in("dx") port, in("al") c, options(nomem, nostack));
        }
    }
    #[cfg(test)]
    let _ = c;
}

/// Print macro for boot-time serial output.
#[macro_export]
macro_rules! print {
    ($s:expr) => {
        $crate::serial_write($s);
    };
}

/// Panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    hlt_loop()
}

#[cfg(test)]
mod tests {
    #[test]
    fn serial_write_does_not_panic_in_tests() {
        // Verify the no-op path compiles and runs
        crate::serial_write("test");
        crate::serial_putc(b'x');
    }
}
