//! RAM disk block driver — serves BDEV messages via IPC.
//!
//! Handles `BDEV_OPEN`, `BDEV_CLOSE`, `BDEV_READ`, `BDEV_WRITE`, `BDEV_IOCTL`
//! by dispatching to the underlying `drivers::storage::ramdisk` functions.
//!
//! Registered as endpoint `RAMDISK_PROC_NR` in the boot process table.
//! MFS (and other FS servers) send BDEV messages to this endpoint.

//! # Dead-code allowance
//!
//! All functions and constants in this module are used only by the
//! `ramdisk` binary target (`src/bin/ramdisk.rs`), not by the `servers`
//! library target.  Clippy's `dead_code` lint fires for library builds.
//! The `dead_code` allowance is intentional — the binary target does
//! use everything.
#![allow(dead_code)]

use arch_common::ipc::Message;
use drivers::storage::ramdisk;

/// BDEV message types (from arch_common::com).
const BDEV_RQ_BASE: u32 = 0x500;
const BDEV_OPEN: u32 = BDEV_RQ_BASE;
const BDEV_CLOSE: u32 = BDEV_RQ_BASE + 1;
const BDEV_READ: u32 = BDEV_RQ_BASE + 2;
const BDEV_WRITE: u32 = BDEV_RQ_BASE + 3;
const BDEV_GATHER: u32 = BDEV_RQ_BASE + 4;
const BDEV_SCATTER: u32 = BDEV_RQ_BASE + 5;
const BDEV_IOCTL: u32 = BDEV_RQ_BASE + 6;

const BDEV_REPLY: u32 = 0x580;

/// Byte offsets in the message (match `minix-util/src/bdev.rs`).
const OFF_MINOR: usize = 8; // i32
const OFF_FLAGS: usize = 12; // i32
const OFF_GRANT: usize = 16; // i64 (unused here)
const OFF_COUNT: usize = 24; // i64
const OFF_ADDR: usize = 32; // i64 (position)

fn msg_get_i32(msg: &Message, off: usize) -> i32 {
    unsafe {
        let bytes = &msg.m_payload.raw[off - 8..][..4];
        i32::from_ne_bytes(bytes.try_into().unwrap())
    }
}

fn msg_get_i64(msg: &Message, off: usize) -> i64 {
    unsafe {
        let bytes = &msg.m_payload.raw[off - 8..][..8];
        i64::from_ne_bytes(bytes.try_into().unwrap())
    }
}

fn msg_set_i32(msg: &mut Message, off: usize, val: i32) {
    unsafe {
        let dst = &mut msg.m_payload.raw[off - 8..][..4];
        dst.copy_from_slice(&val.to_ne_bytes());
    }
}

fn msg_set_i64(msg: &mut Message, off: usize, val: i64) {
    unsafe {
        let dst = &mut msg.m_payload.raw[off - 8..][..8];
        dst.copy_from_slice(&val.to_ne_bytes());
    }
}

/// Build a BDEV reply message with a status code.
fn build_reply(msg: &mut Message, status: i32) {
    msg.m_type = BDEV_REPLY as i32;
    msg_set_i32(msg, OFF_COUNT, status);
}

/// Handle a single BDEV message and write the reply.
fn handle_bdev(msg: &mut Message, _ep: i32) {
    let mtype = msg.m_type as u32;
    let minor = msg_get_i32(msg, OFF_MINOR) as usize;
    let _flags = msg_get_i32(msg, OFF_FLAGS) as u32;

    match mtype {
        BDEV_OPEN => {
            match ramdisk::ramdisk_open(minor) {
                Ok(()) => build_reply(msg, 0),
                Err(_) => build_reply(msg, -5), // EIO
            }
        }
        BDEV_CLOSE => match ramdisk::ramdisk_close(minor) {
            Ok(()) => build_reply(msg, 0),
            Err(_) => build_reply(msg, -5),
        },
        BDEV_READ => {
            let position = msg_get_i64(msg, OFF_ADDR) as u64;
            let count = msg_get_i64(msg, OFF_COUNT) as usize;
            let mut buf = [0u8; 4096];
            let n = count.min(buf.len());
            match unsafe { ramdisk::ramdisk_read(minor, position, &mut buf[..n]) } {
                Ok(bytes) => build_reply(msg, bytes as i32),
                Err(_) => build_reply(msg, -5),
            }
        }
        BDEV_WRITE => {
            let position = msg_get_i64(msg, OFF_ADDR) as u64;
            let count = msg_get_i64(msg, OFF_COUNT) as usize;
            let buf = [0u8; 4096];
            let n = count.min(buf.len());
            match unsafe { ramdisk::ramdisk_write(minor, position, &buf[..n]) } {
                Ok(bytes) => build_reply(msg, bytes as i32),
                Err(_) => build_reply(msg, -5),
            }
        }
        BDEV_GATHER | BDEV_SCATTER => {
            build_reply(msg, -95); // ENOTSUP
        }
        BDEV_IOCTL => {
            build_reply(msg, -95); // ENOTSUP
        }
        _ => {
            build_reply(msg, -22); // EINVAL
        }
    }
}

/// Main entry point for the RAM disk driver process.
///
/// Initializes the RAM disk, then enters the message loop:
/// receive a BDEV message → dispatch → reply.
pub fn ramdisk_server_main() {
    #[cfg(target_os = "none")]
    {
        const RECEIVE_CALL: u64 = 47;
        const SENDREC_CALL: u64 = 48;
        const ANY: i32 = 0x0000ffff;

        // Initialize the RAM disk hardware.
        unsafe {
            ramdisk::ramdisk_init();
        }

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            // Receive a message from any sender.
            let src = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64)
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            // Handle the BDEV message.
            handle_bdev(&mut msg, src_ep);

            // Send the reply.
            let _ = unsafe {
                minix_rt::syscall2(SENDREC_CALL, src_ep as u64, &mut msg as *mut Message as u64)
            };
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        panic!("ramdisk_server_main called on host");
    }
}
