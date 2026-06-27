//! Safe copy grant types from `minix/safecopies.h`

use crate::types::{CpGrantId, Endpoint, VirBytes};
use core::fmt;

// ── Grant struct ────────────────────────────────────────────────────────

/// A grant entry — direct, indirect, or magic.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpGrant {
    pub cp_flags: i32,
    pub cp_u: CpUnion,
    pub cp_reserved: [u8; 8],
}

impl fmt::Debug for CpGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpGrant")
            .field("cp_flags", &self.cp_flags)
            .finish()
    }
}

/// The grant union: direct, indirect, or magic variant.
#[repr(C)]
#[derive(Clone, Copy)]
pub union CpUnion {
    pub cp_direct: CpDirect,
    pub cp_indirect: CpIndirect,
    pub cp_magic: CpMagic,
}

impl fmt::Debug for CpUnion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpUnion").finish()
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpDirect {
    pub cp_who_to: Endpoint,
    pub cp_start: VirBytes,
    pub cp_len: usize,
    pub cp_reserved: [u8; 8],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpIndirect {
    pub cp_who_to: Endpoint,
    pub cp_who_from: Endpoint,
    pub cp_grant: CpGrantId,
    pub cp_reserved: [u8; 8],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpMagic {
    pub cp_who_from: Endpoint,
    pub cp_who_to: Endpoint,
    pub cp_start: VirBytes,
    pub cp_len: usize,
    pub cp_reserved: [u8; 8],
}

// ── Vectored safecopy descriptor ────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VscpVec {
    pub v_from: Endpoint,
    pub v_to: Endpoint,
    pub v_gid: CpGrantId,
    pub v_offset: usize,
    pub v_addr: VirBytes,
    pub v_bytes: usize,
}

// ── Constants ───────────────────────────────────────────────────────────

pub const GRANT_INVALID: CpGrantId = -1;

pub const fn grant_valid(g: CpGrantId) -> bool {
    g > GRANT_INVALID
}

pub const CPF_READ: i32 = 0x000001;
pub const CPF_WRITE: i32 = 0x000002;
pub const CPF_TRY: i32 = 0x000010;
pub const CPF_USED: i32 = 0x000100;
pub const CPF_DIRECT: i32 = 0x000200;
pub const CPF_INDIRECT: i32 = 0x000400;
pub const CPF_MAGIC: i32 = 0x000800;
pub const CPF_VALID: i32 = 0x001000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grant_invalid() {
        assert_eq!(GRANT_INVALID, -1);
        assert!(!grant_valid(GRANT_INVALID));
        assert!(grant_valid(0));
        assert!(grant_valid(100));
    }

    #[test]
    fn test_grant_flags() {
        assert_eq!(CPF_READ, 0x000001);
        assert_eq!(CPF_WRITE, 0x000002);
        assert_eq!(CPF_DIRECT, 0x000200);
        assert_eq!(CPF_INDIRECT, 0x000400);
        assert_eq!(CPF_MAGIC, 0x000800);
        assert_eq!(CPF_VALID, 0x001000);
    }

    #[test]
    fn test_grant_struct_size() {
        assert!(size_of::<CpGrant>() >= 36);
    }

    #[test]
    fn test_vscp_vec_size() {
        assert!(size_of::<VscpVec>() >= 32);
    }
}
