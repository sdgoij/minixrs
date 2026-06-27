//! Driver framework + individual drivers.

#![no_std]

pub mod prelude {
    pub use core::ops::Deref;
    pub use core::ops::DerefMut;
}

/// Driver trait — every driver must implement this.
pub trait Driver {
    fn init(&mut self) -> Result<(), ()>;
    fn shutdown(&mut self);
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
