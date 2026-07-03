//! RS-232 serial port driver for UART 16550-compatible hardware.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/tty/tty/`
//!
//! Provides full UART 16550 register definitions, baud rate
//! configuration, line parameter setup, FIFO/interrupt management,
//! modem control, circular input buffering, and error tracking.

use crate::DriverError;

// ═══════════════════════════════════════════════════════════════════════════
// I/O backend trait
// ═══════════════════════════════════════════════════════════════════════════

/// Abstract I/O port access for the UART.
pub trait IoPort {
    fn inb(&self, port: u16) -> u8;
    fn outb(&mut self, port: u16, val: u8);
}

/// Real I/O port access using x86 `in`/`out` instructions.
pub struct RealIo;

impl IoPort for RealIo {
    fn inb(&self, port: u16) -> u8 {
        unsafe { crate::arch_io::inb(port) }
    }
    fn outb(&mut self, port: u16, val: u8) {
        unsafe { crate::arch_io::outb(port, val) }
    }
}

/// Mock I/O port for testing.
pub struct MockIo {
    pub ports: [u8; 0x1000],
}

#[allow(clippy::new_without_default)]
impl MockIo {
    pub fn new() -> Self {
        Self {
            ports: [0u8; 0x1000],
        }
    }
}

impl IoPort for MockIo {
    fn inb(&self, port: u16) -> u8 {
        self.ports[port as usize]
    }
    fn outb(&mut self, port: u16, val: u8) {
        self.ports[port as usize] = val;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// UART register offsets (relative to base port)
// ═══════════════════════════════════════════════════════════════════════════

pub const UART_RBR: u16 = 0; // Receive Buffer Register (read, DLAB=0)
pub const UART_THR: u16 = 0; // Transmit Holding Register (write, DLAB=0)
pub const UART_DLL: u16 = 0; // Divisor Latch Low (DLAB=1)
pub const UART_DLM: u16 = 1; // Divisor Latch High (DLAB=1)
pub const UART_IER: u16 = 1; // Interrupt Enable Register (DLAB=0)
pub const UART_IIR: u16 = 2; // Interrupt Identification Register (read)
pub const UART_FCR: u16 = 2; // FIFO Control Register (write)
pub const UART_LCR: u16 = 3; // Line Control Register
pub const UART_MCR: u16 = 4; // Modem Control Register
pub const UART_LSR: u16 = 5; // Line Status Register
pub const UART_MSR: u16 = 6; // Modem Status Register
pub const UART_SCR: u16 = 7; // Scratch Register

// ═══════════════════════════════════════════════════════════════════════════
// Register bit definitions
// ═══════════════════════════════════════════════════════════════════════════

// ── IER bits ─────────────────────────────────────────────────────────────

pub const IER_ERXBF: u8 = 0x01; // Enable Received Data Available Interrupt
pub const IER_ETXBE: u8 = 0x02; // Enable Transmitter Holding Register Empty Interrupt
pub const IER_ERLS: u8 = 0x04; // Enable Receiver Line Status Interrupt
pub const IER_EMSC: u8 = 0x08; // Enable Modem Status Interrupt

// ── IIR bits ─────────────────────────────────────────────────────────────

pub const IIR_IPEND: u8 = 0x01; // Interrupt Pending (0 = pending)
pub const IIR_IID: u8 = 0x0E; // Interrupt ID mask
pub const IIR_FIFO: u8 = 0xC0; // FIFO enable mask

/// Interrupt identification values (IIR bits 3-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartInterruptId {
    ModemStatus = 0,
    TxEmpty = 2,
    RxAvailable = 4,
    RxLineStatus = 6,
    Timeout = 12,
    Unknown = 0xFF,
}

impl From<u8> for UartInterruptId {
    fn from(iir: u8) -> Self {
        match (iir >> 1) & 7 {
            0 => UartInterruptId::ModemStatus,
            1 => UartInterruptId::TxEmpty,
            2 => UartInterruptId::RxAvailable,
            3 => UartInterruptId::RxLineStatus,
            6 => UartInterruptId::Timeout,
            _ => UartInterruptId::Unknown,
        }
    }
}

// ── FCR bits ─────────────────────────────────────────────────────────────

pub const FCR_ENABLE: u8 = 0x01; // Enable FIFOs
pub const FCR_CLEAR_RX: u8 = 0x02; // Clear Receiver FIFO
pub const FCR_CLEAR_TX: u8 = 0x04; // Clear Transmitter FIFO
pub const FCR_DMA_MODE: u8 = 0x08; // DMA mode select
pub const FCR_TRIGGER_1: u8 = 0x00; // 1 byte trigger
pub const FCR_TRIGGER_4: u8 = 0x40; // 4 bytes trigger
pub const FCR_TRIGGER_8: u8 = 0x80; // 8 bytes trigger
pub const FCR_TRIGGER_14: u8 = 0xC0; // 14 bytes trigger

// ── LCR bits ─────────────────────────────────────────────────────────────

pub const LCR_WLEN5: u8 = 0x00; // 5 data bits
pub const LCR_WLEN6: u8 = 0x01; // 6 data bits
pub const LCR_WLEN7: u8 = 0x02; // 7 data bits
pub const LCR_WLEN8: u8 = 0x03; // 8 data bits
pub const LCR_STOP: u8 = 0x04; // 2 stop bits (1 if clear)
pub const LCR_PEN: u8 = 0x08; // Parity Enable
pub const LCR_EPS: u8 = 0x10; // Even Parity Select (0 = odd)
pub const LCR_SP: u8 = 0x20; // Stick Parity
pub const LCR_BREAK: u8 = 0x40; // Break Control (send break)
pub const LCR_DLAB: u8 = 0x80; // Divisor Latch Access Bit

// ── MCR bits ─────────────────────────────────────────────────────────────

pub const MCR_DTR: u8 = 0x01; // Data Terminal Ready
pub const MCR_RTS: u8 = 0x02; // Request To Send
pub const MCR_OUT1: u8 = 0x04; // Auxiliary output 1
pub const MCR_OUT2: u8 = 0x08; // Auxiliary output 2 (IRQ enable on PC)
pub const MCR_LOOP: u8 = 0x10; // Loopback mode

// ── LSR bits ─────────────────────────────────────────────────────────────

pub const LSR_DR: u8 = 0x01; // Data Ready (receiver)
pub const LSR_OE: u8 = 0x02; // Overrun Error
pub const LSR_PE: u8 = 0x04; // Parity Error
pub const LSR_FE: u8 = 0x08; // Framing Error
pub const LSR_BI: u8 = 0x10; // Break Interrupt
pub const LSR_THRE: u8 = 0x20; // Transmitter Holding Register Empty
pub const LSR_TEMT: u8 = 0x40; // Transmitter Empty
pub const LSR_RXFE: u8 = 0x80; // Error in Receiver FIFO

// ── MSR bits ─────────────────────────────────────────────────────────────

pub const MSR_DCTS: u8 = 0x01; // Delta Clear To Send
pub const MSR_DDSR: u8 = 0x02; // Delta Data Set Ready
pub const MSR_TERI: u8 = 0x04; // Trailing Edge Ring Indicator
pub const MSR_DDCD: u8 = 0x08; // Delta Data Carrier Detect
pub const MSR_CTS: u8 = 0x10; // Clear To Send
pub const MSR_DSR: u8 = 0x20; // Data Set Ready
pub const MSR_RI: u8 = 0x40; // Ring Indicator
pub const MSR_DCD: u8 = 0x80; // Data Carrier Detect

// ═══════════════════════════════════════════════════════════════════════════
// Standard COM port base addresses
// ═══════════════════════════════════════════════════════════════════════════

pub const COM1_BASE: u16 = 0x3F8;
pub const COM2_BASE: u16 = 0x2F8;
pub const COM3_BASE: u16 = 0x3E8;
pub const COM4_BASE: u16 = 0x2E8;

/// System clock frequency (default 100 Hz, used for timeouts).
pub const SYSTEM_HZ: u32 = 100;

/// UART base clock (1.8432 MHz).
pub const UART_CLOCK: u32 = 1_843_200;

/// Default baud rate.
pub const DEFAULT_BAUD: u32 = 9600;

/// Circular input buffer size.
pub const RBUF_SIZE: usize = 256;

// ═══════════════════════════════════════════════════════════════════════════
// Data structures
// ═══════════════════════════════════════════════════════════════════════════

/// Line status error counters.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct RsError {
    pub overrun: u32,
    pub parity: u32,
    pub framing: u32,
    pub break_int: u32,
}

/// Circular input buffer.
pub struct RsInputBuf {
    pub buf: [u8; RBUF_SIZE],
    pub head: usize,
    pub tail: usize,
    pub count: usize,
}

impl RsInputBuf {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; RBUF_SIZE],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    pub fn push(&mut self, byte: u8) -> bool {
        if self.count >= RBUF_SIZE {
            return false;
        }
        self.buf[self.head] = byte;
        self.head = (self.head + 1) % RBUF_SIZE;
        self.count += 1;
        true
    }

    pub fn pop(&mut self) -> Option<u8> {
        if self.count == 0 {
            return None;
        }
        let byte = self.buf[self.tail];
        self.tail = (self.tail + 1) % RBUF_SIZE;
        self.count -= 1;
        Some(byte)
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn is_full(&self) -> bool {
        self.count >= RBUF_SIZE
    }
}

/// RS-232 serial port driver state.
pub struct Rs232Port<IO: IoPort> {
    pub io: IO,
    pub base: u16,
    pub baud: u32,
    pub lcr: u8,
    pub ier: u8,
    pub mcr: u8,
    pub fcr: u8,
    pub input_buf: RsInputBuf,
    pub error: RsError,
}

impl<IO: IoPort> Rs232Port<IO> {
    pub fn new(io: IO, base: u16) -> Self {
        Self {
            io,
            base,
            baud: DEFAULT_BAUD,
            lcr: LCR_WLEN8,
            ier: 0,
            mcr: 0,
            fcr: 0,
            input_buf: RsInputBuf::new(),
            error: RsError::default(),
        }
    }

    // ── Register access ───────────────────────────────────────────────

    pub fn read_reg(&self, reg: u16) -> u8 {
        self.io.inb(self.base + reg)
    }
    pub fn write_reg(&mut self, reg: u16, val: u8) {
        self.io.outb(self.base + reg, val);
    }

    // ── Baud rate ──────────────────────────────────────────────────────

    pub fn set_baud(&mut self, baud: u32) -> Result<(), DriverError> {
        if baud == 0 {
            return Err(DriverError::InvalidArgument);
        }
        let divisor = (UART_CLOCK / 16 / baud) as u16;
        if divisor == 0 {
            return Err(DriverError::InvalidArgument);
        }
        let lcr = self.read_reg(UART_LCR);
        self.write_reg(UART_LCR, lcr | LCR_DLAB);
        self.write_reg(UART_DLL, (divisor & 0xFF) as u8);
        self.write_reg(UART_DLM, (divisor >> 8) as u8);
        self.write_reg(UART_LCR, lcr);
        self.baud = baud;
        Ok(())
    }

    pub fn baud(&self) -> u32 {
        self.baud
    }

    // ── Line control ──────────────────────────────────────────────────

    pub fn set_line_params(&mut self, bits: u8, parity: u8, stop: u8) {
        let mut lcr = match bits {
            5 => LCR_WLEN5,
            6 => LCR_WLEN6,
            7 => LCR_WLEN7,
            _ => LCR_WLEN8,
        };
        lcr |= match parity {
            1 => LCR_PEN | LCR_EPS,          // even
            2 => LCR_PEN,                    // odd
            3 => LCR_PEN | LCR_SP | LCR_EPS, // mark (stick parity, even = 1)
            4 => LCR_PEN | LCR_SP,           // space (stick parity, odd = 0)
            _ => 0,                          // none
        };
        if stop >= 2 {
            lcr |= LCR_STOP;
        }
        self.write_reg(UART_LCR, lcr);
        self.lcr = lcr;
    }

    // ── FIFO control ───────────────────────────────────────────────────

    pub fn set_fifo(&mut self, trigger: u8) {
        let fcr = FCR_ENABLE | FCR_CLEAR_RX | FCR_CLEAR_TX | (trigger & FCR_TRIGGER_14);
        self.write_reg(UART_FCR, fcr);
        self.fcr = fcr;
    }

    // ── Interrupt control ──────────────────────────────────────────────

    pub fn set_interrupts(&mut self, ier: u8) {
        self.write_reg(UART_IER, ier);
        self.ier = ier;
    }

    pub fn get_interrupt_id(&self) -> UartInterruptId {
        UartInterruptId::from(self.read_reg(UART_IIR))
    }

    // ── Modem control ──────────────────────────────────────────────────

    pub fn set_modem(&mut self, mcr: u8) {
        self.write_reg(UART_MCR, mcr);
        self.mcr = mcr;
    }

    pub fn get_modem_status(&self) -> u8 {
        self.read_reg(UART_MSR)
    }

    pub fn set_dtr_rts(&mut self, dtr: bool, rts: bool) {
        let mut mcr = self.read_reg(UART_MCR);
        if dtr {
            mcr |= MCR_DTR;
        } else {
            mcr &= !MCR_DTR;
        }
        if rts {
            mcr |= MCR_RTS;
        } else {
            mcr &= !MCR_RTS;
        }
        self.set_modem(mcr);
    }

    // ── Transmit / receive ─────────────────────────────────────────────

    pub fn is_data_ready(&self) -> bool {
        self.read_reg(UART_LSR) & LSR_DR != 0
    }

    pub fn is_tx_empty(&self) -> bool {
        self.read_reg(UART_LSR) & LSR_THRE != 0
    }

    pub fn receive_byte(&mut self) -> Option<u8> {
        let lsr = self.read_reg(UART_LSR);
        if lsr & LSR_DR == 0 {
            return None;
        }
        // Check for errors
        if lsr & LSR_OE != 0 {
            self.error.overrun += 1;
        }
        if lsr & LSR_PE != 0 {
            self.error.parity += 1;
        }
        if lsr & LSR_FE != 0 {
            self.error.framing += 1;
        }
        if lsr & LSR_BI != 0 {
            self.error.break_int += 1;
        }
        let byte = self.read_reg(UART_RBR);
        // Reading RBR clears DR on real hardware; simulate for mock.
        self.write_reg(UART_LSR, lsr & !LSR_DR);
        Some(byte)
    }

    pub fn transmit_byte(&mut self, byte: u8) -> bool {
        if !self.is_tx_empty() {
            return false;
        }
        self.write_reg(UART_THR, byte);
        true
    }

    pub fn drain_input(&mut self) -> usize {
        let mut count = 0;
        while let Some(byte) = self.receive_byte() {
            self.input_buf.push(byte);
            count += 1;
        }
        count
    }

    // ── Break control ──────────────────────────────────────────────────

    pub fn send_break(&mut self, on: bool) {
        let mut lcr = self.read_reg(UART_LCR);
        if on {
            lcr |= LCR_BREAK;
        } else {
            lcr &= !LCR_BREAK;
        }
        self.write_reg(UART_LCR, lcr);
        self.lcr = lcr;
    }

    // ── Initialization ─────────────────────────────────────────────────

    pub fn init(&mut self, baud: u32, mcr_out2: bool) -> Result<(), DriverError> {
        self.set_baud(baud)?;
        self.set_line_params(8, 0, 1); // 8N1
        self.set_fifo(FCR_TRIGGER_14);
        let mut mcr = MCR_DTR | MCR_RTS;
        if mcr_out2 {
            mcr |= MCR_OUT2;
        }
        self.set_modem(mcr);
        self.set_interrupts(IER_ERXBF | IER_ERLS);
        // Drain any stale input
        self.drain_input();
        Ok(())
    }

    /// Handle an interrupt — read IIR and dispatch.
    /// Returns the interrupt ID that was handled.
    pub fn handle_interrupt(&mut self) -> UartInterruptId {
        let iid = self.get_interrupt_id();
        if iid == UartInterruptId::Unknown {
            return iid;
        }
        match iid {
            UartInterruptId::RxAvailable | UartInterruptId::Timeout => {
                self.drain_input();
            }
            UartInterruptId::TxEmpty => {
                // The TTY layer will call transmit when data is available.
            }
            UartInterruptId::RxLineStatus => {
                // Read LSR to clear error condition
                self.read_reg(UART_LSR);
            }
            UartInterruptId::ModemStatus => {
                self.read_reg(UART_MSR); // clear
            }
            _ => {}
        }
        iid
    }

    /// Read a byte from the input buffer (non-blocking).
    pub fn read_byte(&mut self) -> Option<u8> {
        self.input_buf.pop()
    }

    /// Write a byte to the transmitter (non-blocking).
    pub fn write_byte(&mut self, byte: u8) -> bool {
        self.transmit_byte(byte)
    }

    /// Borrow the input buffer for examination.
    pub fn input_count(&self) -> usize {
        self.input_buf.count
    }

    /// Borrow the error counters.
    pub fn errors(&self) -> &RsError {
        &self.error
    }
}

// ── Default implementations ─────────────────────────────────────────────

#[allow(clippy::new_without_default)]
impl Default for RsInputBuf {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_port() -> Rs232Port<MockIo> {
        let mut p = Rs232Port::new(MockIo::new(), COM1_BASE);
        // Pre-init the IO to return LSR_THRE so transmits don't block
        p.io.ports[(COM1_BASE + UART_LSR) as usize] = LSR_THRE;
        p
    }

    #[test]
    fn test_com_port_addresses() {
        assert_eq!(COM1_BASE, 0x3F8);
        assert_eq!(COM2_BASE, 0x2F8);
        assert_eq!(COM3_BASE, 0x3E8);
        assert_eq!(COM4_BASE, 0x2E8);
    }

    #[test]
    fn test_register_offsets() {
        assert_eq!(UART_RBR, 0);
        assert_eq!(UART_THR, 0);
        assert_eq!(UART_IER, 1);
        assert_eq!(UART_IIR, 2);
        assert_eq!(UART_FCR, 2);
        assert_eq!(UART_LCR, 3);
        assert_eq!(UART_MCR, 4);
        assert_eq!(UART_LSR, 5);
        assert_eq!(UART_MSR, 6);
        assert_eq!(UART_SCR, 7);
    }

    #[test]
    fn test_ier_bits() {
        assert_eq!(IER_ERXBF, 0x01);
        assert_eq!(IER_ETXBE, 0x02);
        assert_eq!(IER_ERLS, 0x04);
        assert_eq!(IER_EMSC, 0x08);
    }

    #[test]
    fn test_lcr_bits() {
        assert_eq!(LCR_WLEN8, 0x03);
        assert_eq!(LCR_STOP, 0x04);
        assert_eq!(LCR_PEN, 0x08);
        assert_eq!(LCR_DLAB, 0x80);
    }

    #[test]
    fn test_mcr_bits() {
        assert_eq!(MCR_DTR, 0x01);
        assert_eq!(MCR_RTS, 0x02);
        assert_eq!(MCR_OUT2, 0x08);
        assert_eq!(MCR_LOOP, 0x10);
    }

    #[test]
    fn test_lsr_bits() {
        assert_eq!(LSR_DR, 0x01);
        assert_eq!(LSR_OE, 0x02);
        assert_eq!(LSR_THRE, 0x20);
        assert_eq!(LSR_TEMT, 0x40);
    }

    #[test]
    fn test_msr_bits() {
        assert_eq!(MSR_CTS, 0x10);
        assert_eq!(MSR_DSR, 0x20);
        assert_eq!(MSR_RI, 0x40);
        assert_eq!(MSR_DCD, 0x80);
    }

    #[test]
    fn test_iir_decode() {
        assert_eq!(UartInterruptId::from(0x06), UartInterruptId::RxLineStatus);
        assert_eq!(UartInterruptId::from(0x04), UartInterruptId::RxAvailable);
        assert_eq!(UartInterruptId::from(0x0C), UartInterruptId::Timeout);
        assert_eq!(UartInterruptId::from(0x02), UartInterruptId::TxEmpty);
        assert_eq!(UartInterruptId::from(0x00), UartInterruptId::ModemStatus);
    }

    #[test]
    fn test_set_baud() {
        let mut p = make_port();
        assert!(p.set_baud(9600).is_ok());
        assert_eq!(p.baud(), 9600);
    }

    #[test]
    fn test_set_baud_zero_fails() {
        let mut p = make_port();
        assert!(p.set_baud(0).is_err());
    }

    #[test]
    fn test_line_params_8n1() {
        let mut p = make_port();
        p.set_line_params(8, 0, 1);
        assert_eq!(p.lcr, LCR_WLEN8);
    }

    #[test]
    fn test_line_params_7e2() {
        let mut p = make_port();
        p.set_line_params(7, 1, 2);
        assert_eq!(p.lcr, LCR_WLEN7 | LCR_STOP | LCR_PEN | LCR_EPS);
    }

    #[test]
    fn test_fifo() {
        let mut p = make_port();
        p.set_fifo(FCR_TRIGGER_14);
        assert!(p.fcr & FCR_ENABLE != 0);
        assert!(p.fcr & FCR_TRIGGER_14 != 0);
    }

    #[test]
    fn test_transmit() {
        let mut p = make_port();
        assert!(p.transmit_byte(0x55));
        assert_eq!(p.io.ports[(COM1_BASE + UART_THR) as usize], 0x55);
    }

    #[test]
    fn test_receive() {
        let mut p = make_port();
        p.io.ports[(COM1_BASE + UART_LSR) as usize] = LSR_DR;
        p.io.ports[(COM1_BASE + UART_RBR) as usize] = 0xAA;
        assert_eq!(p.receive_byte(), Some(0xAA));
    }

    #[test]
    fn test_receive_no_data() {
        let mut p = make_port();
        assert_eq!(p.receive_byte(), None);
    }

    #[test]
    fn test_drain_input() {
        let mut p = make_port();
        p.io.ports[(COM1_BASE + UART_LSR) as usize] = LSR_DR | LSR_DR;
        p.io.ports[(COM1_BASE + UART_RBR) as usize] = 0x11;
        let n = p.drain_input();
        assert!(n > 0);
        assert_eq!(p.input_buf.count, 1);
        assert_eq!(p.read_byte(), Some(0x11));
    }

    #[test]
    fn test_input_buffer_wrap() {
        let mut buf = RsInputBuf::new();
        for i in 0..RBUF_SIZE {
            assert!(buf.push((i & 0xFF) as u8));
        }
        assert!(buf.is_full());
        assert!(!buf.push(0xFF));
        for i in 0..RBUF_SIZE {
            assert_eq!(buf.pop(), Some((i & 0xFF) as u8));
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn test_init_sequence() {
        let mut p = make_port();
        assert!(p.init(115200, true).is_ok());
        assert_eq!(p.baud(), 115200);
    }

    #[test]
    fn test_send_break() {
        let mut p = make_port();
        p.send_break(true);
        assert!(p.lcr & LCR_BREAK != 0);
        p.send_break(false);
        assert!(p.lcr & LCR_BREAK == 0);
    }

    #[test]
    fn test_dtr_rts_control() {
        let mut p = make_port();
        p.set_dtr_rts(true, false);
        assert!(p.mcr & MCR_DTR != 0);
        assert!(p.mcr & MCR_RTS == 0);
    }

    #[test]
    fn test_modem_status() {
        let mut p = make_port();
        p.io.ports[(COM1_BASE + UART_MSR) as usize] = MSR_CTS | MSR_DCD;
        assert!(p.get_modem_status() & MSR_CTS != 0);
        assert!(p.get_modem_status() & MSR_DCD != 0);
    }

    #[test]
    fn test_error_counters() {
        let mut p = make_port();
        p.io.ports[(COM1_BASE + UART_LSR) as usize] = LSR_DR | LSR_OE | LSR_PE;
        p.receive_byte();
        assert_eq!(p.error.overrun, 1);
        assert_eq!(p.error.parity, 1);
    }

    #[test]
    fn test_interrupt_handler_rx() {
        let mut p = make_port();
        p.io.ports[(COM1_BASE + UART_IIR) as usize] = 0x04; // RxAvailable
        p.io.ports[(COM1_BASE + UART_LSR) as usize] = LSR_DR;
        p.io.ports[(COM1_BASE + UART_RBR) as usize] = 0x42;
        let id = p.handle_interrupt();
        assert_eq!(id, UartInterruptId::RxAvailable);
        assert_eq!(p.read_byte(), Some(0x42));
    }

    #[test]
    fn test_tx_empty_flag() {
        let mut p = make_port();
        p.io.ports[(COM1_BASE + UART_LSR) as usize] = LSR_THRE;
        assert!(p.is_tx_empty());
    }

    #[test]
    fn test_write_byte_via_method() {
        let mut p = make_port();
        assert!(p.write_byte(0x77));
        assert_eq!(p.io.ports[(COM1_BASE + UART_THR) as usize], 0x77);
    }

    #[test]
    fn test_default_baud() {
        let p = make_port();
        assert_eq!(p.baud(), DEFAULT_BAUD);
    }

    #[test]
    fn test_read_byte_empty() {
        let mut p = make_port();
        assert_eq!(p.read_byte(), None);
    }
}
