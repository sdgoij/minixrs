//! Faulty Block Device — fault injection proxy for block I/O testing.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/fbd/`
//!
//! This driver sits between VFS and a real block device, forwarding
//! requests while injecting configurable faults (delays, corruptions,
//! drops) according to user-defined rules.  It is used for testing
//! the resilience of file systems and upper layers against block
//! device failures.
//!
//! All operations depend on IPC to the underlying block driver and
//! the DS (Data Store).  The rule engine and fault injection actions
//! are deferred until the server framework is available (Phase 12).

use crate::DriverError;
use core::cell::UnsafeCell;

/// Maximum number of rules that can be active at once.
pub const MAX_RULES: usize = 16;

/// Scratch buffer size (NR_IOREQS * CLICK_SIZE = 32 * 4096 = 128 KB).
pub const FBD_BUF_SIZE: usize = 128 * 1024;

/// Maximum driver label length.
pub const LABEL_SIZE: usize = 32;

/// Apply pre-transfer hook.
pub const PRE_HOOK: u32 = 0x1;
/// Apply I/O hook (copy mode).
pub const IO_HOOK: u32 = 0x2;
/// Apply post-transfer hook.
pub const POST_HOOK: u32 = 0x4;

/// Add a fault injection rule.
pub const FBDCADDRULE: u32 = 0x7800;
/// Delete a fault injection rule.
pub const FBDCDELRULE: u32 = 0x7801;
/// Get a fault injection rule.
pub const FBDCGETRULE: u32 = 0x7802;

/// Action types for fault injection rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FbdAction {
    None = 0,
    Delay = 1,
    Corrupt = 2,
    Drop = 3,
    Misplace = 4,
    Reorder = 5,
    Stale = 6,
}

pub const FBD_FLAG_READ: u32 = 0x01;
pub const FBD_FLAG_WRITE: u32 = 0x02;

/// A single fault injection rule.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FbdRule {
    /// Action to perform.
    pub action: u32,
    /// Flags (FBD_FLAG_READ / FBD_FLAG_WRITE).
    pub flags: u32,
    /// Start byte position.
    pub pos_start: u64,
    /// End byte position (exclusive).
    pub pos_end: u64,
    /// Probability percentage (0-100).
    pub probability: u32,
    /// Extra parameter (e.g. delay in ticks, corruption pattern).
    pub extra: u32,
}

impl FbdRule {
    pub const fn new() -> Self {
        Self {
            action: 0,
            flags: 0,
            pos_start: 0,
            pos_end: 0,
            probability: 0,
            extra: 0,
        }
    }
}

impl Default for FbdRule {
    fn default() -> Self {
        Self::new()
    }
}

/// FBD driver configuration.
#[derive(Debug, Clone)]
#[repr(C)]
pub struct FbdConfig {
    pub driver_label: [u8; LABEL_SIZE],
    pub driver_minor: i32,
    pub endpoint: i32,
}

impl FbdConfig {
    pub const fn new() -> Self {
        Self {
            driver_label: [0u8; LABEL_SIZE],
            driver_minor: -1,
            endpoint: -1,
        }
    }
}

impl Default for FbdConfig {
    fn default() -> Self {
        Self::new()
    }
}

struct RulesCell(UnsafeCell<[FbdRule; MAX_RULES]>);
unsafe impl Sync for RulesCell {}
impl RulesCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([const { FbdRule::new() }; MAX_RULES]))
    }
    fn get(&self) -> *mut [FbdRule; MAX_RULES] {
        self.0.get()
    }
}

/// Static rule table for fault injection.
static RULES: RulesCell = RulesCell::new();

/// Find a rule by index (0-based). Returns None if out of range or empty.
fn rule_find(index: usize) -> Option<&'static FbdRule> {
    if index >= MAX_RULES {
        return None;
    }
    unsafe {
        let rules = RULES.get();
        let rule = &(*rules)[index];
        if rule.action == 0 && rule.flags == 0 && rule.probability == 0 {
            return None;
        }
        Some(rule)
    }
}

/// Find a rule index by position and flags. Returns the index of the first
/// matching rule, or None.
fn rule_find_by_pos(position: u64, do_write: bool, action: u32) -> Option<usize> {
    let flags = if do_write {
        FBD_FLAG_WRITE
    } else {
        FBD_FLAG_READ
    };
    unsafe {
        let rules = RULES.get();
        #[allow(clippy::needless_range_loop)]
        for i in 0..MAX_RULES {
            let rule = &(*rules)[i];
            if rule.action == 0 && rule.flags == 0 && rule.probability == 0 {
                continue;
            }
            if rule.flags & flags == 0 {
                continue;
            }
            if rule.action != action {
                continue;
            }
            if position < rule.pos_start || position >= rule.pos_end {
                continue;
            }
            return Some(i);
        }
    }
    None
}

/// Pre-transfer hook: check if the transfer should be modified.
fn rule_pre_hook(position: u64, do_write: bool) -> Option<&'static FbdRule> {
    if let Some(_idx) = rule_find_by_pos(position, do_write, FbdAction::Drop as u32) {
        // Drop: pretend the I/O succeeded but discard data.
        return None; // handled by the caller returning success with 0 bytes
    }
    if let Some(_idx) = rule_find_by_pos(position, do_write, FbdAction::Delay as u32) {
        // Delay: would sleep here. For now, just pass through.
    }
    if let Some(idx) = rule_find_by_pos(position, do_write, FbdAction::Corrupt as u32) {
        return rule_find(idx);
    }
    None
}

/// Callback type for forwarding block I/O to the underlying driver.
pub type FbdIoFn =
    unsafe fn(position: u64, buf: &mut [u8], do_write: bool) -> Result<usize, DriverError>;

/// Open the faulty block device.
pub fn fbd_open(_minor: usize, _access: i32) -> Result<(), DriverError> {
    // Forwarding to the underlying driver would happen here via IPC.
    // Without a driver endpoint, just acknowledge the open.
    Ok(())
}

/// Close the faulty block device.
pub fn fbd_close(_minor: usize) -> Result<(), DriverError> {
    Ok(())
}

/// Transfer data through the faulty block device with optional fault injection.
pub fn fbd_transfer(
    _minor: usize,
    do_write: bool,
    position: u64,
    buf: &mut [u8],
    io: FbdIoFn,
) -> Result<usize, DriverError> {
    // Check pre-transfer hooks.
    if let Some(rule) = rule_pre_hook(position, do_write)
        && rule.action == FbdAction::Corrupt as u32
    {
        // Corrupt: flip some bits in the data.
        let corrupt_byte = (position % buf.len() as u64) as usize;
        if corrupt_byte < buf.len() {
            buf[corrupt_byte] ^= 0xFF;
        }
    }

    // Check for drop: return success with 0 bytes.
    if rule_find_by_pos(position, do_write, FbdAction::Drop as u32).is_some() {
        return Ok(0);
    }

    // Forward the I/O to the underlying driver.
    unsafe { io(position, buf, do_write) }
}

/// Handle an IOCTL request on the faulty block device.
pub fn fbd_ioctl(request: u32, arg: u64) -> Result<(), DriverError> {
    match request {
        FBDCADDRULE => {
            // Add a rule: arg points to a FbdRule in userspace (stub: ignore).
            // In the real implementation, copy the rule from userspace.
            let _ = arg;
            Err(DriverError::NotFound)
        }
        FBDCDELRULE => {
            // Delete a rule: arg is the rule index.
            let idx = arg as usize;
            if idx < MAX_RULES {
                unsafe {
                    (*RULES.get())[idx] = FbdRule::new();
                }
                Ok(())
            } else {
                Err(DriverError::NotFound)
            }
        }
        FBDCGETRULE => {
            // Get a rule: arg is the rule index.
            let idx = arg as usize;
            if rule_find(idx).is_some() {
                Ok(())
            } else {
                Err(DriverError::NotFound)
            }
        }
        _ => Err(DriverError::NotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reset global state for tests.
    unsafe fn reset() {
        unsafe {
            let rules = RULES.get();
            for i in 0..MAX_RULES {
                core::ptr::write(&mut (*rules)[i], FbdRule::new());
            }
        }
    }

    #[test]
    fn test_constants() {
        assert_eq!(MAX_RULES, 16);
        assert_eq!(FBD_BUF_SIZE, 131072);
        assert_eq!(PRE_HOOK, 0x1);
        assert_eq!(IO_HOOK, 0x2);
        assert_eq!(POST_HOOK, 0x4);
    }

    #[test]
    fn test_ioctl_codes() {
        assert_eq!(FBDCADDRULE, 0x7800);
        assert_eq!(FBDCDELRULE, 0x7801);
        assert_eq!(FBDCGETRULE, 0x7802);
    }

    #[test]
    fn test_fbd_rule_new() {
        let rule = FbdRule::new();
        assert_eq!(rule.action, 0);
        assert_eq!(rule.probability, 0);
    }

    #[test]
    fn test_fbd_rule_default() {
        let rule: FbdRule = Default::default();
        assert_eq!(rule.pos_start, 0);
        assert_eq!(rule.pos_end, 0);
    }

    #[test]
    fn test_fbd_config_new() {
        let cfg = FbdConfig::new();
        assert_eq!(cfg.driver_minor, -1);
        assert_eq!(cfg.endpoint, -1);
    }

    #[test]
    fn test_action_types() {
        assert_eq!(FbdAction::None as u32, 0);
        assert_eq!(FbdAction::Delay as u32, 1);
        assert_eq!(FbdAction::Corrupt as u32, 2);
        assert_eq!(FbdAction::Drop as u32, 3);
        assert_eq!(FbdAction::Misplace as u32, 4);
        assert_eq!(FbdAction::Reorder as u32, 5);
        assert_eq!(FbdAction::Stale as u32, 6);
    }

    #[test]
    fn test_flags() {
        assert_eq!(FBD_FLAG_READ, 0x01);
        assert_eq!(FBD_FLAG_WRITE, 0x02);
    }

    #[test]
    fn test_rule_send() {
        fn assert_send<T: Send>() {}
        assert_send::<FbdRule>();
    }

    #[test]
    fn test_config_send() {
        fn assert_send<T: Send>() {}
        assert_send::<FbdConfig>();
    }

    #[test]
    fn test_fbd_open_close_ok() {
        assert!(fbd_open(0, 0).is_ok());
        assert!(fbd_close(0).is_ok());
    }

    #[test]
    fn test_rule_find_empty() {
        unsafe {
            reset();
        }
        assert!(rule_find(0).is_none());
        assert!(rule_find(99).is_none());
    }

    #[test]
    fn test_rule_find_by_pos_no_match() {
        unsafe {
            reset();
        }
        assert!(rule_find_by_pos(0, false, FbdAction::Drop as u32).is_none());
    }

    #[test]
    fn test_fbd_ioctl_delete_nonexistent() {
        unsafe {
            reset();
        }
        assert!(fbd_ioctl(FBDCDELRULE, 99).is_err());
        assert!(fbd_ioctl(FBDCGETRULE, 0).is_err());
    }

    #[test]
    fn test_fbd_ioctl_invalid_request() {
        assert!(fbd_ioctl(0xFFFF, 0).is_err());
    }

    #[test]
    fn test_fbd_ioctl_delete_rule() {
        unsafe {
            reset();
        }
        // Delete an empty slot should succeed.
        assert!(fbd_ioctl(FBDCDELRULE, 0).is_ok());
    }

    #[test]
    fn test_fbd_transfer_forward() {
        unsafe {
            reset();
        }
        // Mock I/O callback that writes a pattern.
        unsafe fn mock_io(
            _pos: u64,
            buf: &mut [u8],
            _do_write: bool,
        ) -> Result<usize, DriverError> {
            buf.fill(0x42);
            Ok(buf.len())
        }
        let mut buf = [0u8; 64];
        let r = fbd_transfer(0, true, 0, &mut buf, mock_io);
        assert!(r.is_ok());
        assert_eq!(r.unwrap(), 64);
        assert!(buf.iter().all(|&b| b == 0x42));
    }

    #[test]
    fn test_fbd_transfer_drop() {
        unsafe {
            reset();
            // Add a drop rule.
            (*RULES.get())[0] = FbdRule {
                action: FbdAction::Drop as u32,
                flags: FBD_FLAG_WRITE,
                pos_start: 0,
                pos_end: 65536,
                probability: 100,
                extra: 0,
            };
        }
        unsafe fn mock_io(
            _pos: u64,
            buf: &mut [u8],
            _do_write: bool,
        ) -> Result<usize, DriverError> {
            buf.fill(0x42);
            Ok(buf.len())
        }
        let mut buf = [0xABu8; 64];
        let r = fbd_transfer(0, true, 0, &mut buf, mock_io);
        // Drop returns 0 bytes (I/O is faked as success with 0 bytes).
        assert!(r.is_ok());
        assert_eq!(r.unwrap(), 0);
    }
}
