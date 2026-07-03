//! PS/2 controller I/O — port-level read/write and initialization sequences.
//!
//! Ported from `pckbd.c` functions `kbc_cmd0`, `kbc_cmd1`, `kbc_read`,
//! `scan_keyboard`, `kb_wait`, `kb_init`, and `set_leds`.
//!
//! This module provides safe abstractions over the raw I/O port access
//! needed to talk to the PS/2 controller.

#![allow(clippy::identity_op)]

use crate::input::constants::*;

/// PS/2 controller error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerError {
    /// Timed out waiting for controller to be ready.
    Timeout,
    /// Unexpected data on the data port.
    UnexpectedData,
}

/// Result type for the PS/2 controller.
pub type ControllerResult<T> = Result<T, ControllerError>;

/// A raw PS/2 controller interface.
///
/// Reads and writes to the I/O ports are performed by the provided
/// `IoBackend`.  This allows the controller to be tested without real hardware.
pub trait IoBackend {
    /// Read a byte from the given I/O port.
    ///
    /// # Safety
    ///
    /// Reading from I/O ports may have side-effects on hardware.
    unsafe fn inb(port: u16) -> u8;

    /// Write a byte to the given I/O port.
    ///
    /// # Safety
    ///
    /// Writing to I/O ports may have side-effects on hardware.
    unsafe fn outb(port: u16, value: u8);
}

/// A minimal I/O backend that delegates to the actual x86 `inb`/`outb`
/// instructions.
///
/// This is a placeholder — each platform must provide its own implementation.
pub struct RealIo;

impl IoBackend for RealIo {
    unsafe fn inb(port: u16) -> u8 {
        unsafe { crate::arch_io::inb(port) }
    }

    unsafe fn outb(port: u16, value: u8) {
        unsafe { crate::arch_io::outb(port, value) }
    }
}

/// PS/2 controller abstraction over I/O ports.
#[derive(Debug, Clone, Copy)]
pub struct Ps2Controller;

impl Ps2Controller {
    /// Read the keyboard status port.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn read_status<IO: IoBackend>() -> u8 {
        unsafe { IO::inb(KB_STATUS) }
    }

    /// Read the keyboard data port.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn read_data<IO: IoBackend>() -> u8 {
        unsafe { IO::inb(KEYBD) }
    }

    /// Write a command to the controller command port.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn write_command<IO: IoBackend>(cmd: u8) {
        unsafe { IO::outb(KB_COMMAND, cmd) }
    }

    /// Write data to the keyboard data port.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn write_data<IO: IoBackend>(data: u8) {
        unsafe { IO::outb(KEYBD, data) }
    }

    /// Wait for the controller to be ready (input buffer empty).
    ///
    /// Returns `Err(Timeout)` if the controller does not become ready within
    /// `KBC_WAIT_TIME` polling iterations.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.  May discard pending input.
    pub unsafe fn wait_ready<IO: IoBackend>() -> ControllerResult<()> {
        for _ in 0..KBC_WAIT_TIME {
            let status = unsafe { IO::inb(KB_STATUS) };
            // Discard any pending output so we can check for input buffer empty.
            if status & KB_OUT_FULL != 0 {
                unsafe {
                    let _ = IO::inb(KEYBD);
                }
            }
            if status & (KB_IN_FULL | KB_OUT_FULL) == 0 {
                return Ok(());
            }
        }
        Err(ControllerError::Timeout)
    }

    /// Read a byte from the keyboard or controller, waiting up to
    /// `KBC_READ_TIME` iterations.
    ///
    /// Returns `Err(Timeout)` if no byte appears in time.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn read_byte<IO: IoBackend>() -> ControllerResult<u8> {
        for _ in 0..KBC_READ_TIME {
            let status = unsafe { IO::inb(KB_STATUS) };
            if status & KB_OUT_FULL != 0 {
                return unsafe { Ok(IO::inb(KEYBD)) };
            }
        }
        Err(ControllerError::Timeout)
    }

    /// Send a command with no data byte.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn cmd0<IO: IoBackend>(cmd: u8) -> ControllerResult<()> {
        unsafe { Self::wait_ready::<IO>()? };
        unsafe { IO::outb(KB_COMMAND, cmd) };
        Ok(())
    }

    /// Send a command with one data byte.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn cmd1<IO: IoBackend>(cmd: u8, data: u8) -> ControllerResult<()> {
        unsafe { Self::wait_ready::<IO>()? };
        unsafe { IO::outb(KB_COMMAND, cmd) };
        unsafe { Self::wait_ready::<IO>()? };
        unsafe { IO::outb(KEYBD, data) };
        Ok(())
    }

    /// Read the controller command byte (CCB).
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn read_ccb<IO: IoBackend>() -> ControllerResult<u8> {
        unsafe { Self::cmd0::<IO>(KBC_RD_RAM_CCB)? };
        unsafe { Self::read_byte::<IO>() }
    }

    /// Write the controller command byte (CCB).
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn write_ccb<IO: IoBackend>(ccb: u8) -> ControllerResult<()> {
        unsafe { Self::cmd1::<IO>(KBC_WR_RAM_CCB, ccb) }
    }

    /// Scan the keyboard for an input byte.
    ///
    /// Returns `(scancode, is_aux)` if data is available.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn scan_keyboard<IO: IoBackend>() -> Option<(u8, bool)> {
        let status = unsafe { IO::inb(KB_STATUS) };
        if status & KB_OUT_FULL == 0 {
            return None;
        }
        let data = unsafe { IO::inb(KEYBD) };
        let is_aux = (status & KB_AUX_BYTE) != 0;
        Some((data, is_aux))
    }

    /// Initialize the keyboard and auxiliary (mouse) interfaces.
    ///
    /// This performs the full init sequence:
    /// 1. Disable keyboard and auxiliary
    /// 2. Read CCB
    /// 3. Enable both interrupts (OR 3)
    /// 4. Enable keyboard and auxiliary
    ///
    /// # Safety
    ///
    /// Raw I/O port access.  Should only be called once at driver startup.
    pub unsafe fn init<IO: IoBackend>() -> ControllerResult<()> {
        // Discard any leftover data
        unsafe {
            let _ = Self::scan_keyboard::<IO>();
        }

        // Disable devices
        unsafe { Self::cmd0::<IO>(KBC_DI_KBD)? };
        unsafe { Self::cmd0::<IO>(KBC_DI_AUX)? };

        // Read and modify CCB: enable keyboard and auxiliary interrupts
        let ccb = unsafe { Self::read_ccb::<IO>()? };
        unsafe { Self::write_ccb::<IO>(ccb | 3)? };

        // Enable devices
        unsafe { Self::cmd0::<IO>(KBC_EN_KBD)? };
        unsafe { Self::cmd0::<IO>(KBC_EN_AUX)? };

        Ok(())
    }

    /// Queue a command to set the keyboard LEDs.
    ///
    /// `led_mask` should be a combination of `LED_SCROLL_LOCK`, `LED_NUM_LOCK`,
    /// and `LED_CAPS_LOCK`.
    ///
    /// # Safety
    ///
    /// Raw I/O port access.
    pub unsafe fn set_leds<IO: IoBackend>(led_mask: u8) -> ControllerResult<()> {
        unsafe { Self::wait_ready::<IO>()? };
        unsafe { IO::outb(KEYBD, LED_CODE) };
        unsafe { Self::wait_ready::<IO>()? };
        unsafe { IO::outb(KEYBD, led_mask) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock I/O backend for testing.
    struct MockIo;

    impl IoBackend for MockIo {
        unsafe fn inb(port: u16) -> u8 {
            if port == KB_STATUS {
                let s = MOCK_STATUS.load(core::sync::atomic::Ordering::Relaxed);
                MOCK_STATUS.store(0, core::sync::atomic::Ordering::Relaxed);
                s
            } else {
                MOCK_DATA.load(core::sync::atomic::Ordering::Relaxed)
            }
        }

        unsafe fn outb(port: u16, value: u8) {
            if port == KB_COMMAND {
                MOCK_COMMANDS.store(value, core::sync::atomic::Ordering::Relaxed);
            } else {
                MOCK_DATA_WRITES.store(value, core::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    use core::sync::atomic::AtomicU8;

    static MOCK_STATUS: AtomicU8 = AtomicU8::new(0);
    static MOCK_DATA: AtomicU8 = AtomicU8::new(0);
    static MOCK_COMMANDS: AtomicU8 = AtomicU8::new(0);
    static MOCK_DATA_WRITES: AtomicU8 = AtomicU8::new(0);

    #[test]
    fn test_io_port_addresses() {
        assert_eq!(KEYBD, 0x60);
        assert_eq!(KB_COMMAND, 0x64);
        assert_eq!(KB_STATUS, 0x64);
    }

    #[test]
    fn test_controller_cmd0() {
        MOCK_COMMANDS.store(0, core::sync::atomic::Ordering::Relaxed);
        unsafe {
            Ps2Controller::cmd0::<MockIo>(KBC_DI_KBD).unwrap();
            assert_eq!(
                MOCK_COMMANDS.load(core::sync::atomic::Ordering::Relaxed),
                KBC_DI_KBD
            );
        }
    }

    #[test]
    fn test_controller_cmd1() {
        MOCK_COMMANDS.store(0, core::sync::atomic::Ordering::Relaxed);
        MOCK_DATA_WRITES.store(0, core::sync::atomic::Ordering::Relaxed);
        unsafe {
            Ps2Controller::cmd1::<MockIo>(KBC_WR_RAM_CCB, 0x47).unwrap();
            assert_eq!(
                MOCK_COMMANDS.load(core::sync::atomic::Ordering::Relaxed),
                KBC_WR_RAM_CCB
            );
            assert_eq!(
                MOCK_DATA_WRITES.load(core::sync::atomic::Ordering::Relaxed),
                0x47
            );
        }
    }

    #[test]
    fn test_status_bit_constants() {
        assert_eq!(KB_OUT_FULL, 0x01);
        assert_eq!(KB_IN_FULL, 0x02);
        assert_eq!(KB_AUX_BYTE, 0x20);
    }

    #[test]
    fn test_controller_command_constants() {
        assert_eq!(KBC_RD_RAM_CCB, 0x20);
        assert_eq!(KBC_WR_RAM_CCB, 0x60);
        assert_eq!(KBC_DI_AUX, 0xA7);
        assert_eq!(KBC_EN_AUX, 0xA8);
        assert_eq!(KBC_DI_KBD, 0xAD);
        assert_eq!(KBC_EN_KBD, 0xAE);
    }

    #[test]
    fn test_led_command_constants() {
        assert_eq!(LED_CODE, 0xED);
        assert_eq!(LED_SCROLL_LOCK, 0x01);
        assert_eq!(LED_NUM_LOCK, 0x02);
        assert_eq!(LED_CAPS_LOCK, 0x04);
    }
}
