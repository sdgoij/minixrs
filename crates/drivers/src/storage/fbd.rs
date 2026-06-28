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

// ═══════════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════════

/// Maximum number of rules that can be active at once.
pub const MAX_RULES: usize = 16;

/// Scratch buffer size (NR_IOREQS * CLICK_SIZE = 32 * 4096 = 128 KB).
pub const FBD_BUF_SIZE: usize = 128 * 1024;

/// Maximum driver label length.
pub const LABEL_SIZE: usize = 32;

// ── Hook flags (from `rule.h`) ───────────────────────────────────────────

/// Apply pre-transfer hook.
pub const PRE_HOOK: u32 = 0x1;
/// Apply I/O hook (copy mode).
pub const IO_HOOK: u32 = 0x2;
/// Apply post-transfer hook.
pub const POST_HOOK: u32 = 0x4;

// ── IOCTL request codes (from `sys/ioc_fbd.h`) ──────────────────────────

/// Add a fault injection rule.
pub const FBDCADDRULE: u32 = 0x7800;
/// Delete a fault injection rule.
pub const FBDCDELRULE: u32 = 0x7801;
/// Get a fault injection rule.
pub const FBDCGETRULE: u32 = 0x7802;

// ── Fault injection action types ─────────────────────────────────────────

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

// ── Fault flags ──────────────────────────────────────────────────────────

pub const FBD_FLAG_READ: u32 = 0x01;
pub const FBD_FLAG_WRITE: u32 = 0x02;

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// Deferred API
// ═══════════════════════════════════════════════════════════════════════════

/// Open the faulty block device.
pub fn fbd_open(_minor: usize, _access: i32) -> Result<(), DriverError> {
    todo!("fbd_open needs IPC to underlying driver; see PORTING_PLAN.md 12.19")
}

/// Close the faulty block device.
pub fn fbd_close(_minor: usize) -> Result<(), DriverError> {
    todo!("fbd_close needs IPC to underlying driver; see PORTING_PLAN.md 12.19")
}

/// Transfer data through the faulty block device.
pub fn fbd_transfer(
    _minor: usize,
    _do_write: bool,
    _position: u64,
    _buf: &mut [u8],
) -> Result<usize, DriverError> {
    todo!("fbd_transfer needs IPC + rule engine; see PORTING_PLAN.md 12.19")
}

/// Handle an IOCTL request on the faulty block device.
pub fn fbd_ioctl(_request: u32) -> Result<(), DriverError> {
    todo!("fbd_ioctl needs rule engine; see PORTING_PLAN.md 12.19")
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

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
}
