//! System server crates.

#![no_std]

pub mod clock_server;
pub mod pm;
pub mod vm;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
