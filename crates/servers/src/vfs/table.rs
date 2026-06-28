//! VFS system call dispatch table — adapted from `minix/servers/vfs/table.c`
//!
//! Maps VFS call numbers to handler functions. All handlers are stubs
//! that return `ENOSYS` — they will be implemented in later tasks.

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
    table[call_index(VFS_READ)] = no_sys;
    table[call_index(VFS_WRITE)] = no_sys;
    table[call_index(VFS_LSEEK)] = no_sys;
    table[call_index(VFS_OPEN)] = no_sys;
    table[call_index(VFS_CREAT)] = no_sys;
    table[call_index(VFS_CLOSE)] = no_sys;
    table[call_index(VFS_LINK)] = no_sys;
    table[call_index(VFS_UNLINK)] = no_sys;
    table[call_index(VFS_CHDIR)] = no_sys;
    table[call_index(VFS_MKDIR)] = no_sys;
    table[call_index(VFS_MKNOD)] = no_sys;
    table[call_index(VFS_CHMOD)] = no_sys;
    table[call_index(VFS_CHOWN)] = no_sys;
    table[call_index(VFS_MOUNT)] = no_sys;
    table[call_index(VFS_UMOUNT)] = no_sys;
    table[call_index(VFS_ACCESS)] = no_sys;
    table[call_index(VFS_SYNC)] = no_sys;
    table[call_index(VFS_RENAME)] = no_sys;
    table[call_index(VFS_RMDIR)] = no_sys;
    table[call_index(VFS_SYMLINK)] = no_sys;
    table[call_index(VFS_READLINK)] = no_sys;
    table[call_index(VFS_STAT)] = no_sys;
    table[call_index(VFS_FSTAT)] = no_sys;
    table[call_index(VFS_LSTAT)] = no_sys;
    table[call_index(VFS_IOCTL)] = no_sys;
    table[call_index(VFS_FCNTL)] = no_sys;
    table[call_index(VFS_PIPE2)] = no_sys;
    table[call_index(VFS_UMASK)] = no_sys;
    table[call_index(VFS_CHROOT)] = no_sys;
    table[call_index(VFS_GETDENTS)] = no_sys;
    table[call_index(VFS_SELECT)] = no_sys;
    table[call_index(VFS_FCHDIR)] = no_sys;
    table[call_index(VFS_FSYNC)] = no_sys;
    table[call_index(VFS_TRUNCATE)] = no_sys;
    table[call_index(VFS_FTRUNCATE)] = no_sys;
    table[call_index(VFS_FCHMOD)] = no_sys;
    table[call_index(VFS_FCHOWN)] = no_sys;
    table[call_index(VFS_UTIMENS)] = no_sys;
    table[call_index(VFS_VMCALL)] = no_sys;
    table[call_index(VFS_GETVFSSTAT)] = no_sys;
    table[call_index(VFS_STATVFS1)] = no_sys;
    table[call_index(VFS_FSTATVFS1)] = no_sys;
    table[call_index(VFS_GETRUSAGE)] = no_sys;
    table[call_index(VFS_SVRCTL)] = no_sys;
    table[call_index(VFS_GCOV_FLUSH)] = no_sys;
    table[call_index(VFS_MAPDRIVER)] = no_sys;
    table[call_index(VFS_COPYFD)] = no_sys;
    table[call_index(VFS_CHECKPERMS)] = no_sys;
    table[call_index(VFS_GETSYSINFO)] = no_sys;
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

// ── Stub handler ─────────────────────────────────────────────────────────────

/// Default stub: all unimplemented handlers return `ENOSYS`.
pub fn no_sys() -> i32 {
    ENOSYS
}

// ── Trait-based decl macro ───────────────────────────────────────────────────

/// Macro to declare a VFS handler stub with a doc comment.
///
/// Usage:
/// ```ignore
/// vfs_handler!(do_read, "read(2)");
/// ```
#[macro_export]
macro_rules! vfs_handler {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        pub fn $name() -> i32 {
            $crate::vfs::table::no_sys()
        }
    };
}

// ── Handler stubs ────────────────────────────────────────────────────────────
// These will be replaced with real implementations in later tasks.
// They are defined here so the dispatch table compiles and each
// handler name is a first-class function item.

vfs_handler!(do_read, "read(2)");
vfs_handler!(do_write, "write(2)");
vfs_handler!(do_lseek, "lseek(2)");
vfs_handler!(do_open, "open(2)");
vfs_handler!(do_creat, "creat(2)");
vfs_handler!(do_close, "close(2)");
vfs_handler!(do_link, "link(2)");
vfs_handler!(do_unlink, "unlink(2) / rmdir(2)");
vfs_handler!(do_chdir, "chdir(2)");
vfs_handler!(do_mkdir, "mkdir(2)");
vfs_handler!(do_mknod, "mknod(2)");
vfs_handler!(do_chmod, "chmod(2) / fchmod(2)");
vfs_handler!(do_chown, "chown(2) / fchown(2)");
vfs_handler!(do_mount, "mount(2)");
vfs_handler!(do_umount, "umount(2)");
vfs_handler!(do_access, "access(2)");
vfs_handler!(do_sync, "sync(2)");
vfs_handler!(do_rename, "rename(2)");
vfs_handler!(do_slink, "symlink(2)");
vfs_handler!(do_rdlink, "readlink(2)");
vfs_handler!(do_stat, "stat(2)");
vfs_handler!(do_fstat, "fstat(2)");
vfs_handler!(do_lstat, "lstat(2)");
vfs_handler!(do_ioctl, "ioctl(2)");
vfs_handler!(do_fcntl, "fcntl(2)");
vfs_handler!(do_pipe2, "pipe2(2)");
vfs_handler!(do_umask, "umask(2)");
vfs_handler!(do_chroot, "chroot(2)");
vfs_handler!(do_getdents, "getdents(2)");
vfs_handler!(do_select, "select(2)");
vfs_handler!(do_fchdir, "fchdir(2)");
vfs_handler!(do_fsync, "fsync(2)");
vfs_handler!(do_truncate, "truncate(2)");
vfs_handler!(do_ftruncate, "ftruncate(2)");
vfs_handler!(do_utimens, "utimens(2)");
vfs_handler!(do_vm_call, "vm_call");
vfs_handler!(do_getvfsstat, "getvfsstat(2)");
vfs_handler!(do_statvfs, "statvfs(2)");
vfs_handler!(do_fstatvfs, "fstatvfs(2)");
vfs_handler!(do_getrusage, "getrusage(2)");
vfs_handler!(do_svrctl, "svrctl(2)");
vfs_handler!(do_gcov_flush, "gcov_flush(2)");
vfs_handler!(do_mapdriver, "mapdriver(2)");
vfs_handler!(do_copyfd, "copyfd(2)");
vfs_handler!(do_checkperms, "checkperms(2)");
vfs_handler!(do_getsysinfo, "getsysinfo(2)");
