//! VFS grant table — per-process grant entries for safe IPC with FS servers.
//!
//! The VFS maintains a grant table (array of `CpGrant` entries) that is
//! registered with the kernel via `SYS_SETGRANT`.  When the VFS needs to
//! share a pathname or data buffer with a filesystem server, it allocates
//! a grant entry, fills in the address/size/access, sends the grant ID
//! in the request message, and revokes the grant after the reply.
//!
//! Ported from `minix/lib/libminixfs/grant.c` (cpf_grant_magic, cpf_grant_direct).

use arch_common::safecopies::{
    CPF_DIRECT, CPF_MAGIC, CPF_READ, CPF_USED, CPF_VALID, CPF_WRITE, CpDirect, CpGrant, CpMagic,
    CpUnion, GRANT_INVALID,
};

/// Number of grant entries in the VFS grant table.
pub const VFS_NR_GRANTS: usize = 64;

/// A free grant entry has `cp_flags == 0`.
///
/// # Safety
///
/// This static is only accessed from the single-threaded VFS server.
/// The address of this table must be registered with the kernel via
/// `SYS_SETGRANT` during VFS init so that the kernel can read grant
/// entries when an FS server calls `sys_safecopyfrom`.
static VFS_GRANT_TABLE: GrantTable = GrantTable::new();

/// Wrapper for a fixed-size array of `CpGrant` entries.
struct GrantTable {
    entries: core::cell::UnsafeCell<[CpGrant; VFS_NR_GRANTS]>,
}

unsafe impl Sync for GrantTable {}

impl GrantTable {
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
            entries: core::cell::UnsafeCell::new([ENTRY; VFS_NR_GRANTS]),
        }
    }

    /// Get a raw pointer to the grant table for SYS_SETGRANT.
    #[allow(dead_code)]
    fn as_ptr(&self) -> u64 {
        self.entries.get() as u64
    }

    /// Allocate a grant entry and fill it as a magic grant.
    ///
    /// Magic grants allow the callee (FS server) to use `sys_safecopyfrom`
    /// with this grant ID to read from the granter's address space at
    /// the given address, without the granter having a specific buffer
    /// pre-registered.
    ///
    /// Returns the grant ID, or `GRANT_INVALID` if the table is full.
    pub fn cpf_grant_magic(
        &self,
        who_from: i32,
        callee: i32,
        addr: u64,
        len: usize,
        access: i32,
    ) -> i32 {
        unsafe {
            let entries = &mut *self.entries.get();
            for (i, entry) in entries.iter_mut().enumerate() {
                if entry.cp_flags == 0 {
                    entry.cp_flags = CPF_USED | CPF_VALID | CPF_MAGIC | access;
                    entry.cp_u.cp_magic = CpMagic {
                        cp_who_from: who_from,
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

    /// Allocate a grant entry and fill it as a direct grant.
    ///
    /// Direct grants allow the callee to read/write the granter's buffer
    /// at the given address, up to `len` bytes.
    pub fn cpf_grant_direct(
        &self,
        _granter: i32,
        callee: i32,
        addr: u64,
        len: usize,
        access: i32,
    ) -> i32 {
        unsafe {
            let entries = &mut *self.entries.get();
            for (i, entry) in entries.iter_mut().enumerate() {
                if entry.cp_flags == 0 {
                    entry.cp_flags = CPF_USED | CPF_VALID | CPF_DIRECT | access;
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

    /// Revoke (free) a previously allocated grant entry.
    pub fn cpf_revoke(&self, grant_id: i32) {
        if grant_id < 0 || grant_id >= VFS_NR_GRANTS as i32 {
            return;
        }
        unsafe {
            let entries = &mut *self.entries.get();
            entries[grant_id as usize].cp_flags = 0;
        }
    }

    /// Register the grant table with the kernel via SYS_SETGRANT.
    ///
    /// Must be called once during VFS init, before any FS requests.
    /// On host (test mode), this is a no-op.
    pub fn register_with_kernel(&self) {
        #[cfg(target_os = "none")]
        {
            let addr = self.as_ptr();
            let nr = VFS_NR_GRANTS as i32;
            // Build SYS_SETGRANT (kernel call 34) message.
            // The kernel handler (do_setgrant) reads:
            //   msg[0..8]  = addr (u64)
            //   msg[8..12] = nr_entries (i32)
            let mut msg = [0u8; 64];
            msg[0..8].copy_from_slice(&addr.to_le_bytes());
            msg[8..12].copy_from_slice(&nr.to_le_bytes());
            let r = minix_rt::kernel_call(34, &mut msg);
            if r != 0 {
                #[cfg(target_os = "none")]
                minix_rt::write(2, b"vfs: setgrant failed\n");
                let _ = r;
            }
        }
        #[cfg(not(target_os = "none"))]
        {
            // No-op on host — no real kernel to register with.
        }
    }
}

/// Allocate a magic grant for a pathname buffer.
///
/// Convenience wrapper: `granter` is the VFS's own endpoint,
/// `callee` is the target FS server, `addr` is the path string
/// address in VFS space.
/// address in VFS space.
pub fn cpf_grant_magic(who_from: i32, callee: i32, addr: u64, len: usize) -> i32 {
    VFS_GRANT_TABLE.cpf_grant_magic(who_from, callee, addr, len, CPF_READ)
}

/// Allocate a magic grant with write access.
///
/// `who_from` is the endpoint of the process that owns the buffer memory,
/// `callee` is the target FS server. Use this when the FS will write data
/// into the buffer (e.g. FS writing to a user's stat buffer).
pub fn cpf_grant_magic_write(who_from: i32, callee: i32, addr: u64, len: usize) -> i32 {
    VFS_GRANT_TABLE.cpf_grant_magic(who_from, callee, addr, len, CPF_WRITE)
}

/// Allocate a direct grant for data transfer.
pub fn cpf_grant_direct(granter: i32, callee: i32, addr: u64, len: usize, write: bool) -> i32 {
    let access = if write { CPF_WRITE } else { CPF_READ };
    VFS_GRANT_TABLE.cpf_grant_direct(granter, callee, addr, len, access)
}

/// Revoke a previously allocated grant.
pub fn cpf_revoke(grant_id: i32) {
    VFS_GRANT_TABLE.cpf_revoke(grant_id);
}

/// Register the VFS grant table with the kernel.
pub fn vfs_grant_init() {
    VFS_GRANT_TABLE.register_with_kernel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpf_grant_magic_alloc_and_revoke() {
        let table = GrantTable::new();
        let id = table.cpf_grant_magic(1, 2, 0x1234, 256, CPF_READ);
        assert!(id >= 0);
        assert!(id < VFS_NR_GRANTS as i32);

        // Verify the entry was filled correctly
        unsafe {
            let entries = &*table.entries.get();
            let e = &entries[id as usize];
            assert!(e.cp_flags & CPF_USED != 0);
            assert!(e.cp_flags & CPF_VALID != 0);
            assert!(e.cp_flags & CPF_MAGIC != 0);
            assert!(e.cp_flags & CPF_READ != 0);
            assert_eq!(e.cp_u.cp_magic.cp_who_from, 1);
            assert_eq!(e.cp_u.cp_magic.cp_who_to, 2);
            assert_eq!(e.cp_u.cp_magic.cp_start, 0x1234);
            assert_eq!(e.cp_u.cp_magic.cp_len, 256);
        }

        // Revoke
        table.cpf_revoke(id);
        unsafe {
            let entries = &*table.entries.get();
            assert_eq!(entries[id as usize].cp_flags, 0);
        }
    }

    #[test]
    fn test_cpf_grant_direct_alloc() {
        let table = GrantTable::new();
        let id = table.cpf_grant_direct(1, 2, 0x5678, 512, CPF_WRITE);
        assert!(id >= 0);

        unsafe {
            let entries = &*table.entries.get();
            let e = &entries[id as usize];
            assert!(e.cp_flags & CPF_DIRECT != 0);
            assert!(e.cp_flags & CPF_WRITE != 0);
            assert_eq!(e.cp_u.cp_direct.cp_who_to, 2);
            assert_eq!(e.cp_u.cp_direct.cp_start, 0x5678);
            assert_eq!(e.cp_u.cp_direct.cp_len, 512);
        }
    }

    #[test]
    fn test_cpf_grant_table_full_returns_invalid() {
        let table = GrantTable::new();
        // Allocate all entries
        for i in 0..VFS_NR_GRANTS {
            let id = table.cpf_grant_magic(1, 2, 0, 0, CPF_READ);
            assert_eq!(id, i as i32);
        }
        // Next allocation should fail
        let id = table.cpf_grant_magic(1, 2, 0, 0, CPF_READ);
        assert_eq!(id, GRANT_INVALID);
    }

    #[test]
    fn test_cpf_revoke_out_of_range() {
        let table = GrantTable::new();
        // Should not panic
        table.cpf_revoke(-1);
        table.cpf_revoke(VFS_NR_GRANTS as i32);
        table.cpf_revoke(i32::MAX);
    }

    #[test]
    fn test_cpf_revoke_reuses_slot() {
        let table = GrantTable::new();
        let id1 = table.cpf_grant_magic(1, 2, 0, 0, CPF_READ);
        assert!(id1 >= 0);
        table.cpf_revoke(id1);
        let id2 = table.cpf_grant_magic(3, 4, 0, 0, CPF_READ);
        assert_eq!(id1, id2); // same slot reused
    }

    #[test]
    fn test_convenience_fns() {
        let id = cpf_grant_magic(1, 2, 0x1000, 64);
        assert!(id >= 0);
        cpf_revoke(id);

        let id = cpf_grant_direct(1, 2, 0x2000, 128, true);
        assert!(id >= 0);
        cpf_revoke(id);

        let id = cpf_grant_direct(1, 2, 0x3000, 256, false);
        assert!(id >= 0);
        cpf_revoke(id);
    }

    #[test]
    fn test_register_with_kernel_noop_on_host() {
        // Should not crash or panic on host
        let table = GrantTable::new();
        table.register_with_kernel();
    }
}
