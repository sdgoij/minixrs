//! Grant-based safe copy infrastructure — adapted from `system/do_safecopy.c`
//!
//! Provides `verify_grant()`, `safecopy()`, and system call handlers
//! (`do_safecopy_from`, `do_safecopy_to`, `do_vsafecopy`) for safe
//! inter-process data transfer using pre-granted access permissions.

use arch_common::types::CpGrantId;

use crate::proc::*;
use crate::table::{endpoint_slot, is_ok_endpoint, proc_addr};

// ── Re-export types from arch-common ───────────────────────────────────

pub use arch_common::safecopies::*;

// ── Constants ──────────────────────────────────────────────────────────

/// Maximum depth for following indirect grant chains.
pub const MAX_INDIRECT_DEPTH: usize = 5;

/// Top of user-accessible memory (32-bit limit for grant arithmetic;
/// matches C `MEM_TOP = 0xFFFFFFFFUL`).
const MEM_TOP: u64 = 0xFFFFFFFF;

/// Maximum number of vectored safecopy elements.
pub const SCPVEC_NR: usize = 64;

/// Error codes for grant operations.
pub const ELOOP: i32 = -121;
pub const EFAULT_SRC: i32 = -995; // from kernel/vm.h
pub const EFAULT_DST: i32 = -994;
pub const EINVAL: i32 = -22;
pub const EPERM: i32 = -1;
pub const OK: i32 = 0;

// ── Grant validation helper ────────────────────────────────────────────

/// Check if a grant ID is valid (non-negative).
pub fn grant_valid(g: CpGrantId) -> bool {
    g >= 0
}

// ── verify_grant ───────────────────────────────────────────────────────

/// Verify a grant and resolve it to an address and granter.
///
/// Follows indirect grant chains up to `MAX_INDIRECT_DEPTH`.
/// Returns `(offset_result, e_granter, flags)` on success, or an error code.
///
/// # Safety
///
/// Granter and grantee endpoints must be valid. The grant table must
/// be readable from the granter's address space.
pub unsafe fn verify_grant(
    granter: i32,
    grantee: i32,
    grant_id: i32,
    bytes: u64,
    access: i32,
    offset_in: u64,
) -> Result<(u64, i32, i32), i32> {
    unsafe {
        let mut cur_granter = granter;
        let mut cur_grantee = grantee;
        let mut cur_grant = grant_id;
        let mut depth = 0;

        loop {
            // Validate granter endpoint
            if !is_ok_endpoint(cur_granter) {
                return Err(EINVAL);
            }
            if !grant_valid(cur_grant) {
                return Err(EINVAL);
            }

            let proc_nr = endpoint_slot(cur_granter) as usize;
            let granter_proc = proc_addr(proc_nr as i32);
            if granter_proc.is_null() {
                return Err(EINVAL);
            }

            // Check for grant table
            if (*granter_proc).p_priv.is_null() {
                return Err(EPERM);
            }
            let priv_data = &*(*granter_proc).p_priv;
            if priv_data.s_grant_table == 0 || priv_data.s_grant_entries <= 0 {
                return Err(EPERM);
            }
            if cur_grant >= priv_data.s_grant_entries {
                return Err(EPERM);
            }

            // Read the grant entry from the granter's grant table.
            // On bare metal with per-process page tables, the grant table
            // address is only valid in the granter's address space, so we
            // switch CR3 to read it. In test mode (BOOT_CR3 == 0) or when
            // the granter has no per-process page table (identity-mapped),
            // read via the identity-mapped address directly.
            let grant_entry_addr = priv_data.s_grant_table
                + (cur_grant as u64) * core::mem::size_of::<CpGrant>() as u64;

            let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
            let g = if boot_cr3 != 0 && (*granter_proc).p_seg.p_cr3 != 0 {
                let saved = arch_x86_64::asm::read_cr3();
                arch_x86_64::asm::write_cr3((*granter_proc).p_seg.p_cr3);
                let entry = core::ptr::read(grant_entry_addr as *const CpGrant);
                arch_x86_64::asm::write_cr3(saved);
                entry
            } else {
                // No per-process page table — read via identity map
                core::ptr::read(grant_entry_addr as *const CpGrant)
            };

            let flags = g.cp_flags;

            // Check validity (CPF_USED | CPF_VALID)
            if flags & (CPF_USED | CPF_VALID) != (CPF_USED | CPF_VALID) {
                return Err(EPERM);
            }

            // Follow indirect grants
            if flags & CPF_INDIRECT != 0 {
                if depth >= MAX_INDIRECT_DEPTH {
                    return Err(ELOOP);
                }
                depth += 1;

                // Verify actual grantee for indirect
                let indirect = g.cp_u.cp_indirect;
                if indirect.cp_who_to != cur_grantee
                    && cur_grantee != crate::system::NONE
                    && indirect.cp_who_to != crate::system::NONE
                {
                    return Err(EPERM);
                }

                // Follow the chain (C: grantee = granter)
                cur_grantee = cur_granter;
                cur_granter = indirect.cp_who_from;
                cur_grant = indirect.cp_grant;
                continue;
            }

            // Not indirect — check access
            if flags & access != access {
                return Err(EPERM);
            }

            // Resolve based on grant type
            if flags & CPF_DIRECT != 0 {
                let direct = g.cp_u.cp_direct;

                // Check for wrapping — only meaningful for addresses within the
                // 32-bit range where arithmetic can wrap around MEM_TOP.
                if direct.cp_start <= MEM_TOP
                    && MEM_TOP - direct.cp_len as u64 + 1 < direct.cp_start
                {
                    return Err(EPERM);
                }

                // Verify grantee
                if direct.cp_who_to != cur_grantee
                    && cur_grantee != crate::system::NONE
                    && direct.cp_who_to != crate::system::NONE
                {
                    return Err(EPERM);
                }

                // Verify copy range
                let end = offset_in.checked_add(bytes).ok_or(EPERM)?;
                if end > direct.cp_len as u64 {
                    return Err(EPERM);
                }

                let offset_result = direct.cp_start + offset_in;
                return Ok((offset_result, cur_granter, flags));
            }

            if flags & CPF_MAGIC != 0 {
                // Only VFS may do magic grants (C: if(granter != VFS_PROC_NR))
                // Compare process slots (endpoints encode slot + generation).
                if endpoint_slot(cur_granter) != arch_common::com::VFS_PROC_NR {
                    return Err(EPERM);
                }
                let magic = g.cp_u.cp_magic;

                // Verify grantee
                if magic.cp_who_to != cur_grantee
                    && cur_grantee != crate::system::NONE
                    && magic.cp_who_to != crate::system::NONE
                {
                    return Err(EPERM);
                }

                // Verify copy range
                let end = offset_in.checked_add(bytes).ok_or(EPERM)?;
                if end > magic.cp_len as u64 {
                    return Err(EPERM);
                }

                let offset_result = magic.cp_start + offset_in;
                return Ok((offset_result, magic.cp_who_from, flags));
            }

            return Err(EPERM); // unknown grant type
        }
    }
}

// ── safecopy ───────────────────────────────────────────────────────────

/// Perform a safe copy using grant verification.
///
/// `caller` is the process requesting the copy.
/// `access` is `CPF_READ` (copy FROM granter) or `CPF_WRITE` (copy TO granter).
///
/// # Safety
///
/// All endpoints must be valid. The caller's address must be accessible.
#[allow(clippy::too_many_arguments)]
pub unsafe fn safecopy(
    caller: *mut Proc,
    granter: i32,
    grantee: i32,
    grant_id: i32,
    bytes: u64,
    g_offset: u64,
    addr: u64,
    access: i32,
) -> i32 {
    unsafe {
        if granter == crate::system::NONE || grantee == crate::system::NONE {
            return EFAULT_SRC;
        }

        // Phase 6.14: validate caller's buffer address via page table walk.
        // Walks the caller's per-process page table to verify every page
        // in `addr..addr+bytes` is present. For kernel tasks (no per-process
        // CR3), falls back to trusting the address (same as identity-mapped
        // kernel access pattern).
        if !crate::vm::vm_check_range(caller, addr, bytes) {
            return EFAULT_DST;
        }

        // Verify the grant
        let r = verify_grant(granter, grantee, grant_id, bytes, access, g_offset);
        let (v_offset, new_granter, flags) = match r {
            Ok(v) => v,
            Err(e) => return e,
        };

        // Determine src and dst addresses
        let (src_addr, dst_addr) = if access & CPF_READ != 0 {
            // Copy FROM granter TO grantee (caller)
            (v_offset, addr)
        } else {
            // Copy FROM grantee (caller) TO granter
            (addr, v_offset)
        };

        // Perform the copy
        if bytes == 0 {
            return OK;
        }

        // Phase 6.14: wire new_granter into copy path and
        // differentiate CPF_TRY from normal copies.
        //
        // Magic grants set `new_granter` to `cp_who_from` (the
        // process whose memory is actually being accessed). Use
        // the effective granter's CR3 to access v_offset and the
        // caller's CR3 to access addr.
        //
        // CPF_TRY grants prevent page-fault-on-demand behavior;
        // use a direct identity-map copy (same as C's virtual_copy
        // for try grants vs virtual_copy_vmcheck for normal grants).
        if flags & CPF_TRY != 0 {
            core::ptr::copy_nonoverlapping(
                src_addr as *const u8,
                dst_addr as *mut u8,
                bytes as usize,
            );
            OK
        } else {
            let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
            if boot_cr3 == 0 {
                // Pre-init / test mode: direct copy
                core::ptr::copy_nonoverlapping(
                    src_addr as *const u8,
                    dst_addr as *mut u8,
                    bytes as usize,
                );
                return OK;
            }

            // Normal: CR3-switched copy using virtual_copy.
            // virtual_copy handles the bounce-buffer switching
            // between source and destination address spaces.
            // It returns non-zero when a process has no per-process
            // CR3 (kernel task using identity map); in that case
            // fall back to direct copy since both addresses are
            // accessible from the kernel's current CR3.
            let caller_slot = endpoint_slot((*caller).p_endpoint);
            let effective_slot = endpoint_slot(new_granter);

            let (src_proc, dst_proc) = if access & CPF_READ != 0 {
                (effective_slot, caller_slot)
            } else {
                (caller_slot, effective_slot)
            };

            if crate::vm::virtual_copy(src_proc, src_addr, dst_proc, dst_addr, bytes as usize) != 0
            {
                // Fallback: identity-mapped (kernel task) addresses
                core::ptr::copy_nonoverlapping(
                    src_addr as *const u8,
                    dst_addr as *mut u8,
                    bytes as usize,
                );
            }
            OK
        }
    }
}

// ── do_safecopy_to ─────────────────────────────────────────────────────

/// SYS_SAFECOPYTO handler — copy FROM caller TO granter's granted memory.
///
/// # Safety
///
/// Caller process must be valid; message fields must be populated.
pub unsafe fn do_safecopy_to(caller: *mut Proc, msg: &[u8; MESSAGE_SIZE]) -> i32 {
    let granter = i32::from_ne_bytes(msg[8..12].try_into().unwrap_or([0u8; 4]));
    let grant_id: i32 = i32::from_ne_bytes(msg[12..16].try_into().unwrap_or([0u8; 4]));
    let offset: u64 = u64::from_ne_bytes(msg[16..24].try_into().unwrap_or([0u8; 8]));
    let addr: u64 = u64::from_ne_bytes(msg[24..32].try_into().unwrap_or([0u8; 8]));
    let bytes: u64 = u64::from_ne_bytes(msg[32..40].try_into().unwrap_or([0u8; 8]));

    unsafe {
        safecopy(
            caller,
            granter,
            (*caller).p_endpoint,
            grant_id,
            bytes,
            offset,
            addr,
            CPF_WRITE,
        )
    }
}

// ── do_safecopy_from ───────────────────────────────────────────────────

/// SYS_SAFECOPYFROM handler — copy FROM granter's granted memory TO caller.
///
/// # Safety
///
/// Caller process must be valid; message fields must be populated.
pub unsafe fn do_safecopy_from(caller: *mut Proc, msg: &[u8; MESSAGE_SIZE]) -> i32 {
    let granter = i32::from_ne_bytes(msg[8..12].try_into().unwrap_or([0u8; 4]));
    let grant_id: i32 = i32::from_ne_bytes(msg[12..16].try_into().unwrap_or([0u8; 4]));
    let offset: u64 = u64::from_ne_bytes(msg[16..24].try_into().unwrap_or([0u8; 8]));
    let addr: u64 = u64::from_ne_bytes(msg[24..32].try_into().unwrap_or([0u8; 8]));
    let bytes: u64 = u64::from_ne_bytes(msg[32..40].try_into().unwrap_or([0u8; 8]));

    unsafe {
        safecopy(
            caller,
            granter,
            (*caller).p_endpoint,
            grant_id,
            bytes,
            offset,
            addr,
            CPF_READ,
        )
    }
}

// ── do_vsafecopy ──────────────────────────────────────────────────────

/// SYS_VSAFECOPY handler — vectored safe copy.
///
/// # Safety
///
/// Caller process must be valid; message must contain vector address/size.
pub unsafe fn do_vsafecopy(caller: *mut Proc, msg: &[u8; MESSAGE_SIZE]) -> i32 {
    let vec_addr: u64 = u64::from_ne_bytes(msg[8..16].try_into().unwrap_or([0u8; 8]));
    let vec_size: usize = u64::from_ne_bytes(msg[16..24].try_into().unwrap_or([0u8; 8])) as usize;

    // Limit vector size
    let els = vec_size.min(SCPVEC_NR);
    if els == 0 {
        return OK;
    }

    unsafe {
        // Read the vector from caller's address space
        let mut vec: [VscpVec; SCPVEC_NR] = core::mem::zeroed();
        let vec_bytes = els * size_of::<VscpVec>();
        core::ptr::copy_nonoverlapping(
            vec_addr as *const u8,
            vec.as_mut_ptr() as *mut u8,
            vec_bytes,
        );

        // Process each element
        for item in vec.iter().take(els) {
            let (access, granter) = if item.v_from == crate::system::SELF {
                (CPF_WRITE, item.v_to)
            } else if item.v_to == crate::system::SELF {
                (CPF_READ, item.v_from)
            } else {
                return EINVAL; // each element must have exactly one SELF
            };

            let r = safecopy(
                caller,
                granter,
                (*caller).p_endpoint,
                item.v_gid,
                item.v_bytes as u64,
                item.v_offset as u64,
                item.v_addr,
                access,
            );
            if r != OK {
                return r;
            }
        }

        OK
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#priv::*;
    use crate::table::make_endpoint;
    use crate::table::proc_addr;
    use core::sync::atomic::AtomicU32;

    // ── Helper: set up a Proc at slot `proc_nr` with a Priv pointing to a
    //    grant table. The caller provides the grant buffer.
    //
    //    The Priv lives in an aligned static pool (8 slots, 2KB each) for
    //    pointer stability.

    /// Per-slot Priv storage pool — 8 slots × 2048 bytes each.
    /// (Priv is ~1KB, so 2KB per slot is generous.)
    const PRIV_SLOT_BYTES: usize = 2048;
    #[repr(C, align(64))]
    struct AlignedPool {
        data: [u8; PRIV_SLOT_BYTES * 8],
    }
    static mut PRIV_POOL: AlignedPool = AlignedPool {
        data: [0u8; PRIV_SLOT_BYTES * 8],
    };

    /// Reset the Priv at `slot` and set grant table fields.
    unsafe fn setup_priv(slot: i32, grant_table_ptr: u64, grant_entries: i32) -> *mut Priv {
        unsafe {
            // Use raw address of PRIV_POOL (first byte) plus slot offset
            let base = &raw mut PRIV_POOL as *mut u8;
            let p = base.add((slot as usize) * PRIV_SLOT_BYTES).cast::<Priv>();
            core::ptr::write_bytes(p.cast::<u8>(), 0, PRIV_SLOT_BYTES);
            (*p).s_grant_table = grant_table_ptr;
            (*p).s_grant_entries = grant_entries;
            (*p).s_flags = PrivFlags::empty();
            (*p).s_sig_mgr = i32::MIN;
            p
        }
    }

    /// Set up a Proc at `slot` with the given endpoint and priv pointer.
    unsafe fn setup_proc(slot: i32, ep: i32, priv_ptr: *mut Priv) -> *mut Proc {
        unsafe {
            let rp = proc_addr(slot);
            core::ptr::write_bytes(rp.cast::<u8>(), 0, size_of::<Proc>());
            (*rp).p_magic = PMAGIC;
            (*rp).p_endpoint = ep;
            (*rp).p_priv = priv_ptr;
            (*rp).p_rts_flags = AtomicU32::new(RtsFlags::empty().bits());
            rp
        }
    }

    /// Set up Proc + Priv in one call. `grant_buf` is caller-owned and
    /// must remain alive for the duration of the test.
    /// Initializes the process table on first call.
    unsafe fn setup_with_buf(
        slot: i32,
        ep: i32,
        grant_buf: *mut CpGrant,
        grant_entries: i32,
    ) -> (*mut Proc, *mut CpGrant) {
        unsafe {
            static mut INIT_DONE: bool = false;
            if !INIT_DONE {
                crate::table::proc_init();
                INIT_DONE = true;
            }
            let priv_ptr = setup_priv(slot, grant_buf as u64, grant_entries);
            let rp = setup_proc(slot, ep, priv_ptr);
            (rp, grant_buf)
        }
    }

    /// Build a direct CpGrant entry.
    fn make_direct_grant(flags: i32, who_to: i32, start: u64, len: usize) -> CpGrant {
        CpGrant {
            cp_flags: CPF_USED | CPF_VALID | CPF_DIRECT | flags,
            cp_u: CpUnion {
                cp_direct: CpDirect {
                    cp_who_to: who_to,
                    cp_start: start,
                    cp_len: len,
                    cp_reserved: [0u8; 8],
                },
            },
            cp_reserved: [0u8; 8],
        }
    }

    /// Build an indirect CpGrant entry.
    fn make_indirect_grant(who_to: i32, who_from: i32, grant: i32) -> CpGrant {
        CpGrant {
            cp_flags: CPF_USED | CPF_VALID | CPF_INDIRECT | CPF_READ,
            cp_u: CpUnion {
                cp_indirect: CpIndirect {
                    cp_who_to: who_to,
                    cp_who_from: who_from,
                    cp_grant: grant,
                    cp_reserved: [0u8; 8],
                },
            },
            cp_reserved: [0u8; 8],
        }
    }

    /// Build a magic CpGrant entry.
    fn make_magic_grant(flags: i32, who_from: i32, who_to: i32, start: u64, len: usize) -> CpGrant {
        CpGrant {
            cp_flags: CPF_USED | CPF_VALID | CPF_MAGIC | flags,
            cp_u: CpUnion {
                cp_magic: CpMagic {
                    cp_who_from: who_from,
                    cp_who_to: who_to,
                    cp_start: start,
                    cp_len: len,
                    cp_reserved: [0u8; 8],
                },
            },
            cp_reserved: [0u8; 8],
        }
    }

    /// Aligned byte buffer that can hold up to 128 CpGrant entries plus extra data.
    #[repr(C, align(8))]
    struct GrantBuf {
        data: [u8; 48 * 128],
    }

    /// Helper: create a properly sized grant buffer on the stack.
    macro_rules! grant_buf {
        ($buf:ident, $gp:ident, $n:expr) => {
            let mut $buf = GrantBuf {
                data: [0u8; 48 * 128],
            };
            let $gp = (&raw mut $buf.data).cast::<CpGrant>();
        };
    }

    // ── Constants ────────────────────────────────────────────────────────

    #[test]
    fn test_grant_constants() {
        assert_eq!(MAX_INDIRECT_DEPTH, 5);
        assert_eq!(SCPVEC_NR, 64);
        assert_eq!(GRANT_INVALID, -1);
    }

    #[test]
    fn test_grant_valid() {
        assert!(!grant_valid(-1));
        assert!(grant_valid(0));
        assert!(grant_valid(100));
    }

    #[test]
    fn test_cpf_flags() {
        assert_eq!(CPF_READ, 0x000001);
        assert_eq!(CPF_WRITE, 0x000002);
        assert_eq!(CPF_TRY, 0x000010);
        assert_eq!(CPF_USED, 0x000100);
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

    // ── Invalid arguments ────────────────────────────────────────────────

    #[test]
    fn test_verify_grant_invalid_granter() {
        unsafe {
            let r = verify_grant(-999, 0, 0, 10, CPF_READ, 0);
            assert!(r.is_err());
        }
    }

    #[test]
    fn test_verify_grant_invalid_grant_id() {
        unsafe {
            let r = verify_grant(0, 0, -1, 10, CPF_READ, 0);
            assert!(r.is_err());
        }
    }

    #[test]
    fn test_verify_grant_bad_grant_id_negative() {
        unsafe {
            let ep = make_endpoint(0, 0);
            let r = verify_grant(ep, ep, GRANT_INVALID, 10, CPF_READ, 0);
            assert!(r.is_err(), "GRANT_INVALID should be rejected");
        }
    }

    #[test]
    fn test_verify_grant_no_grant_table() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 0);
            let r = verify_grant(ep, ep, 0, 10, CPF_READ, 0);
            assert!(r.is_err(), "no grant entries should fail");
        }
    }

    #[test]
    fn test_verify_grant_grant_id_out_of_range() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 5);
            let r = verify_grant(ep, ep, 5, 10, CPF_READ, 0);
            assert!(r.is_err(), "grant_id >= entries should fail");
        }
    }

    #[test]
    fn test_verify_grant_not_used() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            // Write grant entry WITHOUT CPF_USED
            *grant_ptr.add(0) = CpGrant {
                cp_flags: CPF_VALID | CPF_DIRECT | CPF_READ,
                cp_u: CpUnion {
                    cp_direct: CpDirect {
                        cp_who_to: ep,
                        cp_start: 0x1000,
                        cp_len: 4096,
                        cp_reserved: [0u8; 8],
                    },
                },
                cp_reserved: [0u8; 8],
            };
            let r = verify_grant(ep, ep, 0, 10, CPF_READ, 0);
            assert!(r.is_err(), "missing CPF_USED should fail");
        }
    }

    #[test]
    fn test_verify_grant_invalid_flags() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            // Grant entry with no DIRECT/INDIRECT/MAGIC flag
            *grant_ptr.add(0) = CpGrant {
                cp_flags: CPF_USED | CPF_VALID | CPF_READ,
                cp_u: CpUnion {
                    cp_direct: CpDirect {
                        cp_who_to: ep,
                        cp_start: 0x1000,
                        cp_len: 4096,
                        cp_reserved: [0u8; 8],
                    },
                },
                cp_reserved: [0u8; 8],
            };
            let r = verify_grant(ep, ep, 0, 10, CPF_READ, 0);
            assert!(r.is_err(), "no type flag should return EPERM");
        }
    }

    // ── Direct grant ─────────────────────────────────────────────────────

    #[test]
    fn test_direct_grant_valid() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ, ep, 0x1000, 4096);
            let r = verify_grant(ep, ep, 0, 256, CPF_READ, 0);
            assert_eq!(
                r,
                Ok((0x1000, ep, CPF_DIRECT | CPF_READ | CPF_USED | CPF_VALID))
            );
        }
    }

    #[test]
    fn test_direct_grant_nonzero_offset() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ, ep, 0x1000, 4096);
            let r = verify_grant(ep, ep, 0, 256, CPF_READ, 0x800);
            assert_eq!(
                r,
                Ok((0x1800, ep, CPF_DIRECT | CPF_READ | CPF_USED | CPF_VALID))
            );
        }
    }

    #[test]
    fn test_direct_grant_offset_at_end() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ, ep, 0x1000, 4096);
            let r = verify_grant(ep, ep, 0, 256, CPF_READ, 3840);
            assert_eq!(
                r,
                Ok((
                    0x1000 + 3840,
                    ep,
                    CPF_DIRECT | CPF_READ | CPF_USED | CPF_VALID
                ))
            );
        }
    }

    #[test]
    fn test_direct_grant_out_of_range() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ, ep, 0x1000, 4096);
            let r = verify_grant(ep, ep, 0, 1, CPF_READ, 4096);
            assert!(r.is_err(), "offset past end should fail");
        }
    }

    #[test]
    fn test_direct_grant_any_grantee() {
        unsafe {
            let ep = make_endpoint(0, 0);
            let any_ep = crate::system::NONE;
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ, any_ep, 0x1000, 4096);
            let r = verify_grant(ep, any_ep, 0, 256, CPF_READ, 0);
            assert!(r.is_ok(), "ANY grantee should allow any caller");
        }
    }

    #[test]
    fn test_direct_grant_wrong_grantee() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ, 42, 0x1000, 4096);
            let r = verify_grant(ep, ep, 0, 256, CPF_READ, 0);
            assert!(r.is_err(), "wrong grantee should fail");
        }
    }

    #[test]
    fn test_direct_grant_write_access() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ, ep, 0x1000, 4096);
            let r = verify_grant(ep, ep, 0, 256, CPF_WRITE, 0);
            assert!(r.is_err(), "write access unset should fail");
        }
    }

    #[test]
    fn test_direct_grant_read_write_access() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ | CPF_WRITE, ep, 0x1000, 4096);
            let r = verify_grant(ep, ep, 0, 256, CPF_WRITE, 0);
            assert_eq!(
                r,
                Ok((
                    0x1000,
                    ep,
                    CPF_DIRECT | CPF_READ | CPF_WRITE | CPF_USED | CPF_VALID
                ))
            );
        }
    }

    // ── Indirect grant ───────────────────────────────────────────────────

    #[test]
    fn test_indirect_grant_single_hop() {
        unsafe {
            let ep_a = make_endpoint(0, 0);
            let ep_b = make_endpoint(0, 1);

            #[repr(C, align(8))]
            struct Align8 {
                data: [u8; 48 * 32],
            }
            let mut raw = Align8 {
                data: [0u8; 48 * 32],
            };
            let base = raw.data.as_mut_ptr() as *mut CpGrant;

            {
                let rp = proc_addr(0);
                core::ptr::write_bytes(rp.cast::<u8>(), 0, size_of::<Proc>());
                (*rp).p_magic = PMAGIC;
                (*rp).p_endpoint = ep_a;
                (*rp).p_priv = setup_priv(0, base as u64, 16);
                (*rp).p_rts_flags = AtomicU32::new(RtsFlags::empty().bits());
            }
            {
                let rp = proc_addr(1);
                core::ptr::write_bytes(rp.cast::<u8>(), 0, size_of::<Proc>());
                (*rp).p_magic = PMAGIC;
                (*rp).p_endpoint = ep_b;
                (*rp).p_priv = setup_priv(1, base.add(16) as u64, 16);
                (*rp).p_rts_flags = AtomicU32::new(RtsFlags::empty().bits());
            }

            // A[0]: indirect — original grantee (ep_b) may traverse to B's grant #2
            *base.add(0) = make_indirect_grant(ep_b, ep_b, 2);
            // B[2]: direct — after chain, grantee becomes the old granter (ep_a)
            *base.add(16 + 2) = make_direct_grant(CPF_READ, ep_a, 0x2000, 1024);

            let r = verify_grant(ep_a, ep_b, 0, 64, CPF_READ, 0);
            assert_eq!(
                r,
                Ok((0x2000, ep_b, CPF_DIRECT | CPF_READ | CPF_USED | CPF_VALID))
            );
        }
    }

    #[test]
    fn test_indirect_grant_max_depth() {
        unsafe {
            let ep0 = make_endpoint(0, 0);
            let ep1 = make_endpoint(0, 1);
            let ep2 = make_endpoint(0, 2);
            let ep3 = make_endpoint(0, 3);
            let ep4 = make_endpoint(0, 4);
            let ep5 = make_endpoint(0, 5);
            let ep6 = make_endpoint(0, 6);

            let eps = [ep0, ep1, ep2, ep3, ep4, ep5, ep6];
            grant_buf!(_gb, base, 128);
            for (i, &ep) in eps.iter().enumerate() {
                setup_with_buf(i as i32, ep, base.add(i * 16), 16);
            }

            // Each indirect grant's who_to follows C semantics:
            // after the chain, grantee becomes the old granter.
            for i in 0..6 {
                let allowed = if i == 0 { ep6 } else { eps[i - 1] };
                *base.add(i * 16) = make_indirect_grant(allowed, eps[i + 1], 0);
            }
            *base.add(6 * 16) = make_direct_grant(CPF_READ, eps[5], 0x3000, 512);

            let r = verify_grant(ep0, ep6, 0, 64, CPF_READ, 0);
            assert_eq!(
                r,
                Err(ELOOP),
                "chain exceeding MAX_INDIRECT_DEPTH should return ELOOP"
            );
        }
    }

    #[test]
    fn test_indirect_grant_wrong_indirect_grantee() {
        unsafe {
            let ep_a = make_endpoint(0, 0);
            let ep_b = make_endpoint(0, 1);
            let ep_c = make_endpoint(0, 2);
            grant_buf!(_gb, base, 32);
            setup_with_buf(0, ep_a, base, 16);
            setup_with_buf(2, ep_c, base.add(2 * 16), 16);

            // A's grant 0 is indirect, specifies who_to = ep_c (not ep_b)
            *base.add(0) = make_indirect_grant(ep_c, ep_c, 0);
            *base.add(2 * 16) = make_direct_grant(CPF_READ, ep_c, 0x4000, 256);

            let r = verify_grant(ep_a, ep_b, 0, 64, CPF_READ, 0);
            assert!(r.is_err(), "wrong indirect grantee should fail");
        }
    }

    // ── Magic grant ──────────────────────────────────────────────────────

    #[test]
    fn test_magic_grant_valid() {
        unsafe {
            let ep = make_endpoint(0, 1); // VFS (slot 1, gen 0)
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(1, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_magic_grant(CPF_READ, ep, ep, 0x5000, 2048);
            let r = verify_grant(ep, ep, 0, 128, CPF_READ, 0);
            assert_eq!(
                r,
                Ok((0x5000, ep, CPF_MAGIC | CPF_READ | CPF_USED | CPF_VALID))
            );
        }
    }

    #[test]
    fn test_magic_grant_any_grantee() {
        unsafe {
            let ep = make_endpoint(0, 1); // VFS (slot 1, gen 0)
            let any_ep = crate::system::NONE;
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(1, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_magic_grant(CPF_READ, ep, any_ep, 0x5000, 2048);
            let r = verify_grant(ep, any_ep, 0, 128, CPF_READ, 0);
            assert!(r.is_ok(), "ANY grantee on magic grant should pass");
        }
    }

    #[test]
    fn test_magic_grant_wrong_grantee() {
        unsafe {
            let ep = make_endpoint(0, 1); // VFS (slot 1, gen 0)
            grant_buf!(_gb, grant_ptr, 16);
            setup_with_buf(1, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_magic_grant(CPF_READ, ep, 42, 0x5000, 2048);
            let r = verify_grant(ep, ep, 0, 128, CPF_READ, 0);
            assert!(r.is_err(), "wrong grantee on magic grant should fail");
        }
    }

    #[test]
    fn test_magic_grant_returns_who_from() {
        unsafe {
            let ep_a = make_endpoint(0, 1); // VFS slot 1 (must own the magic grant)
            let ep_b = make_endpoint(0, 2); // Other process at slot 2 (the "real" memory owner)
            grant_buf!(_gb, base, 32);
            setup_with_buf(1, ep_a, base, 16);
            setup_with_buf(2, ep_b, base.add(16), 16);

            // Magic grant where cp_who_from = ep_b (identity is ep_b, not ep_a)
            *base.add(0) = make_magic_grant(CPF_READ, ep_b, ep_a, 0x5000, 2048);
            let r = verify_grant(ep_a, ep_a, 0, 128, CPF_READ, 0);
            assert_eq!(
                r,
                Ok((0x5000, ep_b, CPF_MAGIC | CPF_READ | CPF_USED | CPF_VALID))
            );
        }
    }

    // ── safecopy ─────────────────────────────────────────────────────────

    #[test]
    fn test_safecopy_none_granter_fails() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, _grant) = setup_with_buf(0, ep, grant_ptr, 16);
            let r = safecopy(rp, crate::system::NONE, ep, 0, 16, 0, 0x7000, CPF_READ);
            assert_eq!(r, EFAULT_SRC);
        }
    }

    #[test]
    fn test_safecopy_none_grantee_fails() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, _grant) = setup_with_buf(0, ep, grant_ptr, 16);
            let r = safecopy(rp, ep, crate::system::NONE, 0, 16, 0, 0x7000, CPF_READ);
            assert_eq!(r, EFAULT_SRC);
        }
    }

    #[test]
    fn test_safecopy_zero_bytes() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, grant) = setup_with_buf(0, ep, grant_ptr, 16);
            *grant.add(0) = make_direct_grant(CPF_READ, ep, 0x8000, 1024);
            let r = safecopy(rp, ep, ep, 0, 0, 0, 0x9000, CPF_READ);
            assert_eq!(r, OK, "zero-byte copy should succeed");
        }
    }

    // ── do_safecopy_from / do_safecopy_to ────────────────────────────────

    fn build_safecopy_msg(
        granter: i32,
        grant_id: i32,
        offset: u64,
        addr: u64,
        bytes: u64,
    ) -> [u8; MESSAGE_SIZE] {
        let mut msg = [0u8; MESSAGE_SIZE];
        msg[0..4].copy_from_slice(&0i32.to_ne_bytes()); // call_nr
        msg[8..12].copy_from_slice(&granter.to_ne_bytes());
        msg[12..16].copy_from_slice(&grant_id.to_ne_bytes());
        msg[16..24].copy_from_slice(&offset.to_ne_bytes());
        msg[24..32].copy_from_slice(&addr.to_ne_bytes());
        msg[32..40].copy_from_slice(&bytes.to_ne_bytes());
        msg
    }

    #[test]
    fn test_do_safecopy_from_valid() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, grant) = setup_with_buf(0, ep, grant_ptr, 16);
            // Copy buffer at byte 1536+ (past the 32-entry grant table)
            let base = _gb.data.as_mut_ptr() as u64;
            let copy_off = 48 * 32; // start of free area after 32 grant entries
            *grant.add(0) = make_direct_grant(CPF_READ, ep, base + copy_off, 64);
            let msg = build_safecopy_msg(ep, 0, 0, base + copy_off + 64, 16);
            let r = do_safecopy_from(rp, &msg);
            assert_eq!(r, OK);
        }
    }

    #[test]
    fn test_do_safecopy_to_valid() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, grant) = setup_with_buf(0, ep, grant_ptr, 16);
            let base = _gb.data.as_mut_ptr() as u64;
            let copy_off = 48 * 32;
            *grant.add(0) = make_direct_grant(CPF_READ | CPF_WRITE, ep, base + copy_off + 64, 64);
            let msg = build_safecopy_msg(ep, 0, 0, base + copy_off, 16);
            let r = do_safecopy_to(rp, &msg);
            assert_eq!(r, OK);
        }
    }

    #[test]
    fn test_do_safecopy_from_out_of_range() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, _grant) = setup_with_buf(0, ep, grant_ptr, 16);
            *grant_ptr.add(0) = make_direct_grant(CPF_READ, ep, 0x1000, 64);
            let msg = build_safecopy_msg(ep, 0, 64, _gb.data.as_mut_ptr().add(48 * 32) as u64, 1);
            let r = do_safecopy_from(rp, &msg);
            assert!(r != OK, "out-of-range should fail");
        }
    }

    // ── do_vsafecopy ─────────────────────────────────────────────────────

    fn build_vsafecpy_msg(vec_addr: u64, vec_size: usize) -> [u8; MESSAGE_SIZE] {
        let mut msg = [0u8; MESSAGE_SIZE];
        msg[8..16].copy_from_slice(&vec_addr.to_ne_bytes());
        msg[16..24].copy_from_slice(&(vec_size as u64).to_ne_bytes());
        msg
    }

    #[test]
    fn test_do_vsafecopy_zero_elements() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, _grant) = setup_with_buf(0, ep, grant_ptr, 16);
            let msg = build_vsafecpy_msg(0, 0);
            let r = do_vsafecopy(rp, &msg);
            assert_eq!(r, OK, "zero elements should succeed");
        }
    }

    #[test]
    fn test_do_vsafecopy_no_self_fails() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, grant) = setup_with_buf(0, ep, grant_ptr, 16);
            *grant.add(0) = make_direct_grant(CPF_READ, ep, _gb.data.as_ptr() as u64, 256);

            let vec = [VscpVec {
                v_from: 100,
                v_to: 101,
                v_gid: 0,
                v_offset: 0,
                v_addr: 0xD000,
                v_bytes: 64,
            }];
            let vec_addr = &vec as *const VscpVec as u64;
            let msg = build_vsafecpy_msg(vec_addr, 1);
            let r = do_vsafecopy(rp, &msg);
            assert_eq!(r, EINVAL, "no SELF element should fail");
        }
    }

    #[test]
    fn test_do_vsafecopy_single_element() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 16);
            let (rp, grant) = setup_with_buf(0, ep, grant_ptr, 16);
            let base = _gb.data.as_mut_ptr() as u64;
            let copy_off = 48 * 32;
            *grant.add(0) = make_direct_grant(CPF_READ, ep, base + copy_off, 256);

            let vec = [VscpVec {
                v_from: ep,
                v_to: crate::system::SELF,
                v_gid: 0,
                v_offset: 0,
                v_addr: base + copy_off + 128,
                v_bytes: 64,
            }];
            let vec_addr = &vec as *const VscpVec as u64;
            let msg = build_vsafecpy_msg(vec_addr, 1);
            let r = do_vsafecopy(rp, &msg);
            assert_eq!(r, OK, "single vsafecopy element should succeed");
        }
    }

    #[test]
    fn test_do_vsafecopy_multi_element() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 32);
            let (rp, grant) = setup_with_buf(0, ep, grant_ptr, 32);
            let base = _gb.data.as_mut_ptr() as u64;
            let copy_off = 48 * 32; // past 32-entry grant table (1536)
            *grant.add(0) = make_direct_grant(CPF_READ, ep, base + copy_off, 512);
            *grant.add(1) = make_direct_grant(CPF_READ | CPF_WRITE, ep, base + copy_off + 256, 256);

            let vec = [
                VscpVec {
                    v_from: ep,
                    v_to: crate::system::SELF,
                    v_gid: 0,
                    v_offset: 0,
                    v_addr: base + copy_off + 64,
                    v_bytes: 64,
                },
                VscpVec {
                    v_from: crate::system::SELF,
                    v_to: ep,
                    v_gid: 1,
                    v_offset: 0,
                    v_addr: base + copy_off + 128,
                    v_bytes: 128,
                },
            ];
            let vec_addr = &vec as *const VscpVec as u64;
            let msg = build_vsafecpy_msg(vec_addr, 2);
            let r = do_vsafecopy(rp, &msg);
            assert_eq!(r, OK, "multi-element vsafecopy should succeed");
        }
    }

    #[test]
    fn test_do_vsafecopy_element_fails_stops() {
        unsafe {
            let ep = make_endpoint(0, 0);
            grant_buf!(_gb, grant_ptr, 32);
            let (rp, grant) = setup_with_buf(0, ep, grant_ptr, 32);
            let base = _gb.data.as_mut_ptr() as u64;
            let copy_off = 48 * 32;
            *grant.add(0) = make_direct_grant(CPF_READ, ep, base + copy_off, 64);
            *grant.add(1) = make_direct_grant(CPF_READ, ep, base + copy_off + 256, 256);

            let vec = [
                VscpVec {
                    v_from: ep,
                    v_to: crate::system::SELF,
                    v_gid: 0,
                    v_offset: 0,
                    v_addr: base + copy_off + 128,
                    v_bytes: 64,
                },
                VscpVec {
                    v_from: ep,
                    v_to: crate::system::SELF,
                    v_gid: 1,
                    v_offset: 256,
                    v_addr: base + copy_off + 512,
                    v_bytes: 64,
                },
            ];
            let vec_addr = &vec as *const VscpVec as u64;
            let msg = build_vsafecpy_msg(vec_addr, 2);
            let r = do_vsafecopy(rp, &msg);
            assert!(r != OK, "failing element should return error");
        }
    }
}
