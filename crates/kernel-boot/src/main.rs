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
        serial_write("  loading init from initramfs...\r\n");

        unsafe {
            kernel::table::proc_init();
        }

        let init_info = match unsafe { boot_init::load_and_prepare_init() } {
            Some(info) => info,
            None => {
                serial_write("  FAILED: no valid init binary\r\n");
                hlt_loop();
            }
        };

        serial_write("  creating per-process page table...\r\n");

        let pt_phys = unsafe { boot_init::boot_create_page_table() };
        if pt_phys == 0 {
            serial_write("  FAILED: page table allocation\r\n");
            hlt_loop();
        }

        serial_write("  jumping to ring-3...\r\n");

        unsafe {
            arch_x86_64::apic::mask_timer_irq();
        }

        #[cfg(target_os = "none")]
        unsafe {
            arch_x86_64::asm::syscall_abi::set_syscall_handler(syscall_handler_c);
            let entry = arch_x86_64::asm::syscall_abi::syscall_entry as *const () as u64;
            arch_x86_64::arch_syscall::setup_syscall_msrs(entry);
            arch_x86_64::cpulocals::init_cpulocals();
            arch_x86_64::cpulocals::set_cpulocal_proc_ptr(
                init_info.proc_ptr as *mut core::ffi::c_void,
            );
        }

        unsafe {
            boot_init::boot_jump_to_user(&init_info, pt_phys);
        }
    }
}

/// C handler for syscall entry — called from arch-x86_64's naked asm.
/// Saves/restores registers, dispatches to kernel::syscall.
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
