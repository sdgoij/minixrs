//! Data Store (DS) client — publish, retrieve, subscribe, and delete keys.
//!
//! Thin wrappers over IPC `sendrec` to the DS server (`DS_PROC_NR` = 6).
//! All functions return `Err(MinixErr((71,)))` on host (`cfg(not(target_os = "none"))`).

#![allow(dead_code)]

use minix_std::MinixErr;

type Message = [u8; 64];

// ── DS message types (from arch_common::com) ───────────────────────────

const DS_RQ_BASE: u32 = 0x800;
const DS_PUBLISH: u32 = DS_RQ_BASE;
const DS_RETRIEVE: u32 = DS_RQ_BASE + 1;
const DS_SUBSCRIBE: u32 = DS_RQ_BASE + 2;
const DS_CHECK: u32 = DS_RQ_BASE + 3;
const DS_DELETE: u32 = DS_RQ_BASE + 4;

// ── Message field offsets ─────────────────────────────────────────────

const OFF_TYPE: usize = 0; // i32: message type (DS_PUBLISH, etc.)
const OFF_M2_I1: usize = 8; // i32: key length
const OFF_M2_I2: usize = 12; // i32: value (u32) or endpoint for labels

// ── DS endpoint ───────────────────────────────────────────────────────

const DS_ENDPOINT: i32 = 6; // DS_PROC_NR

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

/// Check the message type field for a negative error code.
/// Returns `Ok(())` if non-negative, `Err(MinixErr)` if negative.
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

/// Publish an unsigned 32-bit value under `key`.
pub fn ds_publish_u32(key: &[u8], value: u32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DS_PUBLISH);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I2, value as i32);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (key, value);
        Err(MinixErr(71))
    }
}

/// Retrieve the unsigned 32-bit value at `key`.
pub fn ds_retrieve_u32(key: &[u8]) -> Result<u32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DS_RETRIEVE);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)?;
        Ok(msg_get_i32(&msg, OFF_M2_I2) as u32)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = key;
        Err(MinixErr(71))
    }
}

/// Publish a label (endpoint mapping) under `key`.
pub fn ds_publish_label(key: &[u8], endpoint: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DS_PUBLISH);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I2, endpoint);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (key, endpoint);
        Err(MinixErr(71))
    }
}

/// Retrieve the endpoint (label) at `key`.
pub fn ds_retrieve_label(key: &[u8]) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DS_RETRIEVE);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)?;
        Ok(msg_get_i32(&msg, OFF_M2_I2))
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = key;
        Err(MinixErr(71))
    }
}

/// Subscribe to keys matching `pattern`.
pub fn ds_subscribe(pattern: &[u8], overwrite: bool) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DS_SUBSCRIBE);
        msg_set_i32(&mut msg, OFF_M2_I1, pattern.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I2, overwrite as i32);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (pattern, overwrite);
        Err(MinixErr(71))
    }
}

/// Delete a key from the store.
pub fn ds_delete(key: &[u8]) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    {
        let mut msg = build_msg(DS_DELETE);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = key;
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
    fn test_ds_constants() {
        assert_eq!(DS_RQ_BASE, 0x800);
        assert_eq!(DS_PUBLISH, 0x800);
        assert_eq!(DS_RETRIEVE, 0x801);
        assert_eq!(DS_SUBSCRIBE, 0x802);
        assert_eq!(DS_DELETE, 0x804);
    }

    #[test]
    fn test_ds_publish_u32_returns_enosys_on_host() {
        let r = ds_publish_u32(b"test.key", 42);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().0, 71);
    }

    #[test]
    fn test_ds_retrieve_u32_returns_enosys_on_host() {
        let r = ds_retrieve_u32(b"test.key");
        assert!(r.is_err());
    }

    #[test]
    fn test_ds_publish_label_returns_enosys_on_host() {
        let r = ds_publish_label(b"process.test", 17);
        assert!(r.is_err());
    }

    #[test]
    fn test_ds_retrieve_label_returns_enosys_on_host() {
        let r = ds_retrieve_label(b"process.test");
        assert!(r.is_err());
    }

    #[test]
    fn test_ds_subscribe_returns_enosys_on_host() {
        let r = ds_subscribe(b"test.*", false);
        assert!(r.is_err());
    }

    #[test]
    fn test_ds_delete_returns_enosys_on_host() {
        let r = ds_delete(b"test.key");
        assert!(r.is_err());
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 0, -42);
        assert_eq!(msg_get_i32(&msg, 0), -42);

        msg_set_i32(&mut msg, 8, 0x12345678);
        assert_eq!(msg_get_i32(&msg, 8), 0x12345678);
    }

    #[test]
    fn test_build_msg_sets_type() {
        let msg = build_msg(DS_PUBLISH);
        assert_eq!(msg_get_i32(&msg, 0), DS_PUBLISH as i32);
    }

    #[test]
    fn test_check_result() {
        let mut msg_ok = [0u8; 64];
        msg_set_i32(&mut msg_ok, 0, 0);
        assert!(check_result(&msg_ok).is_ok());

        let mut msg_ok2 = [0u8; 64];
        msg_set_i32(&mut msg_ok2, 0, 42);
        assert!(check_result(&msg_ok2).is_ok());

        let mut msg_err = [0u8; 64];
        msg_set_i32(&mut msg_err, 0, -71);
        assert_eq!(check_result(&msg_err), Err(MinixErr(71)));
    }
}
