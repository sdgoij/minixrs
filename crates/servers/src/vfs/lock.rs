//! VFS advisory file locking — ported from `minix/servers/vfs/lock.c`.
//!
//! Implements POSIX `fcntl(F_SETLK)`, `F_GETLK`, and `F_SETLKW` (blocking
//! lock).  Byte-range locks on regular files and block devices.  Read locks
//! are shared; write locks are exclusive.
//!
//! The lock table holds up to `NR_LOCKS` (8) entries.  `lock_revive()`
//! wakes all processes blocked on `FP_BLOCKED_ON_LOCK` — each retries its
//! request from scratch.

use crate::vfs::consts::*;
use crate::vfs::types::*;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, Ordering};

fn r2_i16(buf: &[u8; 64], off: usize) -> i16 {
    i16::from_le_bytes(buf[off..off + 2].try_into().unwrap_or([0; 2]))
}
fn w2_i16(buf: &mut [u8; 64], off: usize, val: i16) {
    buf[off..off + 2].copy_from_slice(&val.to_le_bytes());
}
fn r2_i32(buf: &[u8; 64], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}
fn r2_i64(buf: &[u8; 64], off: usize) -> i64 {
    i64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}
fn w2_i64(buf: &mut [u8; 64], off: usize, val: i64) {
    buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
}
fn w2_i32(buf: &mut [u8; 64], off: usize, val: i32) {
    buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
}

/// Returned by `check_lock` when a lock request cannot be granted.
pub struct LockConflict {
    pub lock_type: i16,
    pub lock_pid: i32,
    pub lock_first: i64,
    pub lock_last: i64,
}

struct LockTable(UnsafeCell<[FileLock; NR_LOCKS]>);
unsafe impl Sync for LockTable {}

impl LockTable {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [FileLock {
                lock_type: F_UNLCK,
                lock_pid: 0,
                lock_vnode: 0,
                lock_first: 0,
                lock_last: 0,
            }; NR_LOCKS],
        ))
    }
}

static LOCK_TABLE: LockTable = LockTable::new();
static NR_ACTIVE_LOCKS: AtomicI32 = AtomicI32::new(0);

/// Maximum byte position (2 GB − 1), used when `l_len == 0` (lock to EOF).
const MAX_FILE_POS: i64 = 0x7FFFFFFF;

/// Compute the absolute byte range `[first, last]` from a `struct flock`
/// description and the file's current position / size.
/// Returns `Err(errno)` on invalid arguments or arithmetic overflow.
fn compute_range(
    _l_type: i16,
    l_whence: i16,
    l_start: i64,
    l_len: i64,
    filp_pos: i64,
    file_size: i64,
) -> Result<(i64, i64), i32> {
    let base = match l_whence {
        0 => 0,         // SEEK_SET
        1 => filp_pos,  // SEEK_CUR
        2 => file_size, // SEEK_END
        _ => return Err(EINVAL),
    };

    let first = base.checked_add(l_start).ok_or(EINVAL)?;
    if first < 0 {
        return Err(EINVAL);
    }

    let last = if l_len == 0 {
        MAX_FILE_POS
    } else if l_len < 0 {
        return Err(EINVAL);
    } else {
        // l_len > 0 — last byte is first + len - 1 (inclusive)
        let len = l_len as u64;
        let end = (first as u64).checked_add(len).ok_or(EINVAL)?;
        if end > 0 {
            (end - 1) as i64
        } else {
            return Err(EINVAL);
        }
    };

    if last < first {
        return Err(EINVAL);
    }
    Ok((first, last))
}

/// Get a raw pointer to the lock table.
fn lock_table_ptr() -> *mut [FileLock; NR_LOCKS] {
    LOCK_TABLE.0.get()
}

/// Find a free slot in the lock table.  Returns `None` if the table is full.
fn alloc_lock_slot() -> Option<usize> {
    let table = lock_table_ptr();
    (0..NR_LOCKS).find(|&i| unsafe { (*table)[i].lock_type == F_UNLCK })
}

/// Insert a new lock into the table.  Caller must ensure no conflict exists
/// and that a free slot is available.
unsafe fn insert_lock(l_type: i16, pid: i32, vnode: *const Vnode, first: i64, last: i64) {
    if let Some(slot) = alloc_lock_slot() {
        let table = lock_table_ptr();
        unsafe {
            (*table)[slot].lock_type = l_type;
            (*table)[slot].lock_pid = pid;
            (*table)[slot].lock_vnode = vnode as u32;
            (*table)[slot].lock_first = first;
            (*table)[slot].lock_last = last;
        }
        NR_ACTIVE_LOCKS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Remove all locks held by a specific PID on a specific vnode.
/// Called by `close_fd()` when a process closes a file.
pub fn remove_locks_by_pid_vnode(pid: i32, vnode: *const Vnode) {
    let vn = vnode as u32;
    let table = lock_table_ptr();
    for i in 0..NR_LOCKS {
        unsafe {
            let l = &mut (*table)[i];
            if l.lock_type != F_UNLCK && l.lock_pid == pid && l.lock_vnode == vn {
                l.lock_type = F_UNLCK;
                NR_ACTIVE_LOCKS.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

/// Check if the proposed lock `(l_type, pid, vnode, first, last)` conflicts
/// with any existing lock in the table.
///
/// Returns `Ok(())` if the lock can be granted immediately.
/// Returns `Err(LockConflict)` describing the first blocking lock.
pub fn check_lock(
    l_type: i16,
    pid: i32,
    vnode: *const Vnode,
    first: i64,
    last: i64,
) -> Result<(), LockConflict> {
    let vn = vnode as u32;
    let table = lock_table_ptr();
    for i in 0..NR_LOCKS {
        let l = unsafe { &(*table)[i] };
        if l.lock_type == F_UNLCK {
            continue;
        }
        if l.lock_vnode != vn {
            continue; // different file
        }
        if last < l.lock_first {
            continue; // new region before existing
        }
        if first > l.lock_last {
            continue; // new region after existing
        }
        // Both read locks — shared, no conflict.
        if l_type == F_RDLCK && l.lock_type == F_RDLCK {
            continue;
        }
        // Same PID — process doesn't conflict with itself.
        if l.lock_pid == pid {
            continue;
        }

        return Err(LockConflict {
            lock_type: l.lock_type,
            lock_pid: l.lock_pid,
            lock_first: l.lock_first,
            lock_last: l.lock_last,
        });
    }
    Ok(())
}

/// Remove or trim locks matching `(pid, vnode, first, last)`.
///
/// There are four overlap cases:
/// 1. Full overlap — clear the slot.
/// 2. Front overlap — shrink the front.
/// 3. Back overlap — shrink the back.
/// 4. Middle removal — split into two locks (needs a free slot; fails with
///    `ENOLCK` if none available).
///
/// Returns the number of locks released (≥ 0) or a negative errno.
pub fn remove_locks(pid: i32, vnode: *const Vnode, first: i64, last: i64) -> i32 {
    let vn = vnode as u32;
    let table = lock_table_ptr();
    let mut freed = 0;

    for i in 0..NR_LOCKS {
        let l = unsafe { &mut (*table)[i] };
        if l.lock_type == F_UNLCK {
            continue;
        }
        if l.lock_pid != pid || l.lock_vnode != vn {
            continue;
        }
        // No overlap at all
        if last < l.lock_first || first > l.lock_last {
            continue;
        }

        // Full overlap: [first..last] covers the entire locked range
        if first <= l.lock_first && last >= l.lock_last {
            l.lock_type = F_UNLCK;
            NR_ACTIVE_LOCKS.fetch_sub(1, Ordering::Relaxed);
            freed += 1;
            continue;
        }

        // Front overlap: unlock the front portion
        if first <= l.lock_first {
            l.lock_first = last + 1;
            freed += 1;
            continue;
        }

        // Back overlap: unlock the back portion
        if last >= l.lock_last {
            l.lock_last = first - 1;
            freed += 1;
            continue;
        }

        // Middle removal: [first..last] is entirely inside the locked range.
        // Split into two locks.
        let saved_last = l.lock_last;
        l.lock_last = first - 1; // left portion

        // Right portion needs a free slot.
        if let Some(slot) = alloc_lock_slot() {
            let t = lock_table_ptr();
            unsafe {
                (*t)[slot].lock_type = l.lock_type;
                (*t)[slot].lock_pid = pid;
                (*t)[slot].lock_vnode = vn;
                (*t)[slot].lock_first = last + 1;
                (*t)[slot].lock_last = saved_last;
            }
            NR_ACTIVE_LOCKS.fetch_add(1, Ordering::Relaxed);
        } else {
            // Roll back — can't split.
            l.lock_last = saved_last;
            return ENOLCK;
        }
        freed += 1;
    }

    freed
}

/// Wake all processes blocked on `FP_BLOCKED_ON_LOCK`.
/// Each will re-execute its lock request from scratch.
///
/// # Safety
///
/// Caller must ensure exclusive access to the fproc table.
pub unsafe fn lock_revive() {
    use crate::vfs::glo::vfs_global;
    let glob = unsafe { &mut *vfs_global() };
    for fp in glob.fproc.iter_mut() {
        if fp.fp_pid == 0 {
            continue;
        }
        if fp.fp_blocked_on == FP_BLOCKED_ON_LOCK {
            fp.fp_blocked_on = FP_BLOCKED_ON_NONE;
        }
    }
}

/// Perform an advisory file lock operation.
///
/// Reads the `struct flock` from the incoming message, validates parameters,
/// checks for conflicts against the lock table, and either grants the lock,
/// reports a conflicting lock (F_GETLK), returns EAGAIN (F_SETLK), or
/// suspends the caller (F_SETLKW).
///
/// # Safety
///
/// Must be called with the current process's fproc set up.
pub unsafe fn lock_op() -> i32 {
    use crate::vfs::glo::vfs_global;
    let glob = unsafe { &mut *vfs_global() };

    let fd = r2_i32(&glob.fs_m_in, LOCK_FD_OFF);
    let cmd = r2_i32(&glob.fs_m_in, LOCK_CMD_OFF);
    let l_type = r2_i16(&glob.fs_m_in, LOCK_TYPE_OFF);
    let l_whence = r2_i16(&glob.fs_m_in, LOCK_WHENCE_OFF);
    let l_start = r2_i64(&glob.fs_m_in, LOCK_START_OFF);
    let l_len = r2_i64(&glob.fs_m_in, LOCK_LEN_OFF);

    // Validate fd.
    if fd < 0 || (fd as usize) >= OPEN_MAX {
        return EBADF;
    }
    let fp = unsafe { &mut *glob.fproc.as_mut_ptr() };
    let filp_idx = fp.fp_filp[fd as usize];
    if filp_idx < 0 {
        return EBADF;
    }

    let filp = unsafe {
        let arr = core::ptr::addr_of_mut!(glob.filp) as *mut Filp;
        &*arr.add(filp_idx as usize)
    };

    // Validate lock type.
    if l_type != F_RDLCK && l_type != F_WRLCK && l_type != F_UNLCK {
        return EINVAL;
    }
    // F_GETLK with F_UNLCK is invalid.
    if cmd == F_GETLK && l_type == F_UNLCK {
        return EINVAL;
    }
    // Check file opened with appropriate access (filp_mode holds the
    // permission-style R_BIT/W_BIT bits, see do_open).
    if l_type == F_RDLCK && (filp.filp_mode & crate::vfs::protect::R_BIT) == 0 {
        return EBADF;
    }
    if l_type == F_WRLCK && (filp.filp_mode & crate::vfs::protect::W_BIT) == 0 {
        return EBADF;
    }

    let vp = filp.filp_vno;
    if vp.is_null() {
        return EBADF;
    }

    let file_size = unsafe { (*vp).v_size };

    let (first, last) =
        match compute_range(l_type, l_whence, l_start, l_len, filp.filp_pos, file_size) {
            Ok(r) => r,
            Err(e) => return e,
        };

    if cmd == F_UNLCK as i32 || l_type == F_UNLCK {
        let freed = remove_locks(fp.fp_pid, vp, first, last);
        if freed < 0 {
            return freed;
        }
        unsafe { lock_revive() };
        return OK;
    }

    // Check for conflicts.
    match check_lock(l_type, fp.fp_pid, vp, first, last) {
        Ok(()) => {
            // No conflict — insert the lock.
            unsafe {
                insert_lock(l_type, fp.fp_pid, vp, first, last);
            }
            OK
        }
        Err(conflict) => {
            if cmd == F_GETLK {
                // Report conflicting lock back to the caller.
                write_flock_reply(l_type, conflict);
                OK
            } else if cmd == F_SETLKW {
                // Blocking lock — suspend the process.
                fp.fp_blocked_on = FP_BLOCKED_ON_LOCK;
                SUSPEND
            } else {
                // F_SETLK non-blocking — return EAGAIN on conflict.
                EAGAIN
            }
        }
    }
}

/// Write a `struct flock` with the conflicting lock's details into the reply.
unsafe fn write_flock_reply(_l_type: i16, conflict: LockConflict) {
    let glob = unsafe { &mut *crate::vfs::glo::vfs_global() };
    w2_i16(&mut glob.fs_m_out, LOCK_TYPE_OFF, conflict.lock_type);
    w2_i16(&mut glob.fs_m_out, LOCK_WHENCE_OFF, 0); // SEEK_SET
    w2_i64(&mut glob.fs_m_out, LOCK_START_OFF, conflict.lock_first);
    // l_len: compute as (last - first + 1), clamp to MAX_FILE_POS
    let len = if conflict.lock_last >= MAX_FILE_POS {
        0 // "until end of file"
    } else {
        conflict.lock_last - conflict.lock_first + 1
    };
    w2_i64(&mut glob.fs_m_out, LOCK_LEN_OFF, len);
    w2_i32(&mut glob.fs_m_out, LOCK_PID_OFF, conflict.lock_pid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::types::Vnode;

    fn make_vnode() -> Vnode {
        Vnode {
            v_fs: 0,
            v_fs_e: 3,
            v_inode_nr: 42,
            v_mode: 0o100000, // regular file
            v_size: 1024,
            v_dev: 0,
            v_pipe: 0,
            ..Default::default()
        }
    }

    fn clear_table() {
        let table = lock_table_ptr();
        for i in 0..NR_LOCKS {
            unsafe {
                (*table)[i].lock_type = F_UNLCK;
                (*table)[i].lock_pid = 0;
                (*table)[i].lock_vnode = 0;
            }
        }
        NR_ACTIVE_LOCKS.store(0, Ordering::Relaxed);
    }

    #[test]
    fn test_no_conflict_empty_table() {
        clear_table();
        let vn = make_vnode();
        assert!(check_lock(F_WRLCK, 100, &vn, 0, 99).is_ok());
    }

    #[test]
    fn test_write_vs_write_conflict() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_WRLCK, 100, &vn, 0, 99);
        }
        assert!(check_lock(F_WRLCK, 200, &vn, 50, 150).is_err());
    }

    #[test]
    fn test_read_vs_read_ok() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_RDLCK, 100, &vn, 0, 99);
        }
        assert!(check_lock(F_RDLCK, 200, &vn, 0, 99).is_ok());
    }

    #[test]
    fn test_read_vs_write_conflict() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_RDLCK, 100, &vn, 0, 99);
        }
        assert!(check_lock(F_WRLCK, 200, &vn, 0, 99).is_err());
    }

    #[test]
    fn test_same_pid_no_conflict() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_WRLCK, 100, &vn, 0, 99);
        }
        assert!(check_lock(F_WRLCK, 100, &vn, 50, 150).is_ok());
    }

    #[test]
    fn test_different_vnode_no_conflict() {
        clear_table();
        let vn1 = make_vnode();
        let vn2 = Vnode {
            v_inode_nr: 99,
            ..make_vnode()
        };
        unsafe {
            insert_lock(F_WRLCK, 100, &vn1, 0, 99);
        }
        assert!(check_lock(F_WRLCK, 200, &vn2, 0, 99).is_ok());
    }

    #[test]
    fn test_no_overlap_before() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_WRLCK, 100, &vn, 100, 199);
        }
        assert!(check_lock(F_WRLCK, 200, &vn, 0, 99).is_ok());
    }

    #[test]
    fn test_no_overlap_after() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_WRLCK, 100, &vn, 0, 99);
        }
        assert!(check_lock(F_WRLCK, 200, &vn, 100, 199).is_ok());
    }

    #[test]
    fn test_remove_full_overlap() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_WRLCK, 100, &vn, 0, 99);
        }
        let r = remove_locks(100, &vn, 0, 99);
        assert_eq!(r, 1);
        assert!(check_lock(F_WRLCK, 200, &vn, 0, 99).is_ok());
    }

    #[test]
    fn test_remove_front_overlap() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_WRLCK, 100, &vn, 50, 149);
        }
        let r = remove_locks(100, &vn, 0, 99);
        assert_eq!(r, 1);
        // Lock should now be [100, 149]
        assert!(check_lock(F_WRLCK, 200, &vn, 60, 120).is_err());
        assert!(check_lock(F_WRLCK, 200, &vn, 100, 110).is_err());
    }

    #[test]
    fn test_remove_back_overlap() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_WRLCK, 100, &vn, 0, 99);
        }
        let r = remove_locks(100, &vn, 50, 149);
        assert_eq!(r, 1);
        // Lock should now be [0, 49]
        assert!(check_lock(F_WRLCK, 200, &vn, 0, 49).is_err());
    }

    #[test]
    fn test_compute_range_seek_set() {
        let r = compute_range(F_WRLCK, 0, 100, 50, 0, 1024).unwrap();
        assert_eq!(r, (100, 149));
    }

    #[test]
    fn test_compute_range_seek_cur() {
        let r = compute_range(F_WRLCK, 1, 10, 20, 50, 1024).unwrap();
        assert_eq!(r, (60, 79));
    }

    #[test]
    fn test_compute_range_seek_end() {
        let r = compute_range(F_WRLCK, 2, -10, 5, 0, 100).unwrap();
        assert_eq!(r, (90, 94));
    }

    #[test]
    fn test_compute_range_len_zero() {
        let r = compute_range(F_WRLCK, 0, 0, 0, 0, 1024).unwrap();
        assert_eq!(r, (0, MAX_FILE_POS));
    }

    #[test]
    fn test_compute_range_invalid_whence() {
        assert!(compute_range(F_WRLCK, 99, 0, 10, 0, 1024).is_err());
    }

    #[test]
    fn test_compute_range_negative_start() {
        let r = compute_range(F_WRLCK, 0, -5, 10, 0, 1024);
        assert!(r.is_err());
    }

    #[test]
    fn test_compute_range_overflow() {
        let r = compute_range(F_WRLCK, 0, i64::MAX, 10, 0, 1024);
        assert!(r.is_err());
    }

    #[test]
    fn test_remove_locks_by_pid_vnode() {
        clear_table();
        let vn = make_vnode();
        unsafe {
            insert_lock(F_WRLCK, 100, &vn, 0, 99);
            insert_lock(F_RDLCK, 100, &vn, 200, 299);
            insert_lock(F_WRLCK, 200, &vn, 0, 99); // different PID
        }
        remove_locks_by_pid_vnode(100, &vn);
        // PID 200's lock should survive
        assert!(check_lock(F_WRLCK, 300, &vn, 0, 99).is_err());
    }
}
