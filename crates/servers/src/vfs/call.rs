//! VFS call handler functions — adapted from the following C sources:
//!
//! | Category         | Source file     | Functions                                 |
//! |------------------|-----------------|-------------------------------------------|
//! | File operations  | `open.c`        | `do_open`, `do_creat`, `do_close`, `do_lseek`, `do_mknod`, `do_mkdir` |
//! | File operations  | `read.c`        | `do_read`, `do_getdents`                  |
//! | File operations  | `write.c`       | `do_write`                                |
//! | File operations  | `pipe.c`        | `do_pipe2`                                |
//! | File operations  | `link.c`        | `do_link`, `do_unlink`, `do_rename`, `do_truncate`, `do_ftruncate`, `do_rdlink` |
//! | File operations  | `select.c`      | `do_select`                               |
//! | Directory ops    | `stadir.c`      | `do_chdir`, `do_fchdir`, `do_chroot`, `do_stat`, `do_fstat`, `do_lstat`, `do_statvfs`, `do_fstatvfs`, `do_getvfsstat` |
//! | Permission ops   | `protect.c`     | `do_access`, `do_chmod`, `do_chown`, `do_umask` |
//! | Mount ops        | `mount.c`       | `do_mount`, `do_umount`                   |
//! | Mount ops        | `dmap.c`        | `do_mapdriver`                            |
//! | Time ops         | `time.c`        | `do_utimens`                              |
//! | Misc ops         | `misc.c`        | `do_fcntl`, `do_sync`, `do_fsync`, `do_svrctl`, `do_getsysinfo`, `do_vm_call`, `do_getrusage` |
//! | Misc ops         | `gcov.c`        | `do_gcov_flush`                           |
//! | Lock ops         | `lock.c`        | `lock_op`                                 |
//!
//! All handlers are stubs returning `ENOSYS` — they will be implemented
//! when the FS request layer is wired in.

use crate::vfs::consts::*;

// =============================================================================
// File operations
// =============================================================================

/// Perform the `open(name, flags)` system call (O_CREAT *not* set).
///
/// C source: `minix/servers/vfs/open.c` — `do_open()` (line 39)
pub fn do_open() -> i32 {
    ENOSYS
}

/// Perform the `creat(name, mode)` system call.
///
/// C source: `minix/servers/vfs/open.c` — `do_creat()` (line 147)
pub fn do_creat() -> i32 {
    ENOSYS
}

/// Perform the `close(fd)` system call.
///
/// C source: `minix/servers/vfs/open.c` — `do_close()` (line 139)
pub fn do_close() -> i32 {
    ENOSYS
}

/// Perform the `lseek(fd, offset, whence)` system call.
///
/// C source: `minix/servers/vfs/open.c` — `do_lseek()` (line 143)
pub fn do_lseek() -> i32 {
    ENOSYS
}

/// Perform the `read(fd, buf, nbytes)` system call.
///
/// C source: `minix/servers/vfs/read.c` — `do_read()` (line 31)
pub fn do_read() -> i32 {
    ENOSYS
}

/// Perform the `write(fd, buf, nbytes)` system call.
///
/// C source: `minix/servers/vfs/write.c` — `do_write()` (line 15)
pub fn do_write() -> i32 {
    ENOSYS
}

/// Perform the `getdents(fd, buf, nbytes)` system call.
///
/// C source: `minix/servers/vfs/read.c` — `do_getdents()` (line 8)
pub fn do_getdents() -> i32 {
    ENOSYS
}

/// Perform the `pipe2(fileds[2], flags)` system call.
///
/// C source: `minix/servers/vfs/pipe.c` — `do_pipe2()` (line 164)
pub fn do_pipe2() -> i32 {
    ENOSYS
}

/// Perform the `ioctl(fd, request, arg)` system call.
///
/// C source: `minix/servers/vfs/device.c` — `do_ioctl()` (line 45)
pub fn do_ioctl() -> i32 {
    ENOSYS
}

/// Perform the `fcntl(fd, cmd, arg)` system call.
///
/// C source: `minix/servers/vfs/misc.c` — `do_fcntl()` (line 110)
pub fn do_fcntl() -> i32 {
    ENOSYS
}

/// Perform the `copyfd(fd, newfd, flags)` — duplicate a file descriptor.
///
/// C source: `minix/servers/vfs/filedes.c` — `do_copyfd()` (line 82)
pub fn do_copyfd() -> i32 {
    ENOSYS
}

/// Perform the `truncate(path, length)` system call.
///
/// C source: `minix/servers/vfs/link.c` — `do_truncate()` (line 91)
pub fn do_truncate() -> i32 {
    ENOSYS
}

/// Perform the `ftruncate(fd, length)` system call.
///
/// C source: `minix/servers/vfs/link.c` — `do_ftruncate()` (line 92)
pub fn do_ftruncate() -> i32 {
    ENOSYS
}

/// Perform the `sync()` system call — flush all filesystem buffers.
///
/// C source: `minix/servers/vfs/misc.c` — `do_sync()` (line 116)
pub fn do_sync() -> i32 {
    ENOSYS
}

/// Perform the `fsync(fd)` system call — flush a single file descriptor.
///
/// C source: `minix/servers/vfs/misc.c` — `do_fsync()` (line 117)
pub fn do_fsync() -> i32 {
    ENOSYS
}

/// Perform the `select(nfds, readfds, writefds, errorfds, timeout)` call.
///
/// C source: `minix/servers/vfs/select.c` — `do_select()` (line 30)
pub fn do_select() -> i32 {
    ENOSYS
}

// =============================================================================
// Directory operations
// =============================================================================

/// Perform the `chdir(path)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_chdir()` (line 50)
pub fn do_chdir() -> i32 {
    ENOSYS
}

/// Perform the `fchdir(fd)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_fchdir()` (line 32)
pub fn do_fchdir() -> i32 {
    ENOSYS
}

/// Perform the `chroot(path)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_chroot()` (line 253)
pub fn do_chroot() -> i32 {
    ENOSYS
}

/// Perform the `stat(path, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_stat()` (line 255)
pub fn do_stat() -> i32 {
    ENOSYS
}

/// Perform the `fstat(fd, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_fstat()` (line 254)
pub fn do_fstat() -> i32 {
    ENOSYS
}

/// Perform the `lstat(path, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_lstat()` (line 259)
pub fn do_lstat() -> i32 {
    ENOSYS
}

/// Perform the `statvfs(path, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_statvfs()` (line 256)
pub fn do_statvfs() -> i32 {
    ENOSYS
}

/// Perform the `fstatvfs(fd, buf)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_fstatvfs()` (line 257)
pub fn do_fstatvfs() -> i32 {
    ENOSYS
}

/// Perform the `getvfsstat(buf, bufsize, flags)` system call.
///
/// C source: `minix/servers/vfs/stadir.c` — `do_getvfsstat()` (line 258)
pub fn do_getvfsstat() -> i32 {
    ENOSYS
}

/// Perform the `readlink(path, buf, bufsize)` system call.
///
/// C source: `minix/servers/vfs/link.c` — `do_rdlink()` (line 94)
pub fn do_rdlink() -> i32 {
    ENOSYS
}

/// Perform the `link(oldpath, newpath)` system call.
///
/// C source: `minix/servers/vfs/link.c` — `do_link()` (line 30)
pub fn do_link() -> i32 {
    ENOSYS
}

/// Perform the `unlink(path)` system call (also used for `rmdir` in C).
///
/// C source: `minix/servers/vfs/link.c` — `do_unlink()` (line 88)
pub fn do_unlink() -> i32 {
    ENOSYS
}

/// Perform the `rename(oldpath, newpath)` system call.
///
/// C source: `minix/servers/vfs/link.c` — `do_rename()` (line 89)
pub fn do_rename() -> i32 {
    ENOSYS
}

/// Perform the `mkdir(path, mode)` system call.
///
/// C source: `minix/servers/vfs/open.c` — `do_mkdir()` (line 145)
pub fn do_mkdir() -> i32 {
    ENOSYS
}

/// Perform the `mknod(path, mode, dev)` system call.
///
/// C source: `minix/servers/vfs/open.c` — `do_mknod()` (line 144)
pub fn do_mknod() -> i32 {
    ENOSYS
}

/// Perform the `symlink(target, linkpath)` system call.
///
/// C source: `minix/servers/vfs/open.c` — `do_slink()` (line 148)
pub fn do_slink() -> i32 {
    ENOSYS
}

/// Perform the `rmdir(path)` system call.
///
/// In the original C code, `VFS_RMDIR` maps to `do_unlink` (see `table.c` line 37).
/// This separate stub is kept for clarity and will dispatch to the same
/// internal logic once implemented.
///
/// C source: `minix/servers/vfs/link.c` — `do_unlink()` (also handles RMDIR, line 88)
pub fn do_rmdir() -> i32 {
    ENOSYS
}

// =============================================================================
// Permission operations
// =============================================================================

/// Perform the `access(path, mode)` system call.
///
/// C source: `minix/servers/vfs/protect.c` — `do_access()` (line 177)
pub fn do_access() -> i32 {
    ENOSYS
}

/// Perform the `chmod(path, mode)` and `fchmod(fd, mode)` system calls.
///
/// C source: `minix/servers/vfs/protect.c` — `do_chmod()` (line 25)
/// Also handles `VFS_FCHMOD` (see `table.c` line 54: `CALL(VFS_FCHMOD) = do_chmod`).
pub fn do_chmod() -> i32 {
    ENOSYS
}

/// Perform the `chown(path, owner, group)` and `fchown(fd, owner, group)` system calls.
///
/// C source: `minix/servers/vfs/protect.c` — `do_chown()` (line 179)
/// Also handles `VFS_FCHOWN` (see `table.c` line 55: `CALL(VFS_FCHOWN) = do_chown`).
pub fn do_chown() -> i32 {
    ENOSYS
}

/// Perform the `umask(mode)` system call.
///
/// C source: `minix/servers/vfs/protect.c` — `do_umask()` (line 180)
pub fn do_umask() -> i32 {
    ENOSYS
}

// =============================================================================
// Mount operations
// =============================================================================

/// Perform the `mount(special, path, rwflag, ...)` system call.
///
/// C source: `minix/servers/vfs/mount.c` — `do_mount()` (line 128)
pub fn do_mount() -> i32 {
    ENOSYS
}

/// Perform the `umount(special)` system call.
///
/// C source: `minix/servers/vfs/mount.c` — `do_umount()` (line 129)
pub fn do_umount() -> i32 {
    ENOSYS
}

/// Perform the `mapdriver(label, major, endpoint)` — register a device driver.
///
/// C source: `minix/servers/vfs/dmap.c` — `do_mapdriver()` (line 50)
pub fn do_mapdriver() -> i32 {
    ENOSYS
}

// =============================================================================
// Time operations
// =============================================================================

/// Perform the `utimens(path, times, flag)` system call (and its friends).
///
/// C source: `minix/servers/vfs/time.c` — `do_utimens()` (line 26)
pub fn do_utimens() -> i32 {
    ENOSYS
}

// =============================================================================
// Misc operations
// =============================================================================

/// Perform the `svrctl(request, arg)` — filesystem server control.
///
/// C source: `minix/servers/vfs/misc.c` — `do_svrctl()` (line 119)
pub fn do_svrctl() -> i32 {
    ENOSYS
}

/// Perform the `getsysinfo(what, where, size)` — copy VFS data structures.
///
/// C source: `minix/servers/vfs/misc.c` — `do_getsysinfo()` (line 120)
pub fn do_getsysinfo() -> i32 {
    ENOSYS
}

/// Perform VM-related calls from VFS.
///
/// C source: `minix/servers/vfs/misc.c` — `do_vm_call()` (line 121)
pub fn do_vm_call() -> i32 {
    ENOSYS
}

/// Perform the `getrusage(who, usage)` system call.
///
/// C source: `minix/servers/vfs/misc.c` — `do_getrusage()` (line 125)
pub fn do_getrusage() -> i32 {
    ENOSYS
}

/// Perform the `gcov_flush()` system call — flush gcov coverage data.
///
/// C source: `minix/servers/vfs/gcov.c` — `do_gcov_flush()` (line 322)
pub fn do_gcov_flush() -> i32 {
    ENOSYS
}

/// Check file access permissions for a given process.
///
/// C source: `minix/servers/vfs/path.c` — `do_checkperms()` (line 161)
pub fn do_checkperms() -> i32 {
    ENOSYS
}

// =============================================================================
// Lock operations
// =============================================================================

/// Perform advisory file locking for the `fcntl` `F_SETLK`/`F_SETLKW`/`F_GETLK` calls.
///
/// In the C source this is a helper called by `do_fcntl` with arguments
/// `(struct filp *f, int req)`. Once the FS request layer is wired, this
/// function will read the filp and request from the global scratchpad.
///
/// C source: `minix/servers/vfs/lock.c` — `lock_op()` (line 21)
pub fn lock_op() -> i32 {
    ENOSYS
}
