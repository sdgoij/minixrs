//! ProcFS VTreeFS hook implementations — adapted from `minix/fs/procfs/tree.c`

use crate::procfs::buf;
use crate::procfs::consts::*;

// ── External process table stubs ──────────────────────────────────────

/// Kernel process table entry (stub).
#[derive(Debug, Default, Clone, Copy)]
pub struct Proc {
    pub p_nr: i32,
    pub p_endpoint: i32,
    pub p_rts_flags: u32,
    pub p_name: [u8; 16],
}

/// PM process table entry (stub).
#[derive(Debug, Default, Clone, Copy)]
pub struct MProc {
    pub mp_flags: u32,
    pub mp_pid: i32,
    pub mp_name: [u8; 16],
}

/// VFS process table entry (stub).
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
pub fn slot_in_use(_slot: i32) -> bool {
    false
}

/// Regenerate the set of PID directories in the root.
pub fn construct_pid_dirs() {
    // No-op (stub).
}

/// Construct one or all file entries in a PID directory.
///
/// `parent` is the inode index of the PID directory.
/// `name` is `None` to construct all entries, or `Some(name)` for a
/// specific file.
pub fn construct_pid_entries(_parent: u32, _name: Option<&str>) {
    // No-op (stub).
}

// ── VTreeFS hooks (called by the VTreeFS event loop) ──────────────────

/// Path name resolution hook.
///
/// Tries `find_inode` first.  If the entry is not found, lazily constructs
/// PID directories / entries.
pub fn lookup_hook(parent: u32, name: &str) -> i32 {
    if libs::vtreefs::find_inode(parent, name).is_some() {
        return OK;
    }

    // Not found — try constructing PID entries lazily.
    construct_pid_entries(parent, Some(name));

    // Check again after construction.
    if libs::vtreefs::find_inode(parent, name).is_some() {
        OK
    } else {
        EINVAL
    }
}

/// Directory entry retrieval hook.
///
/// Updates the process tables and regenerates PID directory entries under
/// the root.
pub fn getdents_hook(node: u32) -> i32 {
    if node == libs::vtreefs::get_root_inode() {
        construct_pid_dirs();
    }
    OK
}

/// Regular file read hook.
///
/// Initialises the output buffer, decodes `cbdata` to find the file
/// handler, calls it, then returns the result.
pub fn read_hook(_node: u32, offset: u64, max_len: usize, cbdata: libs::vtreefs::CbData) -> i32 {
    buf::buf_init(offset, max_len);

    if cbdata != 0 {
        // Decode the handler type from bit 0 of cbdata.
        if cbdata & 1 == 1 {
            // Dynamic handler (bit 0 set): fn(i32).
            // Get the slot number from the parent inode's cbdata.
            let parent_cbdata =
                libs::vtreefs::get_inode_cbdata(libs::vtreefs::get_inode(_node).parent_id);
            let slot = parent_cbdata as i32;
            let f: fn(i32) =
                unsafe { core::mem::transmute::<*const (), fn(i32)>((cbdata & !1) as *const ()) };
            f(slot);
        } else {
            // Static handler (bit 0 clear): fn().
            let f: fn() = unsafe { core::mem::transmute::<*const (), fn()>(cbdata as *const ()) };
            f();
        }
    }

    OK
}

/// Symbolic link resolution hook.
pub fn rdlink_hook(_node: u32, _ptr: &mut [u8]) -> i32 {
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
        // Initialise VTreeFS first so lookup doesn't hit an empty table.
        crate::procfs::main::init_tree();
        // "test" doesn't exist; lookup_hook should return EINVAL.
        assert_eq!(lookup_hook(0, "test"), EINVAL);
        assert_eq!(getdents_hook(0), OK);
        assert_eq!(rdlink_hook(0, &mut []), OK);
    }

    #[test]
    fn read_hook_returns_ok() {
        crate::procfs::main::init_tree();
        let status = read_hook(0, 0, 0, 0);
        assert_eq!(status, OK);
    }

    #[test]
    fn constructors_no_panic() {
        construct_pid_dirs();
        construct_pid_entries(0, None);
        construct_pid_entries(0, Some("psinfo"));
    }
}
