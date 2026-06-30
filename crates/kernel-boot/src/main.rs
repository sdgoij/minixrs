//! Boot binary crate.
//! Breaks circular dependency between kernel and arch-x86_64.
//!
//! Build with: `cargo build -p kernel-boot --target x86_64-unknown-none`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

#[cfg(not(test))]
use core::arch::asm;

#[cfg(not(test))]
use core::panic::PanicInfo;

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
    unsafe {
        fn dma_alloc(pages: usize) -> Option<(*mut u8, u64)> {
            let alloc = arch_x86_64::alloc::global_allocator();
            if alloc.is_null() {
                return None;
            }
            let phys = unsafe { (*alloc).alloc_contig(pages) }?;
            // With identity mapping, virtual address equals physical address.
            Some((phys as *mut u8, phys))
        }
        fn dma_free(virt: *mut u8, pages: usize) {
            let alloc = arch_x86_64::alloc::global_allocator();
            if alloc.is_null() {
                return;
            }
            unsafe { (*alloc).free_contig(virt as u64, pages) };
        }
        drivers::storage::dma::register_allocator(dma_alloc, dma_free);
    }

    // Print banner via serial
    init_serial();
    serial_write(b"Hello MINIX!\r\n");

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

    // Halt loop
    loop {
        unsafe {
            asm!("hlt", options(nomem, nostack));
        }
    }
}

/// Initialize COM1 serial port (115200 baud, 8N1).
#[cfg(not(test))]
fn init_serial() {
    unsafe {
        let port = 0x3F8u16;
        // Disable interrupts
        asm!("out dx, al", in("dx") port + 1, in("al") 0x00u8, options(nomem, nostack));
        // Set DLAB=1 (baud rate divisor)
        asm!("out dx, al", in("dx") port + 3, in("al") 0x80u8, options(nomem, nostack));
        // Divisor low byte: 115200 / 115200 = 1
        asm!("out dx, al", in("dx") port, in("al") 0x01u8, options(nomem, nostack));
        // Divisor high byte
        asm!("out dx, al", in("dx") port + 1, in("al") 0x00u8, options(nomem, nostack));
        // 8N1: 8 bits, no parity, 1 stop bit
        asm!("out dx, al", in("dx") port + 3, in("al") 0x03u8, options(nomem, nostack));
        // Enable FIFO, clear, 14-byte threshold
        asm!("out dx, al", in("dx") port + 2, in("al") 0xC7u8, options(nomem, nostack));
        // IRQs enabled, RTS/DSR set
        asm!("out dx, al", in("dx") port + 4, in("al") 0x0Bu8, options(nomem, nostack));
    }
}

/// Write bytes to COM1 serial port.
#[cfg(not(test))]
fn serial_write(bytes: &[u8]) {
    let port = 0x3F8u16;
    for &b in bytes {
        unsafe {
            // Wait for transmitter holding register empty
            loop {
                let lsr: u8;
                asm!("in al, dx", out("al") lsr, in("dx") port + 5, options(nomem, nostack));
                if lsr & 0x20 != 0 {
                    break;
                }
            }
            // Transmit byte
            asm!("out dx, al", in("dx") port, in("al") b, options(nomem, nostack));
        }
    }
}

/// Panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe {
            asm!("hlt", options(nomem, nostack));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        assert!(true);
    }
}
