//! Device Manager (DEVMAN) client — device tree operations.
//!
//! Drivers register devices with the devman server (`DEVMAN_PROC_NR`) by
//! serializing a `DevmanDeviceInfo` blob, granting it to devman, and
//! sending `DEVMAN_ADD_DEV`; the reply carries the new device id. The
//! message layout matches `crates/servers/src/devman.rs` (the port's
//! `Message` is m_source @0, m_type @4, payload @8).

// The IPC entry points are minix-only (host builds exercise the pure
// helpers); keep the gated API and its constants reachable on the host.
#![allow(dead_code)]

#[cfg(target_os = "minix")]
use arch_common::com::DEVMAN_PROC_NR;
use arch_common::safecopies::{
    CPF_DIRECT, CPF_READ, CPF_USED, CPF_VALID, CpDirect, CpGrant, CpUnion, GRANT_INVALID,
};
#[cfg(target_os = "minix")]
use core::sync::atomic::Ordering;
use minix_std::MinixErr;

type Message = [u8; 64];

const DEVMAN_BASE: u32 = 0x1200;
const DEVMAN_ADD_DEV: u32 = DEVMAN_BASE;
const DEVMAN_DEL_DEV: u32 = DEVMAN_BASE + 1;
const DEVMAN_ADD_BUS: u32 = DEVMAN_BASE + 2;
const DEVMAN_DEL_BUS: u32 = DEVMAN_BASE + 3;
const DEVMAN_ADD_DEVFILE: u32 = DEVMAN_BASE + 4;
const DEVMAN_DEL_DEVFILE: u32 = DEVMAN_BASE + 5;
const DEVMAN_REQUEST: u32 = DEVMAN_BASE + 6;
const DEVMAN_REPLY: u32 = DEVMAN_BASE + 7;
const DEVMAN_BIND: u32 = DEVMAN_BASE + 8;
const DEVMAN_UNBIND: u32 = DEVMAN_BASE + 9;

// DEVMAN payload fields (m4_*), as absolute message offsets:
//   DEVMAN_GRANT_ID  = m4_l1 = offset 8  (i64)
//   DEVMAN_GRANT_SIZE = m4_l2 = offset 16 (i64)
//   DEVMAN_RESULT    = m4_l1 = offset 8  (i64)
//   DEVMAN_DEVICE_ID = m4_l2 = offset 16 (i64)
const OFF_TYPE: usize = 4; // i32: m_type
const OFF_M4_L1: usize = 8; // i64
const OFF_M4_L2: usize = 16; // i64
const OFF_M4_L3: usize = 24; // i64 — DEVMAN_ENDPOINT

/// Size of the serialized `DevmanDeviceInfo` header
/// (count, parent_dev_id, name_offset, subsystem_offset).
const DEVINFO_HEADER: usize = 16;

/// Maximum devinfo blob a driver registers (header + name).
const DEVINFO_MAX: usize = 128;

// Errno values used by the client (MINIX errno table).
const EINVAL: i32 = 22;
const ENOMEM: i32 = 12;
const EIO: i32 = 5;

// Helpers

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

/// Serialize a `DevmanDeviceInfo` blob for a device named `name` under
/// parent `parent_dev_id` (0 = the root device) into `out`. Returns the
/// blob length, or `None` if it does not fit.
///
/// Layout (matches the server's `DevmanDeviceInfo` + `do_add_device_inner`):
/// count=0, parent_dev_id, name_offset=16, subsystem_offset=0, then the
/// NUL-terminated name at offset 16.
pub fn serialize_devinfo(out: &mut [u8], name: &str, parent_dev_id: i32) -> Option<usize> {
    let name_len = name.len();
    let total = DEVINFO_HEADER.checked_add(name_len)?.checked_add(1)?;
    if total > out.len() {
        return None;
    }
    out[0..4].copy_from_slice(&0i32.to_ne_bytes()); // count
    out[4..8].copy_from_slice(&parent_dev_id.to_ne_bytes());
    out[8..12].copy_from_slice(&(DEVINFO_HEADER as u32).to_ne_bytes()); // name_offset
    out[12..16].copy_from_slice(&0u32.to_ne_bytes()); // subsystem_offset
    out[16..16 + name_len].copy_from_slice(name.as_bytes());
    out[16 + name_len] = 0;
    Some(total)
}

/// Direct-grant table registered with the kernel via `SYS_SETGRANT`
/// (kernel call 34) so devman can read serialized devinfo blobs through
/// `SYS_SAFECOPYFROM`.
struct DevmanGrantTable {
    entries: core::cell::UnsafeCell<[CpGrant; NR_DEVMAN_GRANTS]>,
}

unsafe impl Sync for DevmanGrantTable {}

impl DevmanGrantTable {
    const fn new() -> Self {
        const ENTRY: CpGrant = CpGrant {
            cp_flags: 0,
            cp_u: CpUnion {
                cp_direct: CpDirect {
                    cp_who_to: 0,
                    cp_start: 0,
                    cp_len: 0,
                    cp_reserved: [0u8; 8],
                },
            },
            cp_reserved: [0u8; 8],
        };
        Self {
            entries: core::cell::UnsafeCell::new([ENTRY; NR_DEVMAN_GRANTS]),
        }
    }

    #[cfg(target_os = "minix")]
    fn as_ptr(&self) -> u64 {
        self.entries.get() as u64
    }

    /// Allocate a direct grant giving `callee` read access to `len` bytes
    /// at `addr` (devman reads the devinfo blob — CPF_READ).
    fn grant_direct(&self, callee: i32, addr: u64, len: usize) -> i32 {
        unsafe {
            let entries = &mut *self.entries.get();
            for (i, entry) in entries.iter_mut().enumerate() {
                if entry.cp_flags == 0 {
                    entry.cp_flags = CPF_USED | CPF_VALID | CPF_DIRECT | CPF_READ;
                    entry.cp_u.cp_direct = CpDirect {
                        cp_who_to: callee,
                        cp_start: addr,
                        cp_len: len,
                        cp_reserved: [0u8; 8],
                    };
                    return i as i32;
                }
            }
        }
        GRANT_INVALID
    }

    /// Revoke a previously allocated grant, returning its slot to the pool.
    fn revoke(&self, grant_id: i32) {
        if grant_id < 0 || grant_id >= NR_DEVMAN_GRANTS as i32 {
            return;
        }
        unsafe {
            let entries = &mut *self.entries.get();
            entries[grant_id as usize].cp_flags = 0;
        }
    }
}

/// Number of grant entries in the devman client table.
const NR_DEVMAN_GRANTS: usize = 8;

#[cfg(target_os = "minix")]
static DEVMAN_GRANT_TABLE: DevmanGrantTable = DevmanGrantTable::new();

/// Register the devman grant table with the kernel (`SYS_SETGRANT`), once.
#[cfg(target_os = "minix")]
pub fn devman_grant_init() {
    static ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if !ONCE.swap(true, Ordering::Relaxed) {
        let mut msg = [0u8; 64];
        msg[8..16].copy_from_slice(&DEVMAN_GRANT_TABLE.as_ptr().to_le_bytes());
        msg[16..20].copy_from_slice(&(NR_DEVMAN_GRANTS as i32).to_le_bytes());
        let _ = minix_rt::kernel_call(34, &mut msg); // SYS_SETGRANT
    }
}

// Public API

/// Add a device named `name` under `parent_dev_id` (0 = the root device)
/// to the device tree. Blocks until devman replies; returns the new device
/// id, or an errno.
pub fn devman_add_device(name: &str, parent_dev_id: i32) -> Result<i32, MinixErr> {
    #[cfg(target_os = "minix")]
    {
        devman_grant_init();

        let mut blob = [0u8; DEVINFO_MAX];
        let blob_len = serialize_devinfo(&mut blob, name, parent_dev_id).ok_or(MinixErr(EINVAL))?;

        let grant = DEVMAN_GRANT_TABLE.grant_direct(DEVMAN_PROC_NR, blob.as_ptr() as u64, blob_len);
        if grant == GRANT_INVALID {
            return Err(MinixErr(ENOMEM));
        }

        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, DEVMAN_ADD_DEV as i32);
        msg_set_i64(&mut msg, OFF_M4_L1, grant as i64);
        msg_set_i64(&mut msg, OFF_M4_L2, blob_len as i64);

        let r = unsafe { minix_std::sendrec(DEVMAN_PROC_NR, &mut msg) };
        DEVMAN_GRANT_TABLE.revoke(grant);

        r?;
        if msg_get_i32(&msg, OFF_TYPE) != DEVMAN_REPLY as i32 {
            return Err(MinixErr(EIO));
        }
        let result = msg_get_i64(&msg, OFF_M4_L1);
        if result != 0 {
            // The server replies with a negative errno (MINIX convention).
            return Err(MinixErr(-result as i32));
        }
        Ok(msg_get_i64(&msg, OFF_M4_L2) as i32)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (name, parent_dev_id);
        Err(MinixErr(71))
    }
}

/// Remove a device from the device tree by ID.
pub fn devman_del_device(dev_id: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, DEVMAN_DEL_DEV as i32);
        msg_set_i64(&mut msg, OFF_M4_L2, dev_id as i64);
        unsafe { minix_std::sendrec(DEVMAN_PROC_NR, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = dev_id;
        Err(MinixErr(71))
    }
}

/// Add a bus to the device tree.
pub fn devman_add_bus(_name: &str) -> Result<i32, MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, DEVMAN_ADD_BUS as i32);
        unsafe { minix_std::sendrec(DEVMAN_PROC_NR, &mut msg) }?;
        check_result(&msg)?;
        Ok(msg_get_i64(&msg, OFF_M4_L1) as i32)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = _name;
        Err(MinixErr(71))
    }
}

/// Add a device file entry.
pub fn devman_add_devfile(dev_id: i32, _devfile: &str) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, DEVMAN_ADD_DEVFILE as i32);
        msg_set_i64(&mut msg, OFF_M4_L2, dev_id as i64);
        unsafe { minix_std::sendrec(DEVMAN_PROC_NR, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (dev_id, _devfile);
        Err(MinixErr(71))
    }
}

/// Bind a driver to a device (RS-only on the server side).
pub fn devman_bind(dev_id: i32, driver_endpoint: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, DEVMAN_BIND as i32);
        msg_set_i64(&mut msg, OFF_M4_L2, dev_id as i64);
        msg_set_i64(&mut msg, OFF_M4_L3, driver_endpoint as i64);
        unsafe { minix_std::sendrec(DEVMAN_PROC_NR, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (dev_id, driver_endpoint);
        Err(MinixErr(71))
    }
}

/// Unbind a driver from a device (RS-only on the server side).
pub fn devman_unbind(dev_id: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, DEVMAN_UNBIND as i32);
        msg_set_i64(&mut msg, OFF_M4_L2, dev_id as i64);
        unsafe { minix_std::sendrec(DEVMAN_PROC_NR, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = dev_id;
        Err(MinixErr(71))
    }
}

/// Check the reply: m_type must be DEVMAN_REPLY and DEVMAN_RESULT @8 == 0.
fn check_result(msg: &Message) -> Result<(), MinixErr> {
    if msg_get_i32(msg, OFF_TYPE) != DEVMAN_REPLY as i32 {
        return Err(MinixErr(EIO));
    }
    let result = msg_get_i64(msg, OFF_M4_L1);
    if result != 0 {
        // The server replies with a negative errno (MINIX convention).
        Err(MinixErr(-result as i32))
    } else {
        Ok(())
    }
}

// Tests

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
    fn test_serialize_devinfo_layout() {
        let mut out = [0u8; 64];
        let len = serialize_devinfo(&mut out, "tty0", 0).unwrap();
        // 16-byte header + "tty0" + NUL.
        assert_eq!(len, 21);
        assert_eq!(i32::from_ne_bytes(out[0..4].try_into().unwrap()), 0); // count
        assert_eq!(i32::from_ne_bytes(out[4..8].try_into().unwrap()), 0); // parent_dev_id
        assert_eq!(u32::from_ne_bytes(out[8..12].try_into().unwrap()), 16); // name_offset
        assert_eq!(u32::from_ne_bytes(out[12..16].try_into().unwrap()), 0); // subsystem_offset
        assert_eq!(&out[16..20], b"tty0");
        assert_eq!(out[20], 0);
    }

    #[test]
    fn test_serialize_devinfo_parent_and_overflow() {
        let mut out = [0u8; 32];
        let len = serialize_devinfo(&mut out, "disk0", 5).unwrap();
        assert_eq!(len, 22);
        assert_eq!(i32::from_ne_bytes(out[4..8].try_into().unwrap()), 5);
        assert_eq!(&out[16..21], b"disk0");

        let mut tiny = [0u8; 16];
        assert!(serialize_devinfo(&mut tiny, "x", 0).is_none());
    }

    #[test]
    fn test_add_device_message_layout() {
        // The ADD_DEV message must match the server's expectations:
        // m_type @4, grant id @8 (i64), grant size @16 (i64).
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, DEVMAN_ADD_DEV as i32);
        msg_set_i64(&mut msg, OFF_M4_L1, 7);
        msg_set_i64(&mut msg, OFF_M4_L2, 21);
        assert_eq!(msg_get_i32(&msg, 4), DEVMAN_ADD_DEV as i32);
        assert_eq!(msg_get_i64(&msg, 8), 7);
        assert_eq!(msg_get_i64(&msg, 16), 21);
    }

    #[test]
    fn test_check_result() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, DEVMAN_REPLY as i32);
        msg_set_i64(&mut msg, OFF_M4_L1, 0);
        msg_set_i64(&mut msg, OFF_M4_L2, 42);
        assert!(check_result(&msg).is_ok());

        // The server replies with negative errnos; MinixErr stores positive.
        msg_set_i64(&mut msg, OFF_M4_L1, -22);
        let err = check_result(&msg).unwrap_err();
        assert_eq!(err.0, 22);
    }

    #[test]
    fn test_devman_add_device_returns_enosys_on_host() {
        let r = devman_add_device("test_dev", 0);
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
        msg_set_i32(&mut msg, 4, -42);
        assert_eq!(msg_get_i32(&msg, 4), -42);
        msg_set_i64(&mut msg, 8, -4200);
        assert_eq!(msg_get_i64(&msg, 8), -4200);
    }

    #[test]
    fn test_message_size() {
        assert_eq!(core::mem::size_of::<Message>(), 64);
    }
}
