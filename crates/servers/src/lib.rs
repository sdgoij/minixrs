//! System server crates.

#![no_std]

pub mod clock_server;
// The console's screen model. Compiled where it has a display to feed (the wasm port's `wserver`)
// and for the host tests, which cover the model itself.
#[cfg(any(target_arch = "wasm32", test))]
pub mod console;
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
pub mod wserver;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let _ = 0;
    }
}
