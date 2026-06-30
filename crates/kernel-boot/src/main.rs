//! Boot binary crate.
//! Breaks circular dependency between kernel and arch-x86_64.
//!
//! Build with: `cargo build -p kernel-boot --target x86_64-unknown-none`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

#[cfg(not(test))]
use core::panic::PanicInfo;

pub mod boot_init;

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
    // Initialize subsystems
    kernel::init();

    // Initialize basic userspace syscall handlers
    unsafe {
        kernel::syscall::init_basic_syscalls();
    }

    // Register the physical memory allocator with the DMA buffer API.
    fn dma_alloc(pages: usize) -> Option<(*mut u8, u64)> {
        let alloc = arch_x86_64::alloc::global_allocator();
        if alloc.is_null() {
            return None;
        }
        // SAFETY: allocator was initialized, single-threaded boot
        let phys = unsafe { (*alloc).alloc_contig(pages) }?;
        Some((phys as *mut u8, phys))
    }
    fn dma_free(virt: *mut u8, pages: usize) {
        let alloc = arch_x86_64::alloc::global_allocator();
        if alloc.is_null() {
            return;
        }
        // SAFETY: allocator was initialized, single-threaded boot
        unsafe { (*alloc).free_contig(virt as u64, pages) };
    }
    // SAFETY: called once during boot, single-threaded
    unsafe {
        drivers::storage::dma::register_allocator(dma_alloc, dma_free);
    }

    // Initialize the physical memory allocator.
    // The kernel binary + BSS is loaded at 0x200000 and fits within the
    // first 3 MB.  RAM is 256 MB (0x0 - 0x0FFFFFFF) per QEMU's -m 256M.
    // We give the allocator all RAM from 0x300000 to 0x0FFFFFFF.
    let mut mmap = arch_x86_64::alloc::PhysicalMemoryMap::new();
    // Free memory from 3 MB to 256 MB (minus 1 page for the user stack top)
    mmap.add(0x0030_0000, 0x1000_0000);
    // Reserve the user stack region (0x0FE00000 - 0x0FF00000) so the
    // allocator doesn't hand it out.
    mmap.cut(0x0FE0_0000, 0x0FF0_0000);
    arch_x86_64::alloc::init_allocator(&mmap);

    // Print banner via serial
    init_serial();
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

    // ── Boot the first userspace process ─────────────────────────────────
    serial_write("  loading init from initramfs...\r\n");

    let init_info = unsafe { boot_init::load_and_prepare_init() };

    serial_write("  creating per-process page table...\r\n");

    let pt_phys = unsafe { boot_init::boot_create_page_table() };
    if pt_phys == 0 {
        serial_write("  FAILED: page table allocation\r\n");
        hlt_loop();
    }

    serial_write("  jumping to ring-3...\r\n");

    // This never returns — jumps to userspace via sysretq
    unsafe {
        boot_init::boot_jump_to_user(&init_info, pt_phys);
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
