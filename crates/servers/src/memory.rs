//! Memory device server — `/dev/null` and `/dev/zero`.
//!
//! A CDEV server over `drivers::storage::memory`, which is the port of C's `memory` driver.
//! It answers `CDEV_OPEN`, `CDEV_CLOSE`, `CDEV_READ`, `CDEV_WRITE`, `CDEV_SELECT` and
//! `CDEV_IOCTL` from VFS's synchronous inline protocol: a read's bytes go at payload[0] and
//! the reply's `m_type` is the byte count, and a write's chunk arrives at [`OFF_BUF`] with
//! its length in `m2_l2`.
//!
//! `/dev/mem` and `/dev/kmem` are *not* served. C's driver maps physical pages with
//! `vm_map_phys`; the port has no such call, and `memory::mem_read`'s raw cast is only
//! meaningful where the driver's virtual address equals the physical one. Opening those
//! minors here would hand a user process a wild read, so they answer `ENXIO` until the
//! mapping exists.
//!
//! Registered as `MEM_PROC_NR` (3) in the boot process table, with VFS mapping major 1
//! (`MEMORY_MAJOR`) to it.

// The handler and its message offsets are reached only from the `memory` binary target and the
// tests: the `servers` library build has no minix receive loop to call them, so `dead_code` is
// expected here. Same allowance as `ramdisk`.
#![allow(dead_code)]

use arch_common::com::{CDEV_NOTIFY, CDEV_OP_RD, CDEV_OP_WR};
use arch_common::ipc::Message;
use drivers::storage::memory;

/// CDEV message types (from `<minix/com.h>`).
const CDEV_OPEN: i32 = 0x400;
const CDEV_CLOSE: i32 = 0x401;
const CDEV_READ: i32 = 0x402;
const CDEV_WRITE: i32 = 0x403;
const CDEV_IOCTL: i32 = 0x404;
const CDEV_SELECT: i32 = 0x406;

/// Byte offsets in the request (match `vfs/device.rs`'s `CDEV_*_OFF`).
const OFF_MINOR: usize = 8; // m2_i1
const OFF_FLAGS: usize = 12; // m2_i2
const OFF_POS: usize = 24; // m2_l1
const OFF_COUNT: usize = 32; // m2_l2
const OFF_BUF: usize = 40; // m2_l3, the write chunk

/// The most a synchronous request carries: VFS caps a read reply at 48 bytes and
/// copies 8 bytes per write round trip (`INLINE_READ_MAX`/`INLINE_WRITE_MAX`).
const INLINE_READ_MAX: usize = 48;
const INLINE_WRITE_MAX: usize = 8;

const ENXIO: i32 = -6;
const EINVAL: i32 = -22;

fn raw(msg: &Message) -> &[u8; 56] {
    unsafe { &msg.m_payload.raw }
}

fn raw_mut(msg: &mut Message) -> &mut [u8; 56] {
    unsafe { &mut msg.m_payload.raw }
}

fn msg_i32(msg: &Message, off: usize) -> i32 {
    let bytes = &raw(msg)[off - 8..][..4];
    i32::from_ne_bytes(bytes.try_into().unwrap_or([0; 4]))
}

fn msg_u64(msg: &Message, off: usize) -> u64 {
    let bytes = &raw(msg)[off - 8..][..8];
    u64::from_ne_bytes(bytes.try_into().unwrap_or([0; 8]))
}

/// Whether this server serves `minor` at all. Keeping the decision in one place is what
/// keeps `/dev/mem` out of reach: `memory::mem_open` accepts it, and its read path casts the
/// offset straight to a pointer.
fn serves(minor: usize) -> bool {
    matches!(minor, memory::NULL_DEV | memory::ZERO_DEV)
}

/// Handle one CDEV request and return the status VFS reads as the reply.
fn handle_cdev(msg: &mut Message, mtype: i32) -> i32 {
    let minor = msg_i32(msg, OFF_MINOR) as usize;

    match mtype {
        CDEV_OPEN => {
            if !serves(minor) {
                return ENXIO;
            }
            match memory::mem_open(minor, msg_i32(msg, OFF_FLAGS)) {
                Ok(()) => 0,
                Err(_) => ENXIO,
            }
        }
        CDEV_CLOSE => {
            if !serves(minor) {
                return ENXIO;
            }
            match memory::mem_close(minor) {
                Ok(()) => 0,
                Err(_) => ENXIO,
            }
        }
        CDEV_READ => {
            let want = (msg_u64(msg, OFF_COUNT) as usize).min(INLINE_READ_MAX);
            let mut buf = [0u8; INLINE_READ_MAX];
            match memory::mem_read(minor, msg_u64(msg, OFF_POS), &mut buf[..want]) {
                Ok(n) => {
                    raw_mut(msg)[..n].copy_from_slice(&buf[..n]);
                    n as i32
                }
                Err(_) => ENXIO,
            }
        }
        CDEV_WRITE => {
            let len = (msg_u64(msg, OFF_COUNT) as usize).min(INLINE_WRITE_MAX);
            let start = OFF_BUF - 8;
            let data = &raw(msg)[start..start + len];
            match memory::mem_write(minor, msg_u64(msg, OFF_POS), data) {
                Ok(n) => n as i32,
                Err(_) => ENXIO,
            }
        }
        // Both devices are always ready in both directions: a read of `/dev/null` ends at
        // once and a read of `/dev/zero` has bytes waiting, and either write is taken
        // immediately.
        CDEV_SELECT => {
            let ops = msg_i32(msg, OFF_FLAGS) as u32 & !CDEV_NOTIFY;
            (ops & (CDEV_OP_RD | CDEV_OP_WR)) as i32
        }
        // No requests: a termios query on `/dev/null` is not a terminal, and VFS answers an
        // unrouted ioctl with success, so a `0` here would be a lie.
        CDEV_IOCTL => EINVAL,
        _ => EINVAL,
    }
}

/// Main entry point for the memory driver process.
pub fn memory_server_main() {
    #[cfg(target_os = "minix")]
    {
        const RECEIVE_CALL: u64 = 47;
        const SEND_CALL: u64 = 46;
        const ANY: i32 = 0x0000ffff;

        unsafe { memory::mem_init() };

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            let src = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64)
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;

            // RS asks each service it starts to initialise; the shape is shared so the
            // services cannot disagree about it.
            if minix_util::rs::answer_rs_init(msg.m_type, src_ep) {
                continue;
            }

            let mtype = msg.m_type;
            msg.m_type = handle_cdev(&mut msg, mtype);

            let _ = unsafe {
                minix_rt::syscall2(SEND_CALL, src_ep as u64, &mut msg as *mut Message as u64)
            };
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // Host builds dispatch the handler directly in the tests below.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(minor: usize, pos: u64, count: u64) -> Message {
        let mut msg = Message {
            m_source: 0,
            m_type: 0,
            m_payload: unsafe { core::mem::zeroed() },
        };
        let r = raw_mut(&mut msg);
        r[OFF_MINOR - 8..OFF_MINOR - 8 + 4].copy_from_slice(&(minor as i32).to_ne_bytes());
        r[OFF_POS - 8..OFF_POS - 8 + 8].copy_from_slice(&pos.to_ne_bytes());
        r[OFF_COUNT - 8..OFF_COUNT - 8 + 8].copy_from_slice(&count.to_ne_bytes());
        msg
    }

    #[test]
    fn null_reads_as_end_of_file() {
        let mut msg = request(memory::NULL_DEV, 0, 48);
        assert_eq!(handle_cdev(&mut msg, CDEV_READ), 0);
    }

    #[test]
    fn zero_reads_as_zero_bytes() {
        let mut msg = request(memory::ZERO_DEV, 0, 48);
        assert_eq!(handle_cdev(&mut msg, CDEV_READ), 48);
        assert!(raw(&msg)[..48].iter().all(|&b| b == 0));
    }

    #[test]
    fn writes_are_taken_and_discarded() {
        for minor in [memory::NULL_DEV, memory::ZERO_DEV] {
            let mut msg = request(minor, 0, 8);
            raw_mut(&mut msg)[OFF_BUF - 8..OFF_BUF - 8 + 8].copy_from_slice(b"01234567");
            assert_eq!(handle_cdev(&mut msg, CDEV_WRITE), 8);
        }
    }

    #[test]
    fn open_and_close_are_counted() {
        unsafe { memory::mem_init() };
        let mut msg = request(memory::NULL_DEV, 0, 0);
        assert_eq!(handle_cdev(&mut msg, CDEV_OPEN), 0);
        assert_eq!(memory::mem_open_count(memory::NULL_DEV), 1);
        let mut msg = request(memory::NULL_DEV, 0, 0);
        assert_eq!(handle_cdev(&mut msg, CDEV_CLOSE), 0);
        assert_eq!(memory::mem_open_count(memory::NULL_DEV), 0);
    }

    #[test]
    fn mem_and_kmem_are_refused() {
        // Their read path casts the offset to a pointer, and the port has no `vm_map_phys`.
        for minor in [memory::MEM_DEV, memory::KMEM_DEV] {
            let mut msg = request(minor, 0, 0);
            assert_eq!(handle_cdev(&mut msg, CDEV_OPEN), ENXIO);
        }
    }

    #[test]
    fn both_devices_are_always_ready() {
        let mut msg = request(memory::NULL_DEV, 0, 0);
        let ops = (CDEV_OP_RD | CDEV_OP_WR | CDEV_NOTIFY) as i32;
        raw_mut(&mut msg)[OFF_FLAGS - 8..OFF_FLAGS - 8 + 4].copy_from_slice(&ops.to_ne_bytes());
        assert_eq!(
            handle_cdev(&mut msg, CDEV_SELECT),
            (CDEV_OP_RD | CDEV_OP_WR) as i32
        );
    }

    #[test]
    fn ioctl_is_refused() {
        let mut msg = request(memory::NULL_DEV, 0, 0);
        assert_eq!(handle_cdev(&mut msg, CDEV_IOCTL), EINVAL);
    }
}
