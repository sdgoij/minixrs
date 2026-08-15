//! System server crates.

#![no_std]

pub mod clock_server;
pub mod devman;
pub mod ds;
#[cfg(target_os = "minix")]
pub mod fb;
#[cfg(target_os = "minix")]
pub mod input;
pub mod ipc;
pub mod mutex;
pub mod net;
pub mod pm;
pub mod ramdisk;
pub mod rs;
pub mod sched;
pub mod tty;
pub mod vfs;
pub mod virtio_blk;
pub mod virtio_net;
pub mod vm;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let _ = 0;
    }
}
