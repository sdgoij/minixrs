//! Minix userspace syscall layer (`minix-std`).
//!
//! Provides the low-level syscall interface used by all user-space programs:
//! - Endpoint types and validation (from `minix/include/minix/endpoint.h`)
//! - Error types and conversion from syscall return values
//! - IPC primitives: `send`, `receive`, `sendrec`, `notify`
//! - Grant table management for safe inter-process data transfer
//! - Process lifecycle: `fork`, `exit`, `waitpid`, `exec`, `getpid` (`process`)
//!
//! Built on top of `minix-rt` syscall wrappers.

#![no_std]

pub mod fs;
pub mod net;
pub mod process;
pub mod time;
pub mod vmem;

use core::cell::UnsafeCell;

// ═══════════════════════════════════════════════════════════════════════════
// Endpoint constants (from `minix/include/minix/endpoint.h`, `com.h`)
// ═══════════════════════════════════════════════════════════════════════════

/// Endpoint encoding constants (mirrors kernel's `table.rs`).
const EP_GENERATION_SHIFT: i32 = 15;
const EP_GENERATION_SIZE: i32 = 1 << EP_GENERATION_SHIFT;
const MAX_NR_TASKS: i32 = 1023;
const EP_MAX_GENERATION: i32 = i32::MAX / EP_GENERATION_SIZE - 1;

/// Special endpoint: match any process (for `receive`).
pub const ANY: i32 = 0x0000ffff;
/// Special endpoint: no process.
pub const NONE: i32 = 0x0000fffe;
/// Special endpoint: the calling process itself.
pub const SELF: i32 = 0x0000fffd;

/// Process Manager endpoint.
pub const PM_PROC_NR: i32 = 0;
/// Virtual File System endpoint.
pub const VFS_PROC_NR: i32 = 1;
/// Reincarnation Server endpoint.
pub const RS_PROC_NR: i32 = 2;
/// Virtual Memory endpoint.
pub const VM_PROC_NR: i32 = 8;
/// Data Store endpoint.
pub const DS_PROC_NR: i32 = 6;
/// Scheduler endpoint.
pub const SCHED_PROC_NR: i32 = 4;
/// TTY driver endpoint.
pub const TTY_PROC_NR: i32 = 5;
/// Clock task endpoint (kernel task, negative).
pub const CLOCK: i32 = -3;
/// System task endpoint (kernel task, negative).
pub const SYSTEM: i32 = -2;
/// Kernel task endpoint (kernel task, negative).
pub const KERNEL: i32 = -1;
/// Hardware interrupt endpoint (same as KERNEL).
pub const HARDWARE: i32 = -1;

/// Return true if `ep` is a valid endpoint (not NONE, generation in range).
pub fn is_ok_endpoint(ep: i32) -> bool {
    if ep == NONE {
        return false;
    }
    let generation = (ep + MAX_NR_TASKS) >> EP_GENERATION_SHIFT;
    (0..=EP_MAX_GENERATION).contains(&generation)
}

/// Return true if process number `nr` is a kernel task (negative slot).
pub fn is_kernel_nr(nr: i32) -> bool {
    nr < 0
}

/// Extract the process slot number from an endpoint.
///
/// This mirrors `_ENDPOINT_P(e)` from the C header and the kernel's
/// `endpoint_slot()` in `table.rs`.
pub fn endpoint_slot(ep: i32) -> i32 {
    ((ep + MAX_NR_TASKS) & (EP_GENERATION_SIZE - 1)) - MAX_NR_TASKS
}

// ═══════════════════════════════════════════════════════════════════════════
// Error types (from `minix/include/minix/errno.h`)
// ═══════════════════════════════════════════════════════════════════════════

/// A MINIX error code, wrapping a positive errno value.
///
/// MINIX syscalls return non-positive values: `0` or positive on success,
/// negative errno on failure. `from_syscall` converts by negating the
/// return value so the `MinixErr` stores a positive errno.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinixErr(pub i32);

impl MinixErr {
    /// Convert a raw syscall return value into a `Result`.
    ///
    /// MINIX convention:
    /// - `ret >= 0`: success, returns `Ok(ret)`
    /// - `ret < 0`:  failure, returns `Err(MinixErr(-ret))`
    pub fn from_syscall(ret: i32) -> Result<i32, MinixErr> {
        if ret >= 0 {
            Ok(ret)
        } else {
            Err(MinixErr(-ret))
        }
    }
}

// Error constants (negative errno values, matching MINIX C definitions).
pub const OK: i32 = 0;
pub const EPERM: i32 = -1;
pub const ENOENT: i32 = -2;
pub const ESRCH: i32 = -3;
pub const EINTR: i32 = -4;
pub const EIO: i32 = -5;
pub const ENXIO: i32 = -6;
pub const EAGAIN: i32 = -11;
pub const ENOMEM: i32 = -12;
pub const EACCES: i32 = -13;
pub const EFAULT: i32 = -14;
pub const EBUSY: i32 = -16;
pub const EEXIST: i32 = -17;
pub const ENODEV: i32 = -19;
pub const ENOTDIR: i32 = -20;
pub const EISDIR: i32 = -21;
pub const EINVAL: i32 = -22;
pub const ENOSPC: i32 = -28;
pub const EDOM: i32 = -33;
pub const ERANGE: i32 = -34;
pub const ENOSYS: i32 = -71;
pub const EDONTREPLY: i32 = -201;
pub const SUSPEND: i32 = -998;

// ═══════════════════════════════════════════════════════════════════════════
// IPC primitives
// ═══════════════════════════════════════════════════════════════════════════

/// Syscall number for SEND.
pub const SEND_CALL: u64 = 46;
/// Syscall number for RECEIVE.
pub const RECEIVE_CALL: u64 = 47;
/// Syscall number for SENDREC.
pub const SENDREC_CALL: u64 = 48;
/// Syscall number for NOTIFY.
pub const NOTIFY_CALL: u64 = 49;

/// Size of an IPC message in bytes (matches kernel's `Message`).
pub const MESSAGE_SIZE: usize = 64;

/// A fixed-size IPC message buffer.
///
/// The first 4 bytes (`msg[0..4]`) contain the destination endpoint for SEND
/// or the source endpoint for RECEIVE, matching the kernel's `do_sync_ipc`.
pub type Message = [u8; MESSAGE_SIZE];

/// Send a message to `dest`.
/// Send a message to `dest`.  Non-blocking; the target must be ready to
/// receive or the call will fail.
///
/// The destination endpoint must be placed in `msg[0..4]` (little-endian i32).
///
/// # Safety
///
/// `msg` must point to a valid `Message`. The syscall may fail if `dest`
/// is invalid or the target is not ready to receive.
pub unsafe fn send(dest: i32, msg: &Message) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let ret = minix_rt::syscall2(SEND_CALL, dest as u64, msg.as_ptr() as u64);
        return MinixErr::from_syscall(ret as i32).map(|_| ());
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (dest, msg);
        Err(MinixErr(ENOSYS))
    }
}

/// Receive a message from `src` (or `ANY`).
///
/// On success, returns the endpoint of the sender, and `msg` is filled with
/// the received message data.
///
/// # Safety
///
/// `msg` must point to a valid, mutable `Message`.
pub unsafe fn receive(src: i32, msg: &mut Message) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let ret = minix_rt::syscall2(RECEIVE_CALL, src as u64, msg.as_mut_ptr() as u64);
        return MinixErr::from_syscall(ret as i32);
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (src, msg);
        Err(MinixErr(ENOSYS))
    }
}

/// Combine send and receive in one atomic operation.
///
/// Sends the message to `dest`, then blocks until a reply is received.
/// On success, returns the endpoint of the replier.
///
/// # Safety
///
/// `msg` must point to a valid, mutable `Message`. The destination must
/// be placed in `msg[0..4]` before the call.
pub unsafe fn sendrec(dest: i32, msg: &mut Message) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let ret = minix_rt::syscall2(SENDREC_CALL, dest as u64, msg.as_mut_ptr() as u64);
        return MinixErr::from_syscall(ret as i32);
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (dest, msg);
        Err(MinixErr(ENOSYS))
    }
}

/// Send an asynchronous notification to `dest`.
///
/// Notifications carry no data payload; only the fact of notification is
/// delivered.
pub fn notify(dest: i32) -> Result<(), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let ret = minix_rt::syscall1(NOTIFY_CALL, dest as u64);
        return MinixErr::from_syscall(ret as i32).map(|_| ());
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = dest;
        Err(MinixErr(ENOSYS))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Grant table
// ═══════════════════════════════════════════════════════════════════════════

/// Number of grant entries in the table.
pub const NR_GRANTS: usize = 64;

/// Magic value written to `g_flags` when a grant entry is in use.
pub const GRANT_MAGIC: u32 = 0x1A2B3C4D;

/// A single grant table entry.
///
/// Matches the layout from `GrantTable.c` for ABI compatibility.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct GrantEntry {
    /// Flags / magic value. 0 means the entry is free.
    pub g_flags: u32,
    /// Endpoint that created the grant.
    pub g_caller: i32,
    /// Endpoint that is allowed to access the grant.
    pub g_callee: i32,
    /// Grant identifier / index.
    pub g_grant: u32,
}

/// Per-process grant table for safe inter-process data transfer.
///
/// Provides `alloc`, `free`, `get`, and `clear` operations on a fixed-size
/// array of `NR_GRANTS` entries. Uses `UnsafeCell` for interior mutability.
pub struct GrantTable {
    entries: UnsafeCell<[GrantEntry; NR_GRANTS]>,
}

// GrantTable is Send + Sync because all access is mediated through &mut
// methods or safe &-method bounds.
unsafe impl Send for GrantTable {}
unsafe impl Sync for GrantTable {}

impl GrantTable {
    /// Create a new empty grant table (all entries zeroed / free).
    pub fn new() -> Self {
        GrantTable {
            entries: UnsafeCell::new(
                [GrantEntry {
                    g_flags: 0,
                    g_caller: 0,
                    g_callee: 0,
                    g_grant: 0,
                }; NR_GRANTS],
            ),
        }
    }

    /// Allocate a new grant entry for `caller` → `callee`.
    ///
    /// Returns `Some(grant_id)` on success, or `None` if the table is full.
    pub fn alloc(&mut self, caller: i32, callee: i32) -> Option<u32> {
        // SAFETY: We have &mut self, so no other reference exists.
        let entries = unsafe { &mut *self.entries.get() };
        for (i, entry) in entries.iter_mut().enumerate() {
            if entry.g_flags == 0 {
                entry.g_flags = GRANT_MAGIC;
                entry.g_caller = caller;
                entry.g_callee = callee;
                entry.g_grant = i as u32;
                return Some(i as u32);
            }
        }
        None
    }

    /// Free a previously allocated grant entry.
    ///
    /// Has no effect if `grant` is out of range or already free.
    pub fn free(&mut self, grant: u32) {
        // SAFETY: We have &mut self, so no other reference exists.
        let entries = unsafe { &mut *self.entries.get() };
        if let Some(entry) = entries.get_mut(grant as usize) {
            entry.g_flags = 0;
            entry.g_caller = 0;
            entry.g_callee = 0;
            entry.g_grant = 0;
        }
    }

    /// Get a reference to a grant entry, or `None` if out of range.
    pub fn get(&self, grant: u32) -> Option<&GrantEntry> {
        // SAFETY: We return a shared reference derived from the UnsafeCell
        // and never expose mutable references through &self methods.
        let entries = unsafe { &*self.entries.get() };
        entries.get(grant as usize).filter(|e| e.g_flags != 0)
    }

    /// Clear all grant entries (reset table to empty state).
    pub fn clear(&mut self) {
        // SAFETY: We have &mut self, so no other reference exists.
        let entries = unsafe { &mut *self.entries.get() };
        for entry in entries.iter_mut() {
            entry.g_flags = 0;
            entry.g_caller = 0;
            entry.g_callee = 0;
            entry.g_grant = 0;
        }
    }

    /// Allocate a grant for the VFS→FS pattern: VFS (caller) grants
    /// access to FS (callee) for a user-space pathname buffer.
    ///
    /// Returns the grant ID, or `None` if the table is full.
    ///
    /// In MINIX C this calls `cpf_grant_magic()` which creates a
    /// magic grant that the FS can use with `sys_safecopyfrom` to
    /// read the path string from VFS's memory.
    pub fn cpf_grant_magic(&mut self, caller: i32, callee: i32) -> Option<u32> {
        self.alloc(caller, callee)
    }

    /// Allocate a direct grant for data transfer.
    ///
    /// Same as `cpf_grant_magic` but named for the direct-grant
    /// pattern (callee reads/writes caller's buffer).
    pub fn cpf_grant_direct(&mut self, caller: i32, callee: i32) -> Option<u32> {
        self.alloc(caller, callee)
    }

    /// Revoke (free) a previously allocated grant.
    ///
    /// In MINIX C this calls `cpf_revoke()`.  Has no effect if
    /// `grant` is out of range or already free.
    pub fn cpf_revoke(&mut self, grant: u32) {
        self.free(grant);
    }
}

impl Default for GrantTable {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── Endpoint validation ──────────────────────────────────────────────

    #[test]
    fn test_any_none_self_constants() {
        assert_eq!(ANY, 0x0000ffff);
        assert_eq!(NONE, 0x0000fffe);
        assert_eq!(SELF, 0x0000fffd);
    }

    #[test]
    fn test_proc_nr_constants() {
        assert_eq!(PM_PROC_NR, 0);
        assert_eq!(VFS_PROC_NR, 1);
        assert_eq!(RS_PROC_NR, 2);
        assert_eq!(VM_PROC_NR, 8);
        assert_eq!(DS_PROC_NR, 6);
        assert_eq!(SCHED_PROC_NR, 4);
        assert_eq!(TTY_PROC_NR, 5);
    }

    #[test]
    fn test_kernel_endpoints() {
        assert_eq!(CLOCK, -3);
        assert_eq!(SYSTEM, -2);
        assert_eq!(KERNEL, -1);
        assert_eq!(HARDWARE, -1);
    }

    #[test]
    fn test_is_ok_endpoint() {
        // Valid endpoints
        assert!(is_ok_endpoint(PM_PROC_NR));
        assert!(is_ok_endpoint(VFS_PROC_NR));
        assert!(is_ok_endpoint(CLOCK));
        assert!(is_ok_endpoint(SYSTEM));
        assert!(is_ok_endpoint(KERNEL));

        // Special endpoints
        // ANY and SELF have generation 0 and valid slots.
        assert!(is_ok_endpoint(ANY));
        assert!(is_ok_endpoint(SELF));

        // NONE is never valid
        assert!(!is_ok_endpoint(NONE));
    }

    #[test]
    fn test_is_kernel_nr() {
        assert!(is_kernel_nr(-1));
        assert!(is_kernel_nr(-2));
        assert!(is_kernel_nr(-3));
        assert!(!is_kernel_nr(0));
        assert!(!is_kernel_nr(1));
        assert!(!is_kernel_nr(10));
    }

    #[test]
    fn test_endpoint_slot() {
        // Generation-0 endpoints have endpoint == slot
        assert_eq!(endpoint_slot(0), 0);
        assert_eq!(endpoint_slot(1), 1);
        assert_eq!(endpoint_slot(8), 8);

        // Negative endpoints are kernel tasks
        assert_eq!(endpoint_slot(-1), -1);
        assert_eq!(endpoint_slot(-2), -2);
        assert_eq!(endpoint_slot(-3), -3);

        // Endpoint with generation
        let ep = (1 << EP_GENERATION_SHIFT) | 3; // gen=1, slot=3
        assert_eq!(endpoint_slot(ep), 3);

        // Special endpoints map to negative slots
        assert_eq!(endpoint_slot(ANY), -1);
        assert_eq!(endpoint_slot(NONE), -2);
        assert_eq!(endpoint_slot(SELF), -3);
    }

    // ── Error conversion ────────────────────────────────────────────────

    #[test]
    fn test_error_constants() {
        assert_eq!(OK, 0);
        assert_eq!(EPERM, -1);
        assert_eq!(ENOENT, -2);
        assert_eq!(EINVAL, -22);
        assert_eq!(ENOSYS, -71);
        assert_eq!(SUSPEND, -998);
    }

    #[test]
    fn test_from_syscall_ok_zero() {
        let result = MinixErr::from_syscall(0);
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn test_from_syscall_ok_positive() {
        let result = MinixErr::from_syscall(42);
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn test_from_syscall_err_negative() {
        let result = MinixErr::from_syscall(-1);
        assert_eq!(result, Err(MinixErr(1)));
    }

    #[test]
    fn test_from_syscall_err_eperm() {
        // Syscall returns -EPERM (which is 1 negated)
        let result = MinixErr::from_syscall(EPERM);
        assert_eq!(result, Err(MinixErr(-EPERM)));
    }

    #[test]
    fn test_from_syscall_err_enomem() {
        let result = MinixErr::from_syscall(ENOMEM);
        assert_eq!(result, Err(MinixErr(-ENOMEM)));
    }

    #[test]
    fn test_minix_err_debug_clone_copy() {
        fn assert_debug<T: core::fmt::Debug>() {}
        fn assert_clone_copy<T: Clone + Copy>() {}
        assert_debug::<MinixErr>();
        assert_clone_copy::<MinixErr>();
    }

    // ── Grant table lifecycle ───────────────────────────────────────────

    #[test]
    fn test_grant_table_new_is_empty() {
        let table = GrantTable::new();
        // get() returns None for any unallocated grant
        assert!(table.get(0).is_none());
        assert!(table.get(63).is_none());
    }

    #[test]
    fn test_grant_table_alloc_returns_id() {
        let mut table = GrantTable::new();
        let id = table.alloc(100, 200);
        assert!(id.is_some());
        assert_eq!(id.unwrap(), 0); // first free slot
    }

    #[test]
    fn test_grant_table_alloc_entry_contents() {
        let mut table = GrantTable::new();
        let id = table.alloc(42, 99).unwrap();
        let entry = table.get(id).unwrap();
        assert_eq!(entry.g_flags, GRANT_MAGIC);
        assert_eq!(entry.g_caller, 42);
        assert_eq!(entry.g_callee, 99);
        assert_eq!(entry.g_grant, id);
    }

    #[test]
    fn test_grant_table_free_clears_entry() {
        let mut table = GrantTable::new();
        let id = table.alloc(1, 2).unwrap();
        assert!(table.get(id).is_some());
        table.free(id);
        assert!(table.get(id).is_none());
    }

    #[test]
    fn test_grant_table_free_reuses_slot() {
        let mut table = GrantTable::new();
        let id = table.alloc(1, 2).unwrap();
        table.free(id);
        let id2 = table.alloc(3, 4).unwrap();
        assert_eq!(id, id2); // same slot reused
    }

    #[test]
    fn test_grant_table_clear_resets_all() {
        let mut table = GrantTable::new();
        for i in 0..NR_GRANTS {
            assert!(table.alloc(i as i32, (i * 2) as i32).is_some());
        }
        // table is full
        assert!(table.alloc(999, 999).is_none());

        table.clear();
        // all should be free again
        for i in 0..NR_GRANTS {
            assert!(table.get(i as u32).is_none());
        }
        // can alloc again
        assert!(table.alloc(1, 2).is_some());
    }

    #[test]
    fn test_grant_table_full_returns_none() {
        let mut table = GrantTable::new();
        for i in 0..NR_GRANTS {
            assert!(table.alloc(i as i32, (i * 2) as i32).is_some());
        }
        assert!(table.alloc(999, 999).is_none());
    }

    #[test]
    fn test_grant_table_free_out_of_range() {
        let mut table = GrantTable::new();
        // Should not panic
        table.free(100);
        table.free(u32::MAX);
    }

    #[test]
    fn test_grant_table_get_out_of_range() {
        let table = GrantTable::new();
        assert!(table.get(100).is_none());
        assert!(table.get(u32::MAX).is_none());
    }

    #[test]
    fn test_grant_table_default() {
        let table: GrantTable = Default::default();
        assert!(table.get(0).is_none());
    }

    #[test]
    fn test_grant_table_entry_debug_repr() {
        fn assert_debug<T: core::fmt::Debug>() {}
        assert_debug::<GrantEntry>();
        let entry = GrantEntry {
            g_flags: GRANT_MAGIC,
            g_caller: 1,
            g_callee: 2,
            g_grant: 0,
        };
        assert_ne!(entry.g_flags, 0);
    }

    #[test]
    fn test_cpf_grant_magic_and_revoke() {
        let mut table = GrantTable::new();
        let id = table.cpf_grant_magic(10, 20);
        assert!(id.is_some());
        let entry = table.get(id.unwrap()).unwrap();
        assert_eq!(entry.g_caller, 10);
        assert_eq!(entry.g_callee, 20);
        // Revoke and verify freed
        table.cpf_revoke(id.unwrap());
        assert!(table.get(id.unwrap()).is_none());
    }

    #[test]
    fn test_cpf_grant_direct() {
        let mut table = GrantTable::new();
        let id = table.cpf_grant_direct(VFS_PROC_NR, VM_PROC_NR);
        assert!(id.is_some());
        let entry = table.get(id.unwrap()).unwrap();
        assert_eq!(entry.g_caller, VFS_PROC_NR);
        assert_eq!(entry.g_callee, VM_PROC_NR);
    }

    #[test]
    fn test_cpf_revoke_out_of_range() {
        let mut table = GrantTable::new();
        // Should not panic
        table.cpf_revoke(100);
        table.cpf_revoke(u32::MAX);
    }

    #[test]
    fn test_ipc_syscall_numbers() {
        assert_eq!(SEND_CALL, 46);
        assert_eq!(RECEIVE_CALL, 47);
        assert_eq!(SENDREC_CALL, 48);
        assert_eq!(NOTIFY_CALL, 49);
    }

    #[test]
    fn test_message_size() {
        assert_eq!(MESSAGE_SIZE, 64);
    }

    #[test]
    fn test_message_type_size() {
        assert_eq!(core::mem::size_of::<Message>(), 64);
    }

    /// Verify that IPC function signatures compile with correct types.
    /// These do not call actual syscalls.
    #[test]
    fn test_ipc_fn_signatures() {
        fn _check_send(f: unsafe fn(i32, &Message) -> Result<(), MinixErr>) {
            let _ = f;
        }
        fn _check_receive(f: unsafe fn(i32, &mut Message) -> Result<i32, MinixErr>) {
            let _ = f;
        }
        fn _check_sendrec(f: unsafe fn(i32, &mut Message) -> Result<i32, MinixErr>) {
            let _ = f;
        }
        fn _check_notify(f: fn(i32) -> Result<(), MinixErr>) {
            let _ = f;
        }
        let _ = _check_send;
        let _ = _check_receive;
        let _ = _check_sendrec;
        let _ = _check_notify;
    }
}
