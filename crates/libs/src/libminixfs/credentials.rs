//! Credential fetching functions for MFS.
//!
//! Ported from Minix 3.3.0's `libminixfs/fetch_credentials.c`.
//! Used by MFS to obtain caller credentials from VFS.

/// Stub: fetch credentials from VFS.
///
/// In the original Minix, this copies the caller's UID/GID/group credentials
/// from a VFS grant via `sys_safecopyfrom`.  Until the VFS protocol layer is
/// ported, this is a stub.
///
/// Returns `ENOSYS` (not implemented).
pub fn fetch_credentials(_who: i32, _who_e: i32, _vacc: *mut u8, _flags: u32) -> i32 {
    // TODO: implement when VFS protocol is wired.
    todo!("VFS credential protocol not yet ported; see NEXT.md");
}
