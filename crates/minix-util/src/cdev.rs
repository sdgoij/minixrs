//! Character device (CDEV) client — open, close, read, write, ioctl, select.
//!
//! Thin wrappers over IPC `sendrec` to a character device driver.
//! All functions return `Err(MinixErr(71))` on host.

#![allow(dead_code)]

use minix_std::MinixErr;

type Message = [u8; 64];

// ── CDEV message types (from arch_common::com) ────────────────────────

const CDEV_RQ_BASE: u32 = 0x400;
const CDEV_OPEN: u32 = CDEV_RQ_BASE;
const CDEV_CLOSE: u32 = CDEV_RQ_BASE + 1;
const CDEV_READ: u32 = CDEV_RQ_BASE + 2;
const CDEV_WRITE: u32 = CDEV_RQ_BASE + 3;
const CDEV_IOCTL: u32 = CDEV_RQ_BASE + 4;
const CDEV_CANCEL: u32 = CDEV_RQ_BASE + 5;
const CDEV_SELECT: u32 = CDEV_RQ_BASE + 6;

const CDEV_REPLY: u32 = 0x480;
const CDEV_SEL1_REPLY: u32 = 0x481;
const CDEV_SEL2_REPLY: u32 = 0x482;

const CDEV_R_BIT: u32 = 0x01;
const CDEV_W_BIT: u32 = 0x02;

const CDEV_NOFLAGS: u32 = 0x00;
const CDEV_NONBLOCK: u32 = 0x01;

// ── Message field offsets ─────────────────────────────────────────────
// Character devices use the standard driver message layout:
//   m_type    = offset 0  (i32) — request/reply type
//   m2_i1     = offset 8  (i32) — minor device number
//   m2_i2     = offset 12 (i32) — flags
//   m2_l1     = offset 16 (i64) — grant ID / position
//   m2_l2     = offset 24 (i64) — byte count
//   m2_p1     = offset 32 (i64) — user buffer pointer

const OFF_TYPE: usize = 0; // i32: message type
const OFF_MINOR: usize = 8; // i32: minor device number
const OFF_FLAGS: usize = 12; // i32: flags
const OFF_GRANT: usize = 16; // i64: grant ID for data transfer
const OFF_COUNT: usize = 24; // i64: byte count / returned status
const OFF_ADDR: usize = 32; // i64: user buffer / position

// ═══════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════

fn msg_set_i32(msg: &mut Message, off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

fn msg_set_i64(msg: &mut Message, off: usize, val: i64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

fn msg_get_i32(msg: &Message, off: usize) -> i32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&msg[off..off + 4]);
    i32::from_ne_bytes(bytes)
}

fn build_msg(typ: u32) -> Message {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_TYPE, typ as i32);
    msg
}

fn check_result(msg: &Message) -> Result<i32, MinixErr> {
    let mtype = msg_get_i32(msg, OFF_TYPE);
    if mtype < 0 {
        Err(MinixErr(-mtype))
    } else {
        Ok(msg_get_i32(msg, OFF_COUNT))
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════

/// Open a character device.
pub fn cdev_open(endpoint: i32, minor: i32, flags: u32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(CDEV_OPEN);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        msg_set_i32(&mut msg, OFF_FLAGS, flags as i32);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor, flags);
        Err(MinixErr(71))
    }
}

/// Close a character device.
pub fn cdev_close(endpoint: i32, minor: i32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(CDEV_CLOSE);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor);
        Err(MinixErr(71))
    }
}

/// Read from a character device.
///
/// Returns the number of bytes read on success.
pub fn cdev_read(endpoint: i32, minor: i32, count: usize, _grant: u32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(CDEV_READ);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        msg_set_i32(&mut msg, OFF_FLAGS, CDEV_R_BIT as i32);
        msg_set_i64(&mut msg, OFF_GRANT, _grant as i64);
        msg_set_i64(&mut msg, OFF_COUNT, count as i64);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor, count, _grant);
        Err(MinixErr(71))
    }
}

/// Write to a character device.
///
/// Returns the number of bytes written on success.
pub fn cdev_write(endpoint: i32, minor: i32, count: usize, _grant: u32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(CDEV_WRITE);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        msg_set_i32(&mut msg, OFF_FLAGS, CDEV_W_BIT as i32);
        msg_set_i64(&mut msg, OFF_GRANT, _grant as i64);
        msg_set_i64(&mut msg, OFF_COUNT, count as i64);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor, count, _grant);
        Err(MinixErr(71))
    }
}

/// Perform a device ioctl.
pub fn cdev_ioctl(endpoint: i32, minor: i32, request: u32, _grant: u32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(CDEV_IOCTL);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        msg_set_i32(&mut msg, OFF_FLAGS, request as i32);
        msg_set_i64(&mut msg, OFF_GRANT, _grant as i64);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor, request, _grant);
        Err(MinixErr(71))
    }
}

/// Cancel an outstanding I/O request.
pub fn cdev_cancel(endpoint: i32, minor: i32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(CDEV_CANCEL);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor);
        Err(MinixErr(71))
    }
}

/// Check if a character device is ready for I/O.
pub fn cdev_select(endpoint: i32, minor: i32, ops: u32) -> Result<u32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(CDEV_SELECT);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        msg_set_i32(&mut msg, OFF_FLAGS, ops as i32);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        let mtype = msg_get_i32(&msg, OFF_TYPE);
        if mtype < 0 {
            Err(MinixErr(-mtype))
        } else {
            Ok(mtype as u32)
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor, ops);
        Err(MinixErr(71))
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdev_constants() {
        assert_eq!(CDEV_RQ_BASE, 0x400);
        assert_eq!(CDEV_OPEN, 0x400);
        assert_eq!(CDEV_CLOSE, 0x401);
        assert_eq!(CDEV_READ, 0x402);
        assert_eq!(CDEV_WRITE, 0x403);
        assert_eq!(CDEV_IOCTL, 0x404);
        assert_eq!(CDEV_CANCEL, 0x405);
        assert_eq!(CDEV_SELECT, 0x406);
        assert_eq!(CDEV_REPLY, 0x480);
        assert_eq!(CDEV_SEL1_REPLY, 0x481);
        assert_eq!(CDEV_SEL2_REPLY, 0x482);
    }

    #[test]
    fn test_cdev_open_returns_enosys_on_host() {
        let r = cdev_open(10, 0, CDEV_NOFLAGS);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().0, 71);
    }

    #[test]
    fn test_cdev_close_returns_enosys_on_host() {
        let r = cdev_close(10, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_cdev_read_returns_enosys_on_host() {
        let r = cdev_read(10, 0, 64, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_cdev_write_returns_enosys_on_host() {
        let r = cdev_write(10, 0, 64, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_cdev_ioctl_returns_enosys_on_host() {
        let r = cdev_ioctl(10, 0, 0, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_cdev_cancel_returns_enosys_on_host() {
        let r = cdev_cancel(10, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_cdev_select_returns_enosys_on_host() {
        let r = cdev_select(10, 0, CDEV_R_BIT);
        assert!(r.is_err());
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 0, -42);
        assert_eq!(msg_get_i32(&msg, 0), -42);

        msg_set_i64(&mut msg, 16, 0x1234567890ABCDEF);
    }

    #[test]
    fn test_build_msg_sets_type() {
        let msg = build_msg(CDEV_OPEN);
        assert_eq!(msg_get_i32(&msg, 0), CDEV_OPEN as i32);
    }
}
