//! Device Manager (DEVMAN) client — device tree operations.
//!
//! Thin wrappers over IPC `sendrec` to the DEVMAN server.
//! DEVMAN endpoint is typically assigned dynamically; the default is 9 (PFS_PROC_NR).
//! All functions return `Err(MinixErr(71))` on host.

#![allow(dead_code)]

use minix_std::MinixErr;

type Message = [u8; 64];

// ── DEVMAN message types (from arch_common::com) ──────────────────────

const DEVMAN_BASE: u32 = 0x1200;
const DEVMAN_ADD_DEV: u32 = DEVMAN_BASE;
const DEVMAN_DEL_DEV: u32 = DEVMAN_BASE + 1;
const DEVMAN_ADD_BUS: u32 = DEVMAN_BASE + 2;
const DEVMAN_DEL_BUS: u32 = DEVMAN_BASE + 3;
const DEVMAN_ADD_DEVFILE: u32 = DEVMAN_BASE + 4;
const DEVMAN_DEL_DEVFILE: u32 = DEVMAN_BASE + 5;
const DEVMAN_REQUEST: u32 = DEVMAN_BASE + 6;
const DEVMAN_BIND: u32 = DEVMAN_BASE + 8;
const DEVMAN_UNBIND: u32 = DEVMAN_BASE + 9;

// ── Message field offsets ─────────────────────────────────────────────
// DEVMAN uses m4_* fields:
//   m4_l1 = offset 16 (grant ID / result)
//   m4_l2 = offset 20 (grant size / device ID)
//   m4_l3 = offset 24 (endpoint)

const OFF_TYPE: usize = 0; // i32: message type
const OFF_M4_L1: usize = 16; // i32: grant ID / result
const OFF_M4_L2: usize = 20; // i32: grant size / device ID
const OFF_M4_L3: usize = 24; // i32: endpoint

// ── String/name limits ────────────────────────────────────────────────

const DEVMAN_STRING_LEN: usize = 128;

// ═══════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════

fn msg_set_i32(msg: &mut Message, off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
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

fn check_result(msg: &Message) -> Result<(), MinixErr> {
    let mtype = msg_get_i32(msg, OFF_TYPE);
    if mtype < 0 {
        Err(MinixErr(-mtype))
    } else {
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════

/// Add a device to the device tree.
pub fn devman_add_device(endpoint: i32, _name: &str) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DEVMAN_ADD_DEV);
        msg_set_i32(&mut msg, OFF_M4_L3, endpoint);
        unsafe { minix_std::sendrec(endpoint, &mut msg) }?;
        check_result(&msg)?;
        Ok(msg_get_i32(&msg, OFF_M4_L1))
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (endpoint, _name);
        Err(MinixErr(71))
    }
}

/// Remove a device from the device tree by ID.
pub fn devman_del_device(dev_id: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DEVMAN_DEL_DEV);
        msg_set_i32(&mut msg, OFF_M4_L2, dev_id);
        unsafe { minix_std::sendrec(dev_id, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = dev_id;
        Err(MinixErr(71))
    }
}

/// Add a bus to the device tree.
pub fn devman_add_bus(_name: &str) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DEVMAN_ADD_BUS);
        unsafe { minix_std::sendrec(0, &mut msg) }?;
        check_result(&msg)?;
        Ok(msg_get_i32(&msg, OFF_M4_L1))
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = _name;
        Err(MinixErr(71))
    }
}

/// Add a device file entry.
pub fn devman_add_devfile(dev_id: i32, _devfile: &str) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DEVMAN_ADD_DEVFILE);
        msg_set_i32(&mut msg, OFF_M4_L2, dev_id);
        unsafe { minix_std::sendrec(dev_id, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (dev_id, _devfile);
        Err(MinixErr(71))
    }
}

/// Bind a driver to a device.
pub fn devman_bind(dev_id: i32, _driver_endpoint: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DEVMAN_BIND);
        msg_set_i32(&mut msg, OFF_M4_L2, dev_id);
        msg_set_i32(&mut msg, OFF_M4_L3, _driver_endpoint);
        unsafe { minix_std::sendrec(dev_id, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (dev_id, _driver_endpoint);
        Err(MinixErr(71))
    }
}

/// Unbind a driver from a device.
pub fn devman_unbind(dev_id: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DEVMAN_UNBIND);
        msg_set_i32(&mut msg, OFF_M4_L2, dev_id);
        unsafe { minix_std::sendrec(dev_id, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = dev_id;
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
    fn test_devman_constants() {
        assert_eq!(DEVMAN_BASE, 0x1200);
        assert_eq!(DEVMAN_ADD_DEV, 0x1200);
        assert_eq!(DEVMAN_DEL_DEV, 0x1201);
        assert_eq!(DEVMAN_ADD_BUS, 0x1202);
        assert_eq!(DEVMAN_ADD_DEVFILE, 0x1204);
        assert_eq!(DEVMAN_BIND, 0x1208);
        assert_eq!(DEVMAN_UNBIND, 0x1209);
    }

    #[test]
    fn test_devman_add_device_returns_enosys_on_host() {
        let r = devman_add_device(42, "test_dev");
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().0, 71);
    }

    #[test]
    fn test_devman_del_device_returns_enosys_on_host() {
        let r = devman_del_device(1);
        assert!(r.is_err());
    }

    #[test]
    fn test_devman_add_bus_returns_enosys_on_host() {
        let r = devman_add_bus("pci");
        assert!(r.is_err());
    }

    #[test]
    fn test_devman_bind_returns_enosys_on_host() {
        let r = devman_bind(1, 42);
        assert!(r.is_err());
    }

    #[test]
    fn test_devman_unbind_returns_enosys_on_host() {
        let r = devman_unbind(1);
        assert!(r.is_err());
    }

    #[test]
    fn test_devman_add_devfile_returns_enosys_on_host() {
        let r = devman_add_devfile(1, "/dev/test");
        assert!(r.is_err());
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 0, -42);
        assert_eq!(msg_get_i32(&msg, 0), -42);
    }

    #[test]
    fn test_message_size() {
        assert_eq!(core::mem::size_of::<Message>(), 64);
    }
}
