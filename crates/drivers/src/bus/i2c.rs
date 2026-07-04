//! I2C bus driver — device reservation and ioctl exec.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/bus/i2c/i2c.c`
//!
//! Provides the I2C bus management layer: device reservation table
//! with endpoint tracking, ioctl exec validation, and a hardware-
//! specific process callback. Supports up to 1024 devices (10-bit
//! addressing).

use crate::DriverError;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

/// Maximum I2C devices (10-bit addressing: 0-1023).
pub const NR_I2C_DEV: usize = 1024;

/// Maximum key length for device labels.
pub const DS_MAX_KEYLEN: usize = 64;

/// An I2C device reservation entry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct I2cDevice {
    /// Whether this slot is in use.
    pub inuse: bool,
    /// Endpoint that reserved the device.
    pub endpt: i32,
    /// Label key for the reservation.
    pub key: [u8; DS_MAX_KEYLEN],
}

impl I2cDevice {
    const fn new() -> Self {
        Self {
            inuse: false,
            endpt: -1,
            key: [0u8; DS_MAX_KEYLEN],
        }
    }
}

pub use crate::eeprom::cat24c256::I2cExec;

/// Type for the hardware-specific I2C process function.
///
/// Implemented by architecture-specific code (e.g., OMAP I2C).
pub type I2cProcessFn = fn(ioctl: &mut I2cExec) -> Result<(), DriverError>;

struct I2cDevicesCell(UnsafeCell<[I2cDevice; NR_I2C_DEV]>);
unsafe impl Sync for I2cDevicesCell {}
impl I2cDevicesCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([I2cDevice::new(); NR_I2C_DEV]))
    }
    fn get(&self) -> *mut [I2cDevice; NR_I2C_DEV] {
        self.0.get()
    }
}

/// Device reservation table.
static I2C_DEVICES: I2cDevicesCell = I2cDevicesCell::new();

/// I2C bus ID for this driver instance.
static I2C_BUS_ID: AtomicU32 = AtomicU32::new(0);

struct I2cProcessCell(UnsafeCell<Option<I2cProcessFn>>);
unsafe impl Sync for I2cProcessCell {}
impl I2cProcessCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(None))
    }
    fn get(&self) -> *mut Option<I2cProcessFn> {
        self.0.get()
    }
}

/// Hardware-specific process callback.
static I2C_PROCESS: I2cProcessCell = I2cProcessCell::new();

/// Build a reservation key: "drv.i2c.{bus+1}.{label}" into the output buffer.
///
/// Returns the number of bytes written (not including NUL).
fn build_key(bus_id: u32, label: &[u8], out: &mut [u8]) -> usize {
    let prefix = b"drv.i2c.";
    let mut pos = 0;
    // Copy prefix.
    for &b in prefix.iter() {
        if pos < out.len() {
            out[pos] = b;
            pos += 1;
        }
    }
    // Append bus+1 as decimal.
    let bus_val = bus_id.wrapping_add(1);
    if bus_val >= 1000 && pos < out.len() {
        out[pos] = b'0' + (bus_val / 1000) as u8;
        pos += 1;
    }
    if bus_val >= 100 && pos < out.len() {
        out[pos] = b'0' + ((bus_val / 100) % 10) as u8;
        pos += 1;
    }
    if bus_val >= 10 && pos < out.len() {
        out[pos] = b'0' + ((bus_val / 10) % 10) as u8;
        pos += 1;
    }
    if pos < out.len() {
        out[pos] = b'0' + (bus_val % 10) as u8;
        pos += 1;
    }
    // Append '.'.
    if pos < out.len() {
        out[pos] = b'.';
        pos += 1;
    }
    // Append label.
    for &b in label.iter() {
        if pos < out.len() {
            out[pos] = b;
            pos += 1;
        }
    }
    // NUL-terminate if space.
    if pos < out.len() {
        out[pos] = 0;
    }
    pos
}

/// Initialize the I2C driver.
///
/// `bus_id` is the 0-based bus number. `process_fn` is the hardware-
/// specific callback for executing I2C transactions.
///
/// # Safety
///
/// Must be called exactly once during driver initialization.
pub unsafe fn i2c_init(bus_id: u32, process_fn: I2cProcessFn) {
    I2C_BUS_ID.store(bus_id, Ordering::Relaxed);
    unsafe {
        let devs = I2C_DEVICES.get();
        let ptr: *mut I2cDevice = (*devs).as_mut_ptr();
        for i in 0..NR_I2C_DEV {
            core::ptr::write(ptr.add(i), I2cDevice::new());
        }
        *I2C_PROCESS.get() = Some(process_fn);
    }
}

/// Get the I2C bus ID.
pub fn i2c_bus_id() -> u32 {
    I2C_BUS_ID.load(Ordering::Relaxed)
}

/// Reserve an I2C device for exclusive use by an endpoint.
///
/// `slave_addr` is the 7- or 10-bit I2C address. `label` is the
/// driver label used for the reservation key.
///
/// # Safety
///
/// Must be called with exclusive access to the reservation table.
pub unsafe fn i2c_reserve(endpt: i32, slave_addr: usize, label: &[u8]) -> Result<(), DriverError> {
    unsafe {
        if slave_addr >= NR_I2C_DEV {
            return Err(DriverError::InvalidArgument);
        }

        let bus_id = 0; // fixed for now
        let mut key = [0u8; DS_MAX_KEYLEN];
        build_key(bus_id, label, &mut key);

        let devs = I2C_DEVICES.get();
        let dev = &mut (*devs)[slave_addr];
        if dev.inuse && dev.key != key {
            return Err(DriverError::Busy);
        }

        dev.inuse = true;
        dev.endpt = endpt;
        dev.key = key;
        Ok(())
    }
}

/// Check if a device is reserved by a given endpoint.
///
/// # Safety
///
/// Must be called with exclusive access to the reservation table.
pub unsafe fn i2c_check_reservation(endpt: i32, slave_addr: usize) -> Result<(), DriverError> {
    unsafe {
        if slave_addr >= NR_I2C_DEV {
            return Err(DriverError::InvalidArgument);
        }

        let devs = I2C_DEVICES.get();
        let dev = &(*devs)[slave_addr];
        if !dev.inuse || dev.endpt != endpt {
            return Err(DriverError::NotFound);
        }
        Ok(())
    }
}

/// Release a device reservation by endpoint.
///
/// # Safety
///
/// Must be called with exclusive access to the reservation table.
pub unsafe fn i2c_release(endpt: i32) {
    unsafe {
        let devs = I2C_DEVICES.get();
        let ptr: *mut I2cDevice = (*devs).as_mut_ptr();
        for i in 0..NR_I2C_DEV {
            let dev = &mut *ptr.add(i);
            if dev.inuse && dev.endpt == endpt {
                *dev = I2cDevice::new();
            }
        }
    }
}

/// Execute an I2C transaction via the hardware callback.
///
/// # Safety
///
/// Must be called with exclusive access to the I2C bus.
pub unsafe fn i2c_exec(ioctl: &mut I2cExec) -> Result<(), DriverError> {
    unsafe {
        match *I2C_PROCESS.get() {
            Some(process) => process(ioctl),
            None => Err(DriverError::Unsupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_i2c_init_clears_table() {
        unsafe {
            i2c_init(0, |_ioctl| Err(DriverError::Unsupported));
            let devs = I2C_DEVICES.get();
            for i in 0..10 {
                let dev = &(*devs)[i];
                assert!(!dev.inuse);
                assert_eq!(dev.endpt, -1);
            }
        }
    }

    #[test]
    fn test_i2c_reserve_valid() {
        unsafe {
            i2c_init(0, |_ioctl| Err(DriverError::Unsupported));
            assert!(i2c_reserve(100, 0x50, b"cat24c256").is_ok());
            let devs = I2C_DEVICES.get();
            let dev = &(*devs)[0x50];
            assert!(dev.inuse);
            assert_eq!(dev.endpt, 100);
        }
    }

    #[test]
    fn test_i2c_reserve_out_of_range() {
        unsafe {
            i2c_init(0, |_ioctl| Err(DriverError::Unsupported));
            assert!(i2c_reserve(100, 9999, b"test").is_err());
        }
    }

    #[test]
    fn test_i2c_reserve_twice_same_endpoint() {
        unsafe {
            i2c_init(0, |_ioctl| Err(DriverError::Unsupported));
            assert!(i2c_reserve(100, 0x50, b"drv").is_ok());
            assert!(i2c_reserve(100, 0x50, b"drv").is_ok());
        }
    }

    #[test]
    fn test_i2c_reserve_twice_different_endpoint() {
        unsafe {
            i2c_init(0, |_ioctl| Err(DriverError::Unsupported));
            assert!(i2c_reserve(100, 0x50, b"drv1").is_ok());
            assert!(i2c_reserve(200, 0x50, b"drv2").is_err());
        }
    }

    #[test]
    fn test_i2c_check_reservation_valid() {
        unsafe {
            i2c_init(1, |_ioctl| Err(DriverError::Unsupported));
            assert!(i2c_reserve(42, 0x51, b"test").is_ok());
            assert!(i2c_check_reservation(42, 0x51).is_ok());
        }
    }

    #[test]
    fn test_i2c_check_reservation_wrong_endpoint() {
        unsafe {
            i2c_init(0, |_ioctl| Err(DriverError::Unsupported));
            assert!(i2c_reserve(42, 0x51, b"test").is_ok());
            assert!(i2c_check_reservation(99, 0x51).is_err());
        }
    }

    #[test]
    fn test_i2c_release() {
        unsafe {
            i2c_init(0, |_ioctl| Err(DriverError::Unsupported));
            assert!(i2c_reserve(42, 0x52, b"test").is_ok());
            i2c_release(42);
            let devs = I2C_DEVICES.get();
            let dev = &(*devs)[0x52];
            assert!(!dev.inuse);
        }
    }

    #[test]
    fn test_i2c_exec_no_process() {
        unsafe {
            i2c_init(0, |_ioctl| Err(DriverError::Unsupported));
            let mut ioctl = I2cExec::new();
            assert!(i2c_exec(&mut ioctl).is_err());
        }
    }

    #[test]
    fn test_i2c_exec_custom_process() {
        unsafe {
            i2c_init(0, |_ioctl| Ok(()));
            let mut ioctl = I2cExec::new();
            assert!(i2c_exec(&mut ioctl).is_ok());

            i2c_init(0, |_ioctl| Err(DriverError::Io));
            assert!(i2c_exec(&mut ioctl).is_err());
        }
    }

    #[test]
    fn test_i2c_device_default() {
        let d = I2cDevice::new();
        assert!(!d.inuse);
        assert_eq!(d.endpt, -1);
    }

    #[test]
    fn test_i2c_bus_id() {
        unsafe {
            i2c_init(3, |_ioctl| Err(DriverError::Unsupported));
            assert_eq!(i2c_bus_id(), 3);
        }
    }

    #[test]
    fn test_build_key() {
        let mut buf = [0u8; DS_MAX_KEYLEN];
        let len = build_key(0, b"test", &mut buf);
        let key = core::str::from_utf8(&buf[..len]).unwrap_or("");
        assert_eq!(key, "drv.i2c.1.test");

        let len = build_key(2, b"my_driver", &mut buf);
        let key = core::str::from_utf8(&buf[..len]).unwrap_or("");
        assert_eq!(key, "drv.i2c.3.my_driver");
    }

    #[test]
    fn test_nr_i2c_dev() {
        assert_eq!(NR_I2C_DEV, 1024);
    }

    #[test]
    fn test_i2c_device_send() {
        fn assert_send<T: Send>() {}
        assert_send::<I2cDevice>();
    }
}
