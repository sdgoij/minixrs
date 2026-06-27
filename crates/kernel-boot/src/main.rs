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

/// Kernel main entry point — called from boot assembly.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    // Initialize subsystems
    kernel::init();

    // Initialize basic userspace syscall handlers
    unsafe {
        kernel::syscall::init_basic_syscalls();
    }

    // Print banner via serial
    init_serial();
    serial_write(b"Hello MINIX!\r\n");

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
