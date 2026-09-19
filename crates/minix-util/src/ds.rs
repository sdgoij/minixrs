//! Data Store (DS) client — publish, retrieve, subscribe, delete, and
//! getsysinfo.
//!
//! Thin wrappers over IPC `sendrec` to the DS server (`DS_PROC_NR` = 6),
//! matching the wire layout the DS server's dispatch expects (m2/m1
//! payload offsets; the request code lives at message bytes 4..8, like the
//! PM/VFS clients). All functions return `Err(MinixErr(71))` on host
//! (`cfg(not(target_os = "minix"))`).

#![allow(dead_code)]

use minix_std::MinixErr;

// The wire helpers are used from the `target_os = "minix"` bodies and from the
// tests, so they are imported where they are used rather than unconditionally.
#[cfg(any(target_os = "minix", test))]
use crate::wire::{
    OFF_M2_I1, OFF_M2_I3, OFF_M2_L1, OFF_M2_L2, build_msg, check_result, msg_get_i64, msg_set_i32,
    msg_set_u64,
};
#[cfg(target_os = "minix")]
use crate::wire::{OFF_M2_I2, reply_status};

// DS request codes (arch_common::com, DS_RQ_BASE = 0x800).
const DS_RQ_BASE: u32 = 0x800;
const DS_PUBLISH: u32 = DS_RQ_BASE;
const DS_RETRIEVE: u32 = DS_RQ_BASE + 1;
const DS_SUBSCRIBE: u32 = DS_RQ_BASE + 2;
const DS_DELETE: u32 = DS_RQ_BASE + 4;
const DS_GETSYSINFO: u32 = DS_RQ_BASE + 7;

// m1 payload (getsysinfo): what@8, where@16, size@24. Kept here rather than in
// `wire` because no other client uses the m1 layout.
const OFF_GS_WHAT: usize = 8;
const OFF_GS_WHERE: usize = 16;
const OFF_GS_SIZE: usize = 24;

const DS_ENDPOINT: i32 = 6; // DS_PROC_NR

// Flag bits (mirror of servers/ds.rs DSF_*).
const DSF_OVERWRITE: u32 = 0x002;
const DSF_TYPE_U32: u32 = 0x010;
const DSF_TYPE_LABEL: u32 = 0x080;

// Public API

/// Publish an unsigned 32-bit value under `key`.
pub fn ds_publish_u32(key: &[u8], value: u32) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = build_msg(DS_PUBLISH);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I3, DSF_TYPE_U32 as i32);
        msg_set_u64(&mut msg, OFF_M2_L1, key.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_M2_L2, value as u64);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (key, value);
        Err(MinixErr(71))
    }
}

/// Retrieve the unsigned 32-bit value at `key`.
pub fn ds_retrieve_u32(key: &[u8]) -> Result<u32, MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = build_msg(DS_RETRIEVE);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I2, DSF_TYPE_U32 as i32);
        msg_set_u64(&mut msg, OFF_M2_L1, key.as_ptr() as u64);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        reply_status(&msg)?;
        Ok(msg_get_i64(&msg, OFF_M2_L1) as u32)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = key;
        Err(MinixErr(71))
    }
}

/// Publish a label (endpoint mapping) under `key`.
pub fn ds_publish_label(key: &[u8], endpoint: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = build_msg(DS_PUBLISH);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I3, DSF_TYPE_LABEL as i32);
        msg_set_u64(&mut msg, OFF_M2_L1, key.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_M2_L2, endpoint as u32 as u64);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (key, endpoint);
        Err(MinixErr(71))
    }
}

/// Retrieve the endpoint (label) at `key`.
pub fn ds_retrieve_label(key: &[u8]) -> Result<i32, MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = build_msg(DS_RETRIEVE);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I2, DSF_TYPE_LABEL as i32);
        msg_set_u64(&mut msg, OFF_M2_L1, key.as_ptr() as u64);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        reply_status(&msg)?;
        Ok(msg_get_i64(&msg, OFF_M2_L1) as i32)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = key;
        Err(MinixErr(71))
    }
}

/// Subscribe to keys matching `pattern`.
pub fn ds_subscribe(pattern: &[u8], overwrite: bool) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut flags = DSF_TYPE_U32;
        if overwrite {
            flags |= DSF_OVERWRITE;
        }
        let mut msg = build_msg(DS_SUBSCRIBE);
        msg_set_i32(&mut msg, OFF_M2_I1, pattern.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I2, flags as i32);
        msg_set_u64(&mut msg, OFF_M2_L1, pattern.as_ptr() as u64);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (pattern, overwrite);
        Err(MinixErr(71))
    }
}

/// Delete a key from the store.
pub fn ds_delete(key: &[u8]) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = build_msg(DS_DELETE);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        msg_set_u64(&mut msg, OFF_M2_L1, key.as_ptr() as u64);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = key;
        Err(MinixErr(71))
    }
}

/// Copy the whole data store into `buf` (DS_GETSYSINFO, SI_DATA_STORE).
///
/// `buf.len()` must equal `sizeof(struct data_store) * NR_DS_KEYS`
/// (192 * 64 bytes), else DS replies EINVAL.
pub fn ds_getsysinfo(buf: &mut [u8]) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = build_msg(DS_GETSYSINFO);
        msg_set_i32(&mut msg, OFF_GS_WHAT, 5); // SI_DATA_STORE (sysinfo.h)
        msg_set_u64(&mut msg, OFF_GS_WHERE, buf.as_mut_ptr() as u64);
        msg_set_u64(&mut msg, OFF_GS_SIZE, buf.len() as u64);
        unsafe { minix_std::sendrec(DS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = buf;
        Err(MinixErr(71))
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{OFF_CALL, msg_get_i32};

    #[test]
    fn test_ds_constants() {
        assert_eq!(DS_RQ_BASE, 0x800);
        assert_eq!(DS_PUBLISH, 0x800);
        assert_eq!(DS_RETRIEVE, 0x801);
        assert_eq!(DS_SUBSCRIBE, 0x802);
        assert_eq!(DS_DELETE, 0x804);
        assert_eq!(DS_GETSYSINFO, 0x807);
    }

    #[test]
    fn test_build_msg_sets_type() {
        // The request code goes at bytes 4..8 (the kernel overwrites 0..4
        // with the dest endpoint); DS reads m_type from 4..8.
        let msg = build_msg(DS_PUBLISH);
        assert_eq!(msg_get_i32(&msg, OFF_CALL), DS_PUBLISH as i32);
        assert_eq!(msg_get_i32(&msg, 0), 0);
    }

    #[test]
    fn test_publish_message_format() {
        let key = b"test.key";
        let mut msg = build_msg(DS_PUBLISH);
        msg_set_i32(&mut msg, OFF_M2_I1, key.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I3, DSF_TYPE_U32 as i32);
        msg_set_u64(&mut msg, OFF_M2_L1, key.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_M2_L2, 42);
        assert_eq!(msg_get_i32(&msg, OFF_CALL), 0x800);
        assert_eq!(msg_get_i32(&msg, OFF_M2_I1), 8);
        assert_eq!(msg_get_i32(&msg, OFF_M2_I3), DSF_TYPE_U32 as i32);
        assert_eq!(msg_get_i64(&msg, OFF_M2_L1), key.as_ptr() as i64);
        assert_eq!(msg_get_i64(&msg, OFF_M2_L2), 42);
    }

    #[test]
    fn test_getsysinfo_message_format() {
        let mut buf = [0u8; 16];
        let mut msg = build_msg(DS_GETSYSINFO);
        msg_set_i32(&mut msg, OFF_GS_WHAT, 5);
        msg_set_u64(&mut msg, OFF_GS_WHERE, buf.as_mut_ptr() as u64);
        msg_set_u64(&mut msg, OFF_GS_SIZE, buf.len() as u64);
        assert_eq!(msg_get_i32(&msg, OFF_CALL), 0x807);
        assert_eq!(msg_get_i32(&msg, OFF_GS_WHAT), 5);
        assert_eq!(msg_get_i64(&msg, OFF_GS_WHERE), buf.as_mut_ptr() as i64);
        assert_eq!(msg_get_i64(&msg, OFF_GS_SIZE), 16);
    }

    #[test]
    fn test_check_result() {
        let mut msg_ok = [0u8; 64];
        msg_set_i32(&mut msg_ok, OFF_CALL, 0);
        assert!(check_result(&msg_ok).is_ok());

        let mut msg_ok2 = [0u8; 64];
        msg_set_i32(&mut msg_ok2, OFF_CALL, 42);
        assert!(check_result(&msg_ok2).is_ok());

        let mut msg_err = [0u8; 64];
        msg_set_i32(&mut msg_err, OFF_CALL, -71);
        assert_eq!(check_result(&msg_err), Err(MinixErr(71)));
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
    fn test_ds_getsysinfo_returns_enosys_on_host() {
        let mut buf = [0u8; 16];
        let r = ds_getsysinfo(&mut buf);
        assert!(r.is_err());
    }
}
