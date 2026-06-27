//! Privilege structures — placeholder for task 3.2.
//!
//! Minimal stub to satisfy `Proc.p_priv: *mut Priv` until 3.2 is
//! implemented.

/// Privilege structure (placeholder).
#[derive(Debug, Default)]
#[repr(C)]
pub struct Priv {
    _data: [u8; 0],
}
