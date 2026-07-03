//! Driver framework + individual drivers.

#![no_std]

pub mod arch_io;

pub mod prelude {
    pub use core::ops::Deref;
    pub use core::ops::DerefMut;
}

pub mod bus;
pub mod clock;
pub mod eeprom;
pub mod input;
pub mod storage;
pub mod system;
pub mod tty;
pub mod video;

/// Driver error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverError {
    /// Device not found.
    NotFound,
    /// Device is busy.
    Busy,
    /// I/O error during operation.
    Io,
    /// Invalid argument.
    InvalidArgument,
    /// Operation not supported by this device.
    Unsupported,
    /// Unknown or unexpected error.
    Unknown,
}

/// Driver trait — every driver must implement this.
pub trait Driver {
    /// Initialize the driver.
    fn init(&mut self) -> Result<(), DriverError>;
    /// Shut down the driver and release resources.
    fn shutdown(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let _ = 0;
    }

    #[test]
    fn driver_error_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DriverError>();
    }
}
