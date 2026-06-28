//! VFS system call dispatch table — adapted from `minix/servers/vfs/table.c`
//!
//! Maps VFS call numbers to handler functions defined in the `call` module.
//! All handlers currently return `ENOSYS` — they will be implemented in
//! later tasks when the FS request layer is wired in.

use crate::vfs::call::*;
use crate::vfs::consts::*;

// ── Dispatch table ───────────────────────────────────────────────────────────

/// Type of a VFS handler function.
pub type VfsHandler = fn() -> i32;

/// Index helper: convert a VFS call number to a table index.
const fn call_index(n: i32) -> usize {
    (n - VFS_BASE) as usize
}

/// The VFS call dispatch table.
///
/// Maps each `VFS_*` call number to its handler function.
static CALL_VEC: [VfsHandler; NR_VFS_CALLS] = {
    let mut table: [VfsHandler; NR_VFS_CALLS] = [no_sys; NR_VFS_CALLS];
    table[call_index(VFS_READ)] = do_read;
    table[call_index(VFS_WRITE)] = do_write;
    table[call_index(VFS_LSEEK)] = do_lseek;
    table[call_index(VFS_OPEN)] = do_open;
    table[call_index(VFS_CREAT)] = do_creat;
    table[call_index(VFS_CLOSE)] = do_close;
    table[call_index(VFS_LINK)] = do_link;
    table[call_index(VFS_UNLINK)] = do_unlink;
    table[call_index(VFS_CHDIR)] = do_chdir;
    table[call_index(VFS_MKDIR)] = do_mkdir;
    table[call_index(VFS_MKNOD)] = do_mknod;
    table[call_index(VFS_CHMOD)] = do_chmod;
    table[call_index(VFS_CHOWN)] = do_chown;
    table[call_index(VFS_MOUNT)] = do_mount;
    table[call_index(VFS_UMOUNT)] = do_umount;
    table[call_index(VFS_ACCESS)] = do_access;
    table[call_index(VFS_SYNC)] = do_sync;
    table[call_index(VFS_RENAME)] = do_rename;
    table[call_index(VFS_RMDIR)] = do_rmdir;
    table[call_index(VFS_SYMLINK)] = do_slink;
    table[call_index(VFS_READLINK)] = do_rdlink;
    table[call_index(VFS_STAT)] = do_stat;
    table[call_index(VFS_FSTAT)] = do_fstat;
    table[call_index(VFS_LSTAT)] = do_lstat;
    table[call_index(VFS_IOCTL)] = do_ioctl;
    table[call_index(VFS_FCNTL)] = do_fcntl;
    table[call_index(VFS_PIPE2)] = do_pipe2;
    table[call_index(VFS_UMASK)] = do_umask;
    table[call_index(VFS_CHROOT)] = do_chroot;
    table[call_index(VFS_GETDENTS)] = do_getdents;
    table[call_index(VFS_SELECT)] = do_select;
    table[call_index(VFS_FCHDIR)] = do_fchdir;
    table[call_index(VFS_FSYNC)] = do_fsync;
    table[call_index(VFS_TRUNCATE)] = do_truncate;
    table[call_index(VFS_FTRUNCATE)] = do_ftruncate;
    table[call_index(VFS_FCHMOD)] = do_chmod; // same handler as chmod(2)
    table[call_index(VFS_FCHOWN)] = do_chown; // same handler as chown(2)
    table[call_index(VFS_UTIMENS)] = do_utimens;
    table[call_index(VFS_VMCALL)] = do_vm_call;
    table[call_index(VFS_GETVFSSTAT)] = do_getvfsstat;
    table[call_index(VFS_STATVFS1)] = do_statvfs;
    table[call_index(VFS_FSTATVFS1)] = do_fstatvfs;
    table[call_index(VFS_GETRUSAGE)] = do_getrusage;
    table[call_index(VFS_SVRCTL)] = do_svrctl;
    table[call_index(VFS_GCOV_FLUSH)] = do_gcov_flush;
    table[call_index(VFS_MAPDRIVER)] = do_mapdriver;
    table[call_index(VFS_COPYFD)] = do_copyfd;
    table[call_index(VFS_CHECKPERMS)] = do_checkperms;
    table[call_index(VFS_GETSYSINFO)] = do_getsysinfo;
    table
};

// ── Dispatch function ────────────────────────────────────────────────────────

/// Look up and call a handler for the given VFS call number.
///
/// Returns the handler's return value (an errno or OK).
#[inline]
pub fn dispatch(call_nr: i32) -> i32 {
    let idx = (call_nr - VFS_BASE) as usize;
    if idx < NR_VFS_CALLS {
        CALL_VEC[idx]()
    } else {
        ENOSYS
    }
}

// ── Default stub ─────────────────────────────────────────────────────────────

/// Default stub: all unimplemented table slots return `ENOSYS`.
pub fn no_sys() -> i32 {
    ENOSYS
}
