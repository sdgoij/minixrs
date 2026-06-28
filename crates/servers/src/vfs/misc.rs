//! Miscellaneous VFS operations — adapted from `minix/servers/vfs/misc.c`
//!
//! Covers process lifecycle hooks (pm_exit, pm_fork, pm_set*), system
//! info queries (do_getsysinfo), resource usage, and utility helpers.

use crate::vfs::consts::*;
use crate::vfs::types::*;

/// Clean up after a process exits.
///
/// Closes all open file descriptors, releases vnodes, and notifies VM.
///
/// TODO: iterate fp_filp, call close_filp for each, clear fp_flags.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` (free_proc)
pub fn pm_exit() {
    // TODO: implement process exit cleanup
}

/// Handle VFS side of fork: copy fd table and credentials to child.
///
/// Duplicates the parent's filp entries (incrementing refcounts) and
/// copies credential fields to the child's Fproc.
///
/// TODO: for each fd in parent, dup filp into child's fp_filp.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` (pm_fork)
pub fn pm_fork(pproc: i32, cproc: i32, cpid: i32) -> i32 {
    let _ = (pproc, cproc, cpid);
    ENOSYS
}

/// Handle exec: close fds with FD_CLOEXEC flag.
///
/// TODO: iterate fp_filp, close any with FD_CLOEXEC set in fp_cloexec.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` (do_exec)
pub fn pm_exec() {
    // TODO: implement close-on-exec
}

/// Set real and effective gid for a process.
pub fn pm_setgid(proc_e: i32, egid: i32, rgid: i32) {
    let _ = (proc_e, egid, rgid);
}

/// Set real and effective uid for a process.
pub fn pm_setuid(proc_e: i32, euid: i32, ruid: i32) {
    let _ = (proc_e, euid, ruid);
}

/// Set groups for a process.
pub fn pm_setgroups(proc_e: i32, ngroups: i32, addr: *const u16) {
    let _ = (proc_e, ngroups, addr);
}

/// Handle setsid: create a new session.
pub fn pm_setsid(proc_e: i32) {
    let _ = proc_e;
}

/// Prepare for reboot: sync all filesystems.
///
/// TODO: iterate vmnt table, call req_sync for each mounted FS.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` (pm_reboot)
pub fn pm_reboot() {
    // TODO: sync all filesystems
}

/// Create a core dump of a process.
///
/// TODO: open "core" file, write ELF core dump via write_elf_core_file().
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` (pm_dumpcore)
pub fn pm_dumpcore(sig: i32, exe_name: u64) -> i32 {
    let _ = (sig, exe_name);
    ENOSYS
}

/// Get system info: copy a VFS data structure to userspace.
///
/// Supports requests: SI_PROC_TAB (copy FPROC table), SI_DMAP_TAB (copy
/// dmap table), etc.
///
/// TODO: dispatch on `what` field, call sys_datacopy_wrapper to copy
/// the requested table to user address `where`.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` (do_getsysinfo)
pub fn do_getsysinfo() -> i32 {
    ENOSYS
}

/// Get resource usage for a process.
///
/// TODO: fill rusage struct from process accounting data.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` (do_getrusage)
pub fn do_getrusage() -> i32 {
    ENOSYS
}

/// Duplicate a VM file descriptor across process boundaries.
///
/// Used by VM to grant fd access to another process during exec/mmap.
///
/// TODO: look up fp_filp[pfd], dup filp, store in *vmfd.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/misc.c` (dupvm)
pub fn dupvm(fp: &mut Fproc, pfd: i32, vmfd: &mut i32, f: *mut *mut Filp) -> i32 {
    let _ = (fp, pfd, vmfd, f);
    ENOSYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pm_exit_does_not_crash() {
        pm_exit();
    }

    #[test]
    fn test_do_getsysinfo_returns_enosys() {
        assert_eq!(do_getsysinfo(), ENOSYS);
    }
}
