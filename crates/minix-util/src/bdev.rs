//! Block device (BDEV) client — open, close, read, write, ioctl.
//!
//! Thin wrappers over IPC `sendrec` to a block device driver.
//! All functions return `Err(MinixErr(71))` on host.

#![allow(dead_code)]

use minix_std::MinixErr;

type Message = [u8; 64];

// ── BDEV message types (from arch_common::com) ────────────────────────

const BDEV_RQ_BASE: u32 = 0x500;
const BDEV_OPEN: u32 = BDEV_RQ_BASE;
const BDEV_CLOSE: u32 = BDEV_RQ_BASE + 1;
const BDEV_READ: u32 = BDEV_RQ_BASE + 2;
const BDEV_WRITE: u32 = BDEV_RQ_BASE + 3;
const BDEV_GATHER: u32 = BDEV_RQ_BASE + 4;
const BDEV_SCATTER: u32 = BDEV_RQ_BASE + 5;
const BDEV_IOCTL: u32 = BDEV_RQ_BASE + 6;

const BDEV_REPLY: u32 = 0x580;

const BDEV_R_BIT: u32 = 0x01;
const BDEV_W_BIT: u32 = 0x02;
const BDEV_NOFLAGS: u32 = 0x00;

// ── Message field offsets (MINIX driver message format) ───────────────
// Standard driver message layout:
//   m_type    = offset 0  (i32) — request/reply type
//   m2_i1     = offset 8  (i32) — minor device number
//   m2_i2     = offset 12 (i32) — flags / command
//   m2_l1     = offset 16 (i64) — grant ID (low 32) / byte offset
//   m2_l2     = offset 24 (i64) — byte count / status
//   m2_p1     = offset 32 (i64) — buffer pointer / grant

const OFF_TYPE: usize = 0; // i32: message type
const OFF_MINOR: usize = 8; // i32: minor device number
const OFF_FLAGS: usize = 12; // i32: flags (R_BIT, W_BIT)
const OFF_GRANT: usize = 16; // i64: grant ID for data transfer
const OFF_COUNT: usize = 24; // i64: byte count
const OFF_ADDR: usize = 32; // i64: buffer address / position

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

fn msg_get_i64(msg: &Message, off: usize) -> i64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&msg[off..off + 8]);
    i64::from_ne_bytes(bytes)
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
        // Success: return the status field (usually bytes transferred).
        Ok(msg_get_i32(msg, OFF_COUNT))
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════

/// Open a block device.
///
/// `endpoint` is the block driver's endpoint (e.g., `AT_WINI`).
pub fn bdev_open(endpoint: i32, minor: i32, flags: u32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(BDEV_OPEN);
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

/// Close a block device.
pub fn bdev_close(endpoint: i32, minor: i32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(BDEV_CLOSE);
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

/// Read from a block device.
///
/// Returns the number of bytes read.
pub fn bdev_read(endpoint: i32, minor: i32, pos: i64, count: usize) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(BDEV_READ);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        msg_set_i32(&mut msg, OFF_FLAGS, BDEV_R_BIT as i32);
        msg_set_i64(&mut msg, OFF_ADDR, pos);
        msg_set_i64(&mut msg, OFF_COUNT, count as i64);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        // On success, status field holds bytes transferred.
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor, pos, count);
        Err(MinixErr(71))
    }
}

/// Write to a block device.
///
/// Returns the number of bytes written.
pub fn bdev_write(endpoint: i32, minor: i32, pos: i64, count: usize) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(BDEV_WRITE);
        msg_set_i32(&mut msg, OFF_MINOR, minor);
        msg_set_i32(&mut msg, OFF_FLAGS, BDEV_W_BIT as i32);
        msg_set_i64(&mut msg, OFF_ADDR, pos);
        msg_set_i64(&mut msg, OFF_COUNT, count as i64);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, minor, pos, count);
        Err(MinixErr(71))
    }
}

/// Perform a device ioctl.
pub fn bdev_ioctl(endpoint: i32, minor: i32, request: u32, _grant: u32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(BDEV_IOCTL);
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

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bdev_constants() {
        assert_eq!(BDEV_RQ_BASE, 0x500);
        assert_eq!(BDEV_OPEN, 0x500);
        assert_eq!(BDEV_CLOSE, 0x501);
        assert_eq!(BDEV_READ, 0x502);
        assert_eq!(BDEV_WRITE, 0x503);
        assert_eq!(BDEV_IOCTL, 0x506);
        assert_eq!(BDEV_REPLY, 0x580);
    }

    #[test]
    fn test_bdev_open_returns_enosys_on_host() {
        let r = bdev_open(10, 0, BDEV_NOFLAGS);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().0, 71);
    }

    #[test]
    fn test_bdev_close_returns_enosys_on_host() {
        let r = bdev_close(10, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_bdev_read_returns_enosys_on_host() {
        let r = bdev_read(10, 0, 0, 512);
        assert!(r.is_err());
    }

    #[test]
    fn test_bdev_write_returns_enosys_on_host() {
        let r = bdev_write(10, 0, 0, 512);
        assert!(r.is_err());
    }

    #[test]
    fn test_bdev_ioctl_returns_enosys_on_host() {
        let r = bdev_ioctl(10, 0, 0, 0);
        assert!(r.is_err());
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 0, -42);
        assert_eq!(msg_get_i32(&msg, 0), -42);

        msg_set_i64(&mut msg, 16, 0x1234567890ABCDEF);
        assert_eq!(msg_get_i64(&msg, 16), 0x1234567890ABCDEF);
    }

    #[test]
    fn test_build_msg_sets_type() {
        let msg = build_msg(BDEV_OPEN);
        assert_eq!(msg_get_i32(&msg, 0), BDEV_OPEN as i32);
    }

    #[test]
    fn test_check_result_positive() {
        let mut msg_ok = [0u8; 64];
        msg_set_i32(&mut msg_ok, 0, 0);
        msg_set_i32(&mut msg_ok, OFF_COUNT, 512);
        let r = check_result(&msg_ok);
        assert!(r.is_ok());
        assert_eq!(r.unwrap(), 512);

        let mut msg_err = [0u8; 64];
        msg_set_i32(&mut msg_err, 0, -5);
        assert_eq!(check_result(&msg_err), Err(MinixErr(5)));
    }
}
