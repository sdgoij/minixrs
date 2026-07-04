//! GPIO driver — pin mode control and read/write
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/system/gpio/gpio.c`
//!
//! Provides GPIO pin management: claiming, releasing, setting pin mode
//! (input/output), reading and writing values. Supports BeagleBone-
//! specific GPIO configurations. The driver uses VTreeFS in the original
//! MINIX; here we provide a clean Rust API without the VFS dependency.
//!
//! # GPIO Pin Numbers
//!
//! Pins are identified by a global pin number (0–127). Use
//! `gpio_global_pin(bank, pin)` to compute the global number, and
//! `gpio_parse_pin(global_pin)` to decompose it.

use crate::DriverError;
use core::cell::UnsafeCell;

/// GPIO pin direction/mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioMode {
    /// Digital input.
    Input,
    /// Digital output.
    Output,
    /// Alternate function.
    AltFn(u8),
}

/// Per-pin state.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct GpioPin {
    /// Whether this pin has been claimed.
    pub claimed: bool,
    /// The pin mode.
    pub mode: GpioMode,
    /// Current value (0 or 1).
    pub value: u8,
}

impl GpioPin {
    const fn new() -> Self {
        Self {
            claimed: false,
            mode: GpioMode::Input,
            value: 0,
        }
    }
}

/// Number of GPIO pins supported.
pub const GPIO_PINS: usize = 128;

/// Number of GPIO banks (OMAP/AM335x has 4 banks: 0-3).
pub const GPIO_BANKS: usize = 4;

/// Pins per bank.
pub const PINS_PER_BANK: usize = 32;

/// Beaglebone USR0 LED (bank 1, pin 21).
pub const BEAGLEBONE_USR0: usize = gpio_global_pin(1, 21);

/// Beaglebone USR1 LED (bank 1, pin 22).
pub const BEAGLEBONE_USR1: usize = gpio_global_pin(1, 22);

/// Beaglebone USR2 LED (bank 1, pin 23).
pub const BEAGLEBONE_USR2: usize = gpio_global_pin(1, 23);

/// Beaglebone USR3 LED (bank 1, pin 24).
pub const BEAGLEBONE_USR3: usize = gpio_global_pin(1, 24);

/// Beaglebone LCD enable pin (bank 0, pin 27).
pub const BEAGLEBONE_LCD_EN: usize = gpio_global_pin(0, 27);

/// Beaglebone USER button (bank 0, pin 26).
pub const BEAGLEBONE_USER_BTN: usize = gpio_global_pin(0, 26);

/// Compute a global pin number from (bank, pin).
///
/// Bank is 0–3, pin is 0–31.
pub const fn gpio_global_pin(bank: usize, pin: usize) -> usize {
    bank * PINS_PER_BANK + pin
}

/// Parse a global pin number into (bank, pin).
pub fn gpio_parse_pin(global: usize) -> Option<(usize, usize)> {
    if global >= GPIO_PINS {
        return None;
    }
    Some((global / PINS_PER_BANK, global % PINS_PER_BANK))
}

struct GpioStateCell(UnsafeCell<[GpioPin; GPIO_PINS]>);
unsafe impl Sync for GpioStateCell {}
impl GpioStateCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([GpioPin::new(); GPIO_PINS]))
    }
    fn get(&self) -> *mut [GpioPin; GPIO_PINS] {
        self.0.get()
    }
}

struct GpioOwnerCell(UnsafeCell<[[u8; 16]; GPIO_PINS]>);
unsafe impl Sync for GpioOwnerCell {}
impl GpioOwnerCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([[0u8; 16]; GPIO_PINS]))
    }
    fn get(&self) -> *mut [[u8; 16]; GPIO_PINS] {
        self.0.get()
    }
}

/// GPIO pin state table.
static GPIO_STATE: GpioStateCell = GpioStateCell::new();

/// Owner label for each claimed pin.
static GPIO_OWNER: GpioOwnerCell = GpioOwnerCell::new();

/// Initialize the GPIO driver.
///
/// Resets all pin state to unclaimed, input mode.
///
/// # Safety
///
/// Must be called exactly once during driver initialization, before any
/// other GPIO function. Writes to global mutable state.
pub unsafe fn gpio_init() {
    unsafe {
        for i in 0..GPIO_PINS {
            (*GPIO_STATE.get())[i] = GpioPin::new();
            (*GPIO_OWNER.get())[i] = [0u8; 16];
        }
    }
}

/// Claim a GPIO pin for exclusive use.
///
/// `label` is a short name (up to 15 bytes) identifying the claimant.
/// Returns an error if the pin is already claimed.
///
/// # Safety
///
/// Must be called with exclusive access to the GPIO state table.
pub unsafe fn gpio_claim(label: &[u8], nr: usize) -> Result<(), DriverError> {
    unsafe {
        if nr >= GPIO_PINS {
            return Err(DriverError::NotFound);
        }
        if (*GPIO_STATE.get())[nr].claimed {
            return Err(DriverError::Busy);
        }

        (*GPIO_STATE.get())[nr].claimed = true;
        let len = label.len().min(15);
        let owner = &mut *((*GPIO_OWNER.get()).as_mut_ptr().add(nr));
        owner[..len].copy_from_slice(&label[..len]);
        owner[len] = 0; // NUL-terminate
        Ok(())
    }
}

/// Release a previously claimed GPIO pin.
///
/// # Safety
///
/// Must be called with exclusive access to the GPIO state table.
pub unsafe fn gpio_release(nr: usize) {
    unsafe {
        if nr < GPIO_PINS {
            (*GPIO_STATE.get())[nr] = GpioPin::new();
            (*GPIO_OWNER.get())[nr] = [0u8; 16];
        }
    }
}

/// Set the mode (direction) of a GPIO pin.
///
/// The pin must have been claimed first.
///
/// # Safety
///
/// Must be called with exclusive access to the GPIO state table.
pub unsafe fn gpio_pin_mode(nr: usize, mode: GpioMode) -> Result<(), DriverError> {
    unsafe {
        if nr >= GPIO_PINS || !(*GPIO_STATE.get())[nr].claimed {
            return Err(DriverError::NotFound);
        }
        (*GPIO_STATE.get())[nr].mode = mode;
        Ok(())
    }
}

/// Read the value of a GPIO pin.
///
/// Returns 0 or 1. The pin must have been claimed first.
///
/// # Safety
///
/// Must be called with exclusive access to the GPIO state table.
pub unsafe fn gpio_read(nr: usize) -> Result<u8, DriverError> {
    unsafe {
        if nr >= GPIO_PINS || !(*GPIO_STATE.get())[nr].claimed {
            return Err(DriverError::NotFound);
        }
        Ok((*GPIO_STATE.get())[nr].value)
    }
}

/// Write a value to a GPIO pin.
///
/// `val` should be 0 or 1. The pin must be in output mode.
///
/// # Safety
///
/// Must be called with exclusive access to the GPIO state table.
pub unsafe fn gpio_write(nr: usize, val: u8) -> Result<(), DriverError> {
    unsafe {
        if nr >= GPIO_PINS || !(*GPIO_STATE.get())[nr].claimed {
            return Err(DriverError::NotFound);
        }
        if (*GPIO_STATE.get())[nr].mode != GpioMode::Output {
            return Err(DriverError::Unsupported);
        }
        (*GPIO_STATE.get())[nr].value = val & 1;
        Ok(())
    }
}

/// Get the number of pins managed by this driver.
pub fn gpio_pin_count() -> usize {
    GPIO_PINS
}

/// Reset all GPIO pins (for reboot).
///
/// # Safety
///
/// Must be called with exclusive access to the GPIO state table.
pub unsafe fn gpio_reset() {
    unsafe {
        gpio_init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpio_global_pin_0_0() {
        assert_eq!(gpio_global_pin(0, 0), 0);
    }

    #[test]
    fn test_gpio_global_pin_1_0() {
        assert_eq!(gpio_global_pin(1, 0), 32);
    }

    #[test]
    fn test_gpio_parse_pin_valid() {
        let (bank, pin) = gpio_parse_pin(37).unwrap();
        assert_eq!(bank, 1);
        assert_eq!(pin, 5);
    }

    #[test]
    fn test_gpio_parse_pin_invalid() {
        assert!(gpio_parse_pin(999).is_none());
    }

    #[test]
    fn test_gpio_init_resets_all() {
        unsafe {
            gpio_init();
            // Claim a pin, then re-init to verify reset.
            assert!(gpio_claim(b"test", 5).is_ok());
            gpio_init();
            // Pin should no longer be claimed.
            assert!(gpio_claim(b"test2", 5).is_ok());
        }
    }

    #[test]
    fn test_gpio_claim_and_release() {
        unsafe {
            gpio_init();
            assert!(gpio_claim(b"driver", 10).is_ok(), "claim should succeed");
            assert!(
                gpio_claim(b"other", 10).is_err(),
                "double claim should fail"
            );
            gpio_release(10);
            assert!(gpio_claim(b"new", 10).is_ok(), "re-claim should succeed");
        }
    }

    #[test]
    fn test_gpio_claim_out_of_range() {
        unsafe {
            gpio_init();
            assert!(gpio_claim(b"test", 999).is_err());
        }
    }

    #[test]
    fn test_gpio_pin_mode_default() {
        unsafe {
            gpio_init();
            assert!(gpio_claim(b"t", 7).is_ok());
            assert!(gpio_pin_mode(7, GpioMode::Output).is_ok());
            assert_eq!((*GPIO_STATE.get())[7].mode, GpioMode::Output);
        }
    }

    #[test]
    fn test_gpio_pin_mode_unclaimed() {
        unsafe {
            gpio_init();
            assert!(gpio_pin_mode(7, GpioMode::Output).is_err());
        }
    }

    #[test]
    fn test_gpio_read_write() {
        unsafe {
            gpio_init();
            assert!(gpio_claim(b"t", 3).is_ok());
            assert!(gpio_pin_mode(3, GpioMode::Output).is_ok());

            assert!(gpio_write(3, 1).is_ok());
            assert_eq!(gpio_read(3).unwrap(), 1);

            assert!(gpio_write(3, 0).is_ok());
            assert_eq!(gpio_read(3).unwrap(), 0);
        }
    }

    #[test]
    fn test_gpio_write_input_mode_fails() {
        unsafe {
            gpio_init();
            assert!(gpio_claim(b"t", 3).is_ok());
            assert!(gpio_pin_mode(3, GpioMode::Input).is_ok());
            assert!(gpio_write(3, 1).is_err(), "cannot write to input pin");
        }
    }

    #[test]
    fn test_gpio_read_unclaimed_fails() {
        unsafe {
            gpio_init();
            assert!(gpio_read(99).is_err());
        }
    }

    #[test]
    fn test_beaglebone_constants() {
        assert_eq!(BEAGLEBONE_USR0, gpio_global_pin(1, 21));
        assert_eq!(BEAGLEBONE_USR1, gpio_global_pin(1, 22));
        assert_eq!(BEAGLEBONE_USER_BTN, gpio_global_pin(0, 26));
        assert_eq!(BEAGLEBONE_LCD_EN, gpio_global_pin(0, 27));
    }

    #[test]
    fn test_gpio_pin_count() {
        assert_eq!(gpio_pin_count(), 128);
    }

    #[test]
    fn test_gpio_reset() {
        unsafe {
            gpio_init();
            assert!(gpio_claim(b"t", 0).is_ok());
            gpio_reset();
            assert!(gpio_claim(b"t2", 0).is_ok(), "should be free after reset");
        }
    }
}
