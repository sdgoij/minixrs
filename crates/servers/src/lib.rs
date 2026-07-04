//! System server crates.

#![no_std]

pub mod clock_server;
pub mod devman;
pub mod ds;
pub mod ipc;
pub mod mutex;
pub mod pm;
pub mod ramdisk;
pub mod rs;
pub mod sched;
pub mod tty;
pub mod vfs;
pub mod vm;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let _ = 0;
    }
}
