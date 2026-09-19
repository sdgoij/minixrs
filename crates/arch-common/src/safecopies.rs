//! Safe copy grant types from `minix/safecopies.h`

use crate::types::{CpGrantId, Endpoint, VirBytes};
use core::fmt;

/// A copy between two processes' address spaces, made by a layer that can reach
/// both. Returns 0, or a negative errno.
///
/// This is what `SYS_VIRCOPY` reduces to once the endpoints and ranges have been
/// validated. An arch whose page tables already join two address spaces makes
/// the copy itself and never needs one of these; an arch whose processes live in
/// separate memories must hand it to whatever owns them both.
///
/// The signature lives here rather than next to either user because the HAL that
/// supplies it and the kernel that calls it depend on this crate, and neither
/// depends on the other.
pub type CrossAddressSpaceCopy = unsafe fn(i32, u64, i32, u64, usize) -> i32;

/// The "process" whose address space is the kernel's own, for
/// [`CrossAddressSpaceCopy`].
///
/// Negative so it cannot collide with a process number, and equal to the `-1`
/// `do_copy_common` already substitutes for `NONE` — so `SYS_VIRCOPY`'s kernel
/// side and the kernel's internal copies name the same thing.
pub const KERNEL_ADDRESS_SPACE: i32 = -1;

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

pub const GRANT_INVALID: CpGrantId = -1;

/// The `cp_who_to` value that names no particular grantee, so a grant carrying
/// it serves whoever asks.
///
/// C spells this `ANY` (`_ENDPOINT_SLOT_TOP - 1`, i.e. 31744) and compares
/// against it in `verify_grant`. This port has no separate `ANY`: `mini_receive`
/// normalizes the wire sentinel `0x0000ffff` into the kernel's `NONE` (31743),
/// so that is the value the kernel's grantee check compares against, and
/// `PORTING_PLAN.md` finding 18 records the divergence from C. This constant is
/// here rather than in either side so that the kernel's check and a granter
/// filling in an entry cannot drift apart.
pub const GRANTEE_ANY: i32 = 31743;

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

    /// The field offsets the M2 host fixture writes into an instance — it has no
    /// way to use these types, so its `CpGrant` is a handful of literal offsets
    /// and this is what keeps them honest. They are the same on wasm32 as here:
    /// `cp_start` is a `u64` in both, so `cp_len` lands at 24 either way (only its
    /// width differs, which the fixture writes as a `u32`) and `cp_reserved`
    /// keeps the struct at 48 bytes.
    #[test]
    fn test_cp_grant_layout() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(CpGrant, cp_flags), 0);
        assert_eq!(offset_of!(CpGrant, cp_u.cp_direct.cp_who_to), 8);
        assert_eq!(offset_of!(CpGrant, cp_u.cp_direct.cp_start), 16);
        assert_eq!(offset_of!(CpGrant, cp_u.cp_direct.cp_len), 24);
        assert_eq!(size_of::<CpGrant>(), 48);
    }

    #[test]
    fn test_vscp_vec_size() {
        assert!(size_of::<VscpVec>() >= 32);
    }
}
