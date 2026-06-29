//! System server crates.

#![no_std]

pub mod clock_server;
pub mod ds;
pub mod pm;
pub mod vfs;
pub mod vm;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
