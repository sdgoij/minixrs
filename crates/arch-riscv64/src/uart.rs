//! RISC-V64 NS16550a UART driver (MMIO).
//!
//! On QEMU riscv64 virt, the UART is at 0x10000000 and uses the same
//! NS16550a register layout as x86_64 COM ports, but accessed via
//! MMIO (load/store) instead of port I/O (in/out).
//!
//! The SBI console is preferred for simple output, but this driver
//! provides interrupt-driven I/O via the PLIC (IRQ 10).

#![cfg(target_arch = "riscv64")]

use core::ptr::{read_volatile, write_volatile};

/// UART base address on QEMU virt.
pub const UART_BASE: u64 = 0x10000000;

// NS16550a register offsets (MMIO, 1 byte each, spaced 1 byte apart).
const RBR: u64 = 0; // Receive Buffer (read)
const THR: u64 = 0; // Transmit Holding (write)
const IER: u64 = 1; // Interrupt Enable
const IIR: u64 = 2; // Interrupt ID (read)
const FCR: u64 = 2; // FIFO Control (write)
const LCR: u64 = 3; // Line Control
const MCR: u64 = 4; // Modem Control
const LSR: u64 = 5; // Line Status
const MSR: u64 = 6; // Modem Status
const SCR: u64 = 7; // Scratch

// Line status register bits.
const LSR_DR: u8 = 0x01; // Data Ready
const LSR_THRE: u8 = 0x20; // Transmit Holding Register Empty

/// Read a byte from a UART register.
unsafe fn uart_read(reg: u64) -> u8 {
    unsafe { read_volatile((UART_BASE + reg) as *const u8) }
}

/// Write a byte to a UART register.
unsafe fn uart_write(reg: u64, val: u8) {
    unsafe { write_volatile((UART_BASE + reg) as *mut u8, val) }
}

/// Initialize the UART (115200 baud, 8N1).
///
/// # Safety
///
/// Must be called once during boot.
pub unsafe fn init_uart() {
    // Set DLAB=1 (Divisor Latch Access Bit) in LCR
    uart_write(LCR, 0x80);
    // Set divisor for 115200 baud (1.8432 MHz / (16 * 115200) = 1)
    uart_write(RBR, 1); // DLL = 1
    uart_write(IER, 0); // DLM = 0
    // Set LCR: 8 data bits, 1 stop bit, no parity (DLAB=0)
    uart_write(LCR, 0x03);
    // Enable FIFOs, clear them, set trigger level to 1
    uart_write(FCR, 0x07);
}

/// Write a single byte to the UART (blocking).
pub fn putchar(c: u8) {
    unsafe {
        // Wait for THR empty
        loop {
            let lsr = uart_read(LSR);
            if lsr & LSR_THRE != 0 {
                break;
            }
        }
        uart_write(THR, c);
    }
}

/// Non-blocking: check if a byte is available.
pub fn byte_available() -> bool {
    unsafe { (uart_read(LSR) & LSR_DR) != 0 }
}

/// Read a byte from the UART (blocking).
pub fn getchar() -> u8 {
    loop {
        if let Some(b) = try_getchar() {
            return b;
        }
    }
}

/// Non-blocking: try to read a byte.
pub fn try_getchar() -> Option<u8> {
    if byte_available() {
        unsafe { Some(uart_read(RBR)) }
    } else {
        None
    }
}

/// Write a string to the UART.
pub fn puts(s: &str) {
    for &b in s.as_bytes() {
        if b == b'\n' {
            putchar(b'\r');
        }
        putchar(b);
    }
}

/// Read a line from the UART (blocking, until newline).
pub fn get_line(buf: &mut [u8]) -> usize {
    let mut i = 0;
    loop {
        let c = getchar();
        if c == b'\r' || c == b'\n' || i >= buf.len() - 1 {
            putchar(b'\n');
            putchar(b'\r');
            buf[i] = 0;
            return i;
        }
        putchar(c);
        buf[i] = c;
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uart_base() {
        assert_eq!(UART_BASE, 0x10000000);
    }

    #[test]
    fn test_register_offsets() {
        assert_eq!(RBR, 0);
        assert_eq!(THR, 0);
        assert_eq!(IER, 1);
        assert_eq!(LCR, 3);
        assert_eq!(LSR, 5);
    }

    #[test]
    fn test_lsr_bits() {
        assert_eq!(LSR_DR, 0x01);
        assert_eq!(LSR_THRE, 0x20);
    }
}
