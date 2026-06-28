//! ProcFS VTreeFS hook implementations — adapted from `minix/fs/procfs/tree.c`
//!
//! All hooks are currently stubs. Real implementations will call VTreeFS
//! functions (`add_inode`, `get_root_inode`, `delete_inode`, etc.)

use crate::procfs::buf;
use crate::procfs::consts::*;

// ── External process table stubs ──────────────────────────────────────

/// Kernel process table entry (stub).
///
/// TODO: fill in real fields from `kernel/proc.h`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Proc {
    pub p_nr: i32,
    pub p_endpoint: i32,
    pub p_rts_flags: u32,
    pub p_name: [u8; 16],
}

/// PM process table entry (stub).
///
/// TODO: fill in real fields from `pm/mproc.h`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MProc {
    pub mp_flags: u32,
    pub mp_pid: i32,
    pub mp_name: [u8; 16],
}

/// VFS process table entry (stub).
///
/// TODO: fill in real fields from `vfs/fproc.h`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FProc {
    pub fp_flags: u32,
}

// Stub table declarations.
const PROC_INIT: Proc = Proc {
    p_nr: 0,
    p_endpoint: 0,
    p_rts_flags: 0,
    p_name: [0; 16],
};
pub static PROC: [Proc; 36] = [PROC_INIT; 36];

const MPROC_INIT: MProc = MProc {
    mp_flags: 0,
    mp_pid: 0,
    mp_name: [0; 16],
};
pub static MPROC: [MProc; 32] = [MPROC_INIT; 32];

const FPROC_INIT: FProc = FProc { fp_flags: 0 };
pub static FPROC: [FProc; 32] = [FPROC_INIT; 32];

// ── Hook implementations ──────────────────────────────────────────────

/// Return whether the given kernel/PM slot is in use by a process.
///
/// TODO: check `proc[slot].p_rts_flags != RTS_SLOT_FREE` for kernel tasks
///       and `mproc[slot - NR_TASKS].mp_flags & IN_USE` for user processes.
pub fn slot_in_use(_slot: i32) -> bool {
    false
}

/// Regenerate the set of PID directories in the root.
///
/// TODO: iterate slots, compare PIDs, call `add_inode`/`delete_inode`.
pub fn construct_pid_dirs() {
    // No-op (stub).
}

/// Construct one or all file entries in a PID directory.
///
/// `parent` is the inode index of the PID directory.
/// `name` is `None` to construct all entries, or `Some(name)` for a specific file.
///
/// TODO: call `get_inode_by_index`/`get_inode_by_name` and `add_inode`.
pub fn construct_pid_entries(_parent: u16, _name: Option<&str>) {
    // No-op (stub).
}

/// Path name resolution hook.
///
/// TODO: lazily update process tables, reconstruct PID dirs if parent is
///       root, or construct individual entries for PID subdirectories.
pub fn lookup_hook(_parent: u16, _name: &str) -> i32 {
    OK
}

/// Directory entry retrieval hook.
///
/// TODO: update tables and reconstruct PID directories.
pub fn getdents_hook(_node: u16) -> i32 {
    OK
}

/// Regular file read hook.
///
/// TODO: call `buf_init`, invoke the appropriate file handler (static or
///       dynamic), then return the result from `buf_get`.
pub fn read_hook(_node: u16, _offset: u64, _max_len: usize) -> (&'static [u8], i32) {
    buf::buf_init(_offset, _max_len);
    // No handler called (stub).
    let (data, _len) = buf::buf_get();
    (data, OK)
}

/// Symbolic link resolution hook.
///
/// TODO: if parent is a PID directory, call `pid_link` to fill `ptr`.
pub fn rdlink_hook(_node: u16, _ptr: &mut [u8]) -> i32 {
    OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_in_use_returns_false() {
        assert!(!slot_in_use(0));
    }

    #[test]
    fn hooks_return_ok() {
        assert_eq!(lookup_hook(0, "test"), OK);
        assert_eq!(getdents_hook(0), OK);
        assert_eq!(rdlink_hook(0, &mut []), OK);
    }

    #[test]
    fn read_hook_returns_empty() {
        let (data, status) = read_hook(0, 0, 0);
        assert_eq!(status, OK);
        assert!(data.is_empty());
    }

    #[test]
    fn constructors_no_panic() {
        construct_pid_dirs();
        construct_pid_entries(0, None);
        construct_pid_entries(0, Some("psinfo"));
    }
}
