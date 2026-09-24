//! C POSIX system surface for LLVM's Unix support layer: `sys/mman.h`,
//! `dirent.h`, `sys/resource.h`, `sys/socket.h`, and the `unistd.h`
//! process functions.
//!
//! Everything that maps to a real minix syscall is implemented (mmap/
//! munmap via VM, fork/exec via PM, directory reading via VFS getdents).
//! The rest (sockets, mprotect, rlimits, setsid) returns ENOSYS — minix
//! has no equivalent for these yet, and rustc's `--gc-sections` link drops
//! the LLVM code paths that call them.

use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_ushort, c_void};

use crate::c_time::TimeT;

const EINVAL: i32 = 22;
const ENOMEM: i32 = 12;
/// C `errno.h` value, positive because this is what `fail` stores in `errno`.
const ENOSYS: i32 = 78;

// ---- sys/mman.h ----

// mmap/munmap are implemented in lib.rs (minix_std::vmem); only the
// unsupported calls live here.

/// Change mapping protections. VM has no prot-change call yet.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mprotect(_addr: *mut c_void, _length: usize, _prot: c_int) -> c_int {
    crate::fail(ENOSYS)
}

// ---- sys/resource.h ----

/// POSIX `struct rlimit`.
#[repr(C)]
pub struct Rlimit {
    rlim_cur: c_ulong,
    rlim_max: c_ulong,
}

const RLIM_INFINITY: c_ulong = c_ulong::MAX;
const RLIMIT_LAST: c_int = 9; // RLIMIT_AS

/// Get a resource limit. Minix enforces no per-process limits; report
/// `RLIM_INFINITY` for every known resource.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getrlimit(resource: c_int, rlim: *mut Rlimit) -> c_int {
    if rlim.is_null() {
        return crate::fail(EINVAL);
    }
    if resource < 0 || resource > RLIMIT_LAST {
        return crate::fail(EINVAL);
    }
    unsafe {
        (*rlim).rlim_cur = RLIM_INFINITY;
        (*rlim).rlim_max = RLIM_INFINITY;
    }
    0
}

/// Set a resource limit. Minix has no limits to enforce; accept and ignore.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setrlimit(_resource: c_int, _rlim: *const Rlimit) -> c_int {
    0
}

// ---- dirent.h ----

/// POSIX `struct dirent`, fixed-name form (d_name is NUL-terminated).
///
/// `d_ino` is 64-bit because that is what MINIX's `ino_t` is (`uint64_t` in
/// `sys/sys/types.h`) and what the getdents record below carries. `c_ulong`
/// coincided with it on every 64-bit target and silently truncated on wasm32.
#[repr(C)]
pub struct Dirent {
    d_ino: u64,
    d_off: c_long,
    d_reclen: c_ushort,
    d_type: u8,
    d_name: [c_char; 256],
}

/// Opaque-in-C directory stream: the fd plus the getdents buffer and the
/// current parsed entry (readdir must return a stable pointer).
#[repr(C)]
#[allow(clippy::upper_case_acronyms)]
pub struct DIR {
    fd: c_int,
    buf: [u8; 4096],
    buf_len: usize,
    off: usize,
    cur: Dirent,
}

// MFS getdents record layout (matches userland's `ls` parser):
//   d_fileno u64 @0, d_reclen u16 @8, d_namlen u16 @10, d_type u8 @12,
//   name bytes @13 (not NUL-terminated).
const DIRENT_NAME_OFF: usize = 13;

/// Open a directory for reading.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn opendir(path: *const c_char) -> *mut DIR {
    let fd = unsafe { crate::open(path, 0, 0) };
    if fd < 0 {
        return core::ptr::null_mut();
    }
    let d = unsafe { crate::malloc(core::mem::size_of::<DIR>()) } as *mut DIR;
    if d.is_null() {
        let _ = crate::close(fd);
        return core::ptr::null_mut();
    }
    unsafe {
        (*d).fd = fd;
        (*d).buf_len = 0;
        (*d).off = 0;
        (*d).cur = core::mem::zeroed();
    }
    d
}

/// Read the next directory entry, or NULL at the end.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn readdir(dirp: *mut DIR) -> *mut Dirent {
    let d = unsafe { &mut *dirp };
    if d.off >= d.buf_len {
        let n = match minix_std::fs::getdents(d.fd, &mut d.buf) {
            Ok(n) => n as usize,
            Err(_) => return core::ptr::null_mut(),
        };
        if n == 0 {
            return core::ptr::null_mut();
        }
        d.buf_len = n;
        d.off = 0;
    }
    let off = d.off;
    if off + DIRENT_NAME_OFF > d.buf_len {
        d.off = d.buf_len;
        return core::ptr::null_mut();
    }
    let reclen = u16::from_ne_bytes([d.buf[off + 8], d.buf[off + 9]]) as usize;
    if reclen == 0 || off + reclen > d.buf_len {
        return core::ptr::null_mut();
    }
    let namlen = u16::from_ne_bytes([d.buf[off + 10], d.buf[off + 11]]) as usize;
    let mut ino_bytes = [0u8; 8];
    ino_bytes.copy_from_slice(&d.buf[off..off + 8]);
    let ino = u64::from_ne_bytes(ino_bytes);
    let d_type = d.buf[off + 12];
    let name_len = namlen.min(255);
    let name = &d.buf[off + DIRENT_NAME_OFF..off + DIRENT_NAME_OFF + name_len];
    d.cur.d_ino = ino;
    d.cur.d_off = 0;
    d.cur.d_reclen = reclen as u16;
    d.cur.d_type = d_type;
    for (i, b) in name.iter().enumerate() {
        d.cur.d_name[i] = *b as c_char;
    }
    d.cur.d_name[name_len] = 0;
    d.off += reclen;
    &mut d.cur
}

/// Close a directory stream.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn closedir(dirp: *mut DIR) -> c_int {
    if dirp.is_null() {
        return crate::fail(EINVAL);
    }
    let fd = unsafe { (*dirp).fd };
    let _ = crate::close(fd);
    unsafe { crate::free(dirp as *mut c_void) };
    0
}

/// Rewind a directory stream to the first entry.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rewinddir(dirp: *mut DIR) {
    let d = unsafe { &mut *dirp };
    let _ = minix_std::fs::lseek(d.fd, 0, 0); // SEEK_SET
    d.buf_len = 0;
    d.off = 0;
}

// ---- sys/socket.h ----

// The socket family (socket/bind/connect/listen/accept/shutdown/send/
// recv/...) is implemented in lib.rs over minix_std::net; only
// `setsockopt` is unsupported (the net server has no options yet).

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setsockopt(
    _fd: c_int,
    _level: c_int,
    _optname: c_int,
    _optval: *const c_void,
    _optlen: c_ulong,
) -> c_int {
    crate::fail(ENOSYS)
}

// ---- unistd.h ----

// fork is implemented in lib.rs (minix_std::process::fork).

/// Replace the process image (PM→VFS exec chain). Only returns on error.
///
/// `envp` is the environment the new image starts with — bash and every other
/// shell pass theirs to each child. A null `envp` means an empty environment.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn execve(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    if path.is_null() || argv.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let mut argc = 0usize;
    while !unsafe { *argv.add(argc) }.is_null() {
        argc += 1;
    }
    // The kernel's frame holds 63 of each (minix-rt caps there too), so a
    // runaway array without its terminator cannot walk the address space.
    let mut envc = 0usize;
    if !envp.is_null() {
        while envc < 63 && !unsafe { *envp.add(envc) }.is_null() {
            envc += 1;
        }
    }
    let argv_slice = unsafe { core::slice::from_raw_parts(argv as *const *const u8, argc) };
    let envp_slice = unsafe { core::slice::from_raw_parts(envp as *const *const u8, envc) };
    match minix_std::process::exec(path_bytes, argv_slice, envp_slice) {
        Ok(_) => crate::fail(0), // exec never returns on success
        Err(e) => crate::fail(e.0),
    }
}

/// `execve` without an environment.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn execv(path: *const c_char, argv: *const *const c_char) -> c_int {
    unsafe { execve(path, argv, core::ptr::null()) }
}

/// Create a new session. Minix has no session syscall yet.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setsid() -> c_int {
    crate::fail(ENOSYS)
}

/// Page size (fixed 4 KiB).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpagesize() -> c_int {
    4096
}

const SC_ARG_MAX: c_int = 0;
const SC_PAGE_SIZE: c_int = 30;
const SC_OPEN_MAX: c_int = 4;
const SC_CLK_TCK: c_int = 2;
const SC_GETPW_R_SIZE_MAX: c_int = 69;
const OPEN_MAX: c_int = 32;

/// The clock tick rate `_SC_CLK_TCK` reports and `times()` counts in. One
/// number, because the two have to agree.
pub(crate) const CLK_TCK: i64 = 60;

/// Query system configuration values.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sysconf(name: c_int) -> c_long {
    match name {
        SC_ARG_MAX => 131_072,
        SC_PAGE_SIZE => 4096,
        SC_OPEN_MAX => OPEN_MAX as c_long,
        SC_CLK_TCK => CLK_TCK as c_long,
        SC_GETPW_R_SIZE_MAX => 16_384,
        _ => {
            crate::set_errno(EINVAL);
            -1
        }
    }
}

/// Device control (`ioctl(2)`).
///
/// The request number is the NetBSD/Linux-style encoding (direction, size and
/// a type letter in the high bits, the request in the low byte), so a driver
/// learns how to move the argument without knowing the request — see
/// `net::ioc_encode`. `arg` is whatever the request names: a pointer to the
/// struct, or nothing at all for the many requests whose size field is zero.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ioctl(fd: c_int, request: c_ulong, arg: *mut c_void) -> c_int {
    // The C request is an `unsigned long` and the encoding is 32 bits, so a
    // request that does not fit in one is not this system's.
    match unsafe { minix_std::fs::ioctl(fd, request as u32, arg as *mut u8) } {
        Ok(_) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Exit immediately without running atexit/__cxa handlers.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn _exit(status: c_int) -> ! {
    minix_std::process::exit(status);
}

/// C11 `_Exit`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn _Exit(status: c_int) -> ! {
    minix_std::process::exit(status);
}

/// Whether `fd` refers to the terminal. In the boot environment the standard
/// descriptors are connected to the console.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isatty(fd: c_int) -> c_int {
    if (0..=2).contains(&fd) { 1 } else { 0 }
}

/// Duplicate a string into a fresh malloc'd buffer.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strdup(s: *const c_char) -> *mut c_char {
    if s.is_null() {
        return core::ptr::null_mut();
    }
    let len = unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes().len();
    let p = unsafe { crate::malloc(len + 1) } as *mut c_char;
    if p.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { core::ptr::copy_nonoverlapping(s, p, len + 1) };
    p
}

/// Remove a file (VFS unlink).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    match minix_std::fs::unlink(path_bytes) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `remove` — unlink a file (POSIX `remove` on a regular file).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn remove(path: *const c_char) -> c_int {
    unsafe { unlink(path) }
}

// ---- sys/statvfs.h ----

/// Get filesystem statistics for a path (VFS statvfs → FS statvfs).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn statvfs(path: *const c_char, buf: *mut c_void) -> c_int {
    if path.is_null() || buf.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let path_str = match core::str::from_utf8(path_bytes) {
        Ok(s) => s,
        Err(_) => return crate::fail(EINVAL),
    };
    match minix_std::fs::statvfs(path_str) {
        Ok(st) => {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &st as *const minix_std::fs::Statvfs as *const u8,
                    buf as *mut u8,
                    core::mem::size_of::<minix_std::fs::Statvfs>(),
                )
            };
            0
        }
        Err(e) => crate::fail(e.0),
    }
}

/// Get filesystem statistics for an open file descriptor.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstatvfs(fd: c_int, buf: *mut c_void) -> c_int {
    if buf.is_null() {
        return crate::fail(EINVAL);
    }
    match minix_std::fs::fstatvfs(fd) {
        Ok(st) => {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &st as *const minix_std::fs::Statvfs as *const u8,
                    buf as *mut u8,
                    core::mem::size_of::<minix_std::fs::Statvfs>(),
                )
            };
            0
        }
        Err(e) => crate::fail(e.0),
    }
}

// ---- unistd.h path operations ----

/// `struct dirent`'s `d_name` width: a name this walk must be able to hold.
const CWD_NAME_MAX: usize = 255;

/// Prepend `name` and a `/` to the path being built, which grows downward
/// from the end of the caller's buffer towards `buf`. `None` when it would
/// overrun — the caller reports `ERANGE`.
///
/// # Safety
///
/// `buf` and `p` must point into one allocation, with `buf <= p`, and the
/// bytes in `buf .. p` must be the caller's to write.
unsafe fn cwd_prepend(buf: *mut c_char, p: *mut c_char, name: &[u8]) -> Option<*mut c_char> {
    let mut q = p;
    for &byte in name.iter().rev() {
        q = unsafe { q.sub(1) };
        if q < buf {
            return None;
        }
        unsafe { *q = byte as c_char };
    }
    q = unsafe { q.sub(1) };
    if q < buf {
        return None;
    }
    unsafe { *q = b'/' as c_char };
    Some(q)
}

/// Undo the walk's `chdir("..")`s by re-descending the components of the path
/// built so far, leaving the process where it started. `Err` carries the
/// errno of the component it could not re-enter.
///
/// # Safety
///
/// `path` must point at the NUL-terminated pathname built by
/// [`cwd_prepend`], and the process must be at the root of the filesystem —
/// which is where the walk leaves it.
#[cfg(target_os = "minix")]
unsafe fn cwd_recover(path: *mut c_char) -> Result<(), i32> {
    // The caller's errno describes why the walk gave up; the chdirs below
    // would otherwise overwrite it.
    let saved = unsafe { *crate::__errno_location() };
    let mut p = path;
    while unsafe { *p } != 0 {
        p = unsafe { p.add(1) };
        let start = p;
        while unsafe { *p } != 0 && unsafe { *p } as u8 != b'/' {
            p = unsafe { p.add(1) };
        }
        let slash = unsafe { *p };
        unsafe { *p = 0 };
        let component = unsafe { core::ffi::CStr::from_ptr(start) }.to_bytes();
        let outcome = minix_std::fs::chdir(component);
        unsafe { *p = slash };
        if let Err(e) = outcome {
            return Err(e.0);
        }
    }
    crate::set_errno(saved);
    Ok(())
}

/// Scan an open `".."` for the entry naming `want`, writing its name into
/// `name` (NUL-terminated) and returning the name's length.
///
/// Inode numbers are only comparable within one device, so when the parent is
/// on a different one — the directory is a mount point — the entry is
/// identified by an `lstat` of its joined path instead.
///
/// # Safety
///
/// `dir` must be a stream [`opendir`] returned, and `name` must hold the
/// bytes of the longest name compared against.
#[cfg(target_os = "minix")]
unsafe fn cwd_entry(
    dir: *mut DIR,
    want: &minix_std::fs::Stat,
    same_dev: bool,
    name: &mut [u8],
) -> Option<usize> {
    loop {
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            return None;
        }
        let mut len = 0usize;
        while len + 1 < name.len() && unsafe { (*entry).d_name[len] } != 0 {
            name[len] = unsafe { (*entry).d_name[len] } as u8;
            len += 1;
        }
        name[len] = 0;
        if name[..len] == *b"." || name[..len] == *b".." {
            continue;
        }
        let found = if same_dev {
            unsafe { (*entry).d_ino == want.st_ino }
        } else {
            let mut joined = [0u8; 3 + CWD_NAME_MAX + 1];
            joined[..3].copy_from_slice(b"../");
            joined[3..3 + len].copy_from_slice(&name[..len]);
            joined[3 + len] = 0;
            match core::str::from_utf8(&joined[..3 + len]) {
                Ok(path) => match minix_std::fs::lstat(path) {
                    Ok(st) => st.st_dev == want.st_dev && st.st_ino == want.st_ino,
                    Err(_) => false,
                },
                Err(_) => false,
            }
        };
        if found {
            return Some(len);
        }
    }
}

/// The port's pathname ceiling (`unistd.h`'s `PATH_MAX`). No walk can build a
/// longer result, so this is where the allocate-form retry stops.
const CWD_PATH_MAX: usize = 4096;

/// `getcwd(3)`: the absolute pathname of the current directory.
///
/// VFS keeps a directory's vnode, not its name, so the name is recovered by
/// walking up: `stat` of `"."` and `".."` identifies the directory, a scan of
/// `".."` finds which entry holds that inode, and `chdir("..")` moves up for
/// the next round. Root is where the two stats agree. Ported from MINIX's
/// `__getcwd` (`minix/lib/libc/sys/__getcwd.c`), including its duty to put the
/// caller back: the walk finishes at the root, and the components it found are
/// re-descended before it returns, so even a failure leaves the caller where
/// it started.
///
/// A NULL `buf` asks for the result to be `malloc`ed, as glibc allows and as
/// bash uses (`getcwd(0, PATH_MAX)`); the caller frees it.
///
/// The walk moves the process's own cwd, so two threads calling this at once
/// can cross — the same property MINIX's implementation has.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getcwd(buf: *mut c_char, size: usize) -> *mut c_char {
    if !buf.is_null() {
        if size <= 1 {
            crate::set_errno(EINVAL);
            return core::ptr::null_mut();
        }
        return match unsafe { cwd_walk(buf, size) } {
            Ok(()) => buf,
            Err(errno) => {
                crate::set_errno(errno);
                core::ptr::null_mut()
            }
        };
    }

    // The allocate form. Grow on ERANGE the way glibc does, up to the path
    // ceiling — past that the walk itself could not have produced the longer
    // result it is asking for.
    let mut cap = if size == 0 {
        256
    } else {
        size.min(CWD_PATH_MAX)
    };
    loop {
        let alloc = unsafe { crate::malloc(cap) } as *mut c_char;
        if alloc.is_null() {
            crate::set_errno(ENOMEM);
            return core::ptr::null_mut();
        }
        match unsafe { cwd_walk(alloc, cap) } {
            Ok(()) => return alloc,
            Err(ERANGE) if cap < CWD_PATH_MAX => {
                unsafe { crate::free(alloc as *mut c_void) };
                cap = (cap * 2).min(CWD_PATH_MAX);
            }
            Err(errno) => {
                unsafe { crate::free(alloc as *mut c_void) };
                crate::set_errno(errno);
                return core::ptr::null_mut();
            }
        }
    }
}

/// The walk behind [`getcwd`]: fill `buf`, `size` bytes, with the current
/// directory's absolute pathname. `Err` carries the errno to report, and the
/// caller's cwd is restored on every path.
#[cfg(target_os = "minix")]
unsafe fn cwd_walk(buf: *mut c_char, size: usize) -> Result<(), i32> {
    let mut current = minix_std::fs::stat(".").map_err(|e| e.0)?;

    // The path is built backwards, from the buffer's NUL upwards.
    let mut p = unsafe { buf.add(size - 1) };
    unsafe { *p = 0 };

    loop {
        let above = match minix_std::fs::stat("..") {
            Ok(st) => st,
            Err(e) => {
                unsafe { cwd_recover(p) }?;
                return Err(e.0);
            }
        };
        if above.st_dev == current.st_dev && above.st_ino == current.st_ino {
            break; // The parent is this directory, so this directory is the root.
        }
        let dir = unsafe { opendir(b"..\0".as_ptr() as *const c_char) };
        if dir.is_null() {
            unsafe { cwd_recover(p) }?;
            return Err(ENOENT);
        }
        let mut name = [0u8; CWD_NAME_MAX + 1];
        let found = unsafe { cwd_entry(dir, &current, above.st_dev == current.st_dev, &mut name) };
        unsafe { closedir(dir) };
        let len = match found {
            Some(len) => len,
            None => {
                unsafe { cwd_recover(p) }?;
                return Err(ENOENT);
            }
        };
        p = match unsafe { cwd_prepend(buf, p, &name[..len]) } {
            Some(outer) => outer,
            None => {
                unsafe { cwd_recover(p) }?;
                return Err(ERANGE);
            }
        };
        if let Err(e) = minix_std::fs::chdir(b"..") {
            unsafe { cwd_recover(p) }?;
            return Err(e.0);
        }
        current = above;
    }

    unsafe { cwd_recover(p) }?;
    if unsafe { *p } == 0 {
        // Nothing was added: the directory is the root itself.
        p = unsafe { p.sub(1) };
        unsafe { *p = b'/' as c_char };
    }
    if p != buf {
        let len = unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes().len();
        unsafe { core::ptr::copy(p, buf, len + 1) };
    }
    Ok(())
}

/// POSIX `fchdir()`: change directory to the one `fd` refers to (VFS_FCHDIR).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn fchdir(fd: c_int) -> c_int {
    match minix_std::fs::fchdir(fd) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// POSIX `rename()`: rename `old` to `new` (VFS_RENAME).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rename(old: *const c_char, new: *const c_char) -> c_int {
    if old.is_null() || new.is_null() {
        return crate::fail(EINVAL);
    }
    let old_bytes = unsafe { core::ffi::CStr::from_ptr(old) }.to_bytes();
    let new_bytes = unsafe { core::ffi::CStr::from_ptr(new) }.to_bytes();
    match minix_std::fs::rename(old_bytes, new_bytes) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Change the current working directory (VFS chdir).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chdir(path: *const c_char) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    match minix_std::fs::chdir(path_bytes) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Create a hard link (VFS link).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn link(old: *const c_char, new: *const c_char) -> c_int {
    if old.is_null() || new.is_null() {
        return crate::fail(EINVAL);
    }
    let old_b = unsafe { core::ffi::CStr::from_ptr(old) }.to_bytes();
    let new_b = unsafe { core::ffi::CStr::from_ptr(new) }.to_bytes();
    match minix_std::fs::link(old_b, new_b) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Create a symbolic link. VFS has no symlink call yet.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn symlink(_target: *const c_char, _linkpath: *const c_char) -> c_int {
    crate::fail(ENOSYS)
}

/// Truncate an open file (VFS truncate).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ftruncate(fd: c_int, length: c_long) -> c_int {
    match minix_std::fs::truncate(fd, length as i64) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Create a special file (`mknod(2)`): a FIFO, or a device node when the
/// caller is privileged enough for one.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mknod(path: *const c_char, mode: c_uint, dev: c_uint) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    if path_bytes.is_empty() {
        return crate::fail(ENOENT); // X/Open requirement
    }
    match minix_std::fs::mknod(path_bytes, mode, dev) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Create a FIFO (`mkfifo(3)`), which is `mknod` with the FIFO type bits.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mkfifo(path: *const c_char, mode: c_uint) -> c_int {
    unsafe { mknod(path, mode | minix_std::fs::S_IFIFO, 0) }
}

/// Create a directory (VFS mkdir).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mkdir(path: *const c_char, mode: c_uint) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    match minix_std::fs::mkdir(path_bytes, mode) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Check file access permissions (VFS access).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn access(path: *const c_char, mode: c_int) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    match minix_std::fs::access(path_bytes, mode as u32) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Change file permissions (VFS chmod).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chmod(path: *const c_char, mode: c_uint) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    match minix_std::fs::chmod(path_bytes, mode) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Change permissions on an open fd (VFS fchmod → the filp's vnode, routed
/// to the owning FS — PFS for pipe fds).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fchmod(fd: c_int, mode: c_uint) -> c_int {
    match minix_std::fs::fchmod(fd, mode) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Set the file mode creation mask (VFS umask); returns the previous mask.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umask(mask: c_uint) -> c_uint {
    match minix_std::fs::umask(mask) {
        Ok(old) => old,
        Err(e) => {
            crate::set_errno(e.0);
            0
        }
    }
}

/// Synchronize an mmap'd region. VM mappings are private and coherent;
/// there is no write-back cache to flush.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn msync(_addr: *mut c_void, _length: usize, _flags: c_int) -> c_int {
    0
}

/// Page-advice hint. VM has no advice call; the OS pager ignores hints.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn madvise(_addr: *mut c_void, _length: usize, _advice: c_int) -> c_int {
    0
}

/// POSIX shared-memory open. Minix uses System V shm (shmget/shmat); the
/// POSIX name-based interface is not supported.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn shm_open(_name: *const c_char, _oflag: c_int, _mode: c_uint) -> c_int {
    crate::fail(ENOSYS)
}

/// POSIX shared-memory unlink.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn shm_unlink(_name: *const c_char) -> c_int {
    crate::fail(ENOSYS)
}

/// Read a symbolic link target. VFS has no symlink support yet.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn readlink(
    _path: *const c_char,
    _buf: *mut c_char,
    _bufsiz: usize,
) -> isize {
    crate::fail(ENOSYS) as isize
}

/// Change an fd's owner. VFS chown is path-based only.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fchown(_fd: c_int, _owner: c_uint, _group: c_uint) -> c_int {
    crate::fail(ENOSYS)
}

/// Sleep for `usec` microseconds (busy-wait on the monotonic clock — the
/// PM has no nanosleep call yet).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn usleep(usec: c_uint) -> c_int {
    let deadline = match minix_std::time::clock_gettime(1) {
        // CLOCK_MONOTONIC = 1
        Ok(t) => (t.tv_sec as u128) * 1_000_000 + (t.tv_nsec as u128) / 1000 + usec as u128,
        Err(_) => usec as u128,
    };
    loop {
        if let Ok(t) = minix_std::time::clock_gettime(1) {
            let now = (t.tv_sec as u128) * 1_000_000 + (t.tv_nsec as u128) / 1000;
            if now >= deadline {
                return 0;
            }
        }
        core::hint::spin_loop();
    }
}

// ---- stdlib.h realpath ----

/// Canonicalize an absolute path: collapse `.`/`..` and verify it exists.
/// Relative paths fail (no getcwd to resolve against).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn realpath(path: *const c_char, resolved: *mut c_char) -> *mut c_char {
    if path.is_null() {
        crate::set_errno(EINVAL);
        return core::ptr::null_mut();
    }
    let bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    if bytes.is_empty() || bytes[0] != b'/' {
        crate::set_errno(ENOENT);
        return core::ptr::null_mut();
    }
    let mut out = [0u8; 4096];
    let mut n = 1usize;
    out[0] = b'/';
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i] == b'/' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && bytes[i] != b'/' {
            i += 1;
        }
        let comp = &bytes[start..i];
        if comp == b"." {
            continue;
        } else if comp == b".." {
            if n > 1 {
                n -= 1;
                while n > 0 && out[n] != b'/' {
                    n -= 1;
                }
                if n == 0 {
                    out[0] = b'/';
                    n = 1;
                }
            }
        } else {
            if n > 1 {
                out[n] = b'/';
                n += 1;
            }
            if n + comp.len() >= out.len() {
                crate::set_errno(36); // ENAMETOOLONG
                return core::ptr::null_mut();
            }
            out[n..n + comp.len()].copy_from_slice(comp);
            n += comp.len();
        }
    }
    let path_str = match core::str::from_utf8(&out[..n]) {
        Ok(s) => s,
        Err(_) => {
            crate::set_errno(EINVAL);
            return core::ptr::null_mut();
        }
    };
    if minix_std::fs::lstat(path_str).is_err() {
        crate::set_errno(ENOENT);
        return core::ptr::null_mut();
    }
    let dst = if resolved.is_null() {
        let p = unsafe { crate::malloc(n + 1) } as *mut c_char;
        if p.is_null() {
            return core::ptr::null_mut();
        }
        p
    } else {
        resolved
    };
    unsafe {
        core::ptr::copy_nonoverlapping(out.as_ptr() as *const c_char, dst, n);
        *dst.add(n) = 0;
    }
    dst
}

// ---- dlfcn.h dladdr ----

/// `Dl_info` for `dladdr`.
#[repr(C)]
pub struct DlInfo {
    dli_fname: *const c_char,
    dli_fbase: *mut c_void,
    dli_sname: *const c_char,
    dli_saddr: *mut c_void,
}

/// Resolve an address to a symbol. Minix has no dynamic symbol table, so
/// this reports "not found" (LLVM's GetMainExecutable falls back to "").
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dladdr(_addr: *mut c_void, _info: *mut DlInfo) -> c_int {
    0
}

// ---- sys/utsname.h ----

/// `struct utsname` — matches sys/utsname.h.
#[repr(C)]
pub struct Utsname {
    sysname: [c_char; 65],
    nodename: [c_char; 65],
    release: [c_char; 65],
    version: [c_char; 65],
    machine: [c_char; 65],
    domainname: [c_char; 65],
}

fn set_str(field: &mut [c_char; 65], s: &[u8]) {
    let n = s.len().min(field.len() - 1);
    for (i, b) in s[..n].iter().enumerate() {
        field[i] = *b as c_char;
    }
    field[n] = 0;
}

/// Fill the system identification struct from PM_SYSUNAME.
///
/// Each served field comes from PM's `uts_tbl`, so `machine` matches the
/// architecture (the previous hardcoded version said x86_64 everywhere)
/// and release/version come from the same table. `domainname` is not
/// served by PM (C's `uts_tbl` has no entry for it) — left empty. Fields
/// that PM rejects stay empty rather than failing the whole call.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uname(buf: *mut Utsname) -> c_int {
    if buf.is_null() {
        return crate::fail(EINVAL);
    }
    // Zero the whole struct so unserved fields are empty strings.
    unsafe {
        core::ptr::write_bytes(buf as *mut u8, 0, core::mem::size_of::<Utsname>());
    }
    let u = unsafe { &mut *buf };
    let fetch = |field: i32, dst: &mut [c_char; 65]| {
        let r =
            unsafe { minix_std::process::sysuname(field, dst.as_mut_ptr() as *mut u8, dst.len()) };
        if r.is_err() {
            set_str(dst, b"");
        }
    };
    fetch(minix_std::process::UTS_SYSNAME, &mut u.sysname);
    fetch(minix_std::process::UTS_NODENAME, &mut u.nodename);
    fetch(minix_std::process::UTS_RELEASE, &mut u.release);
    fetch(minix_std::process::UTS_VERSION, &mut u.version);
    fetch(minix_std::process::UTS_MACHINE, &mut u.machine);
    0
}

// ---- utime.h / sys/time.h timestamps (VFS_UTIMENS → FS fs_utime) ----

/// C `struct utimbuf` (utime.h).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Utimbuf {
    pub actime: TimeT,
    pub modtime: TimeT,
}

/// Set a file's access/modification times (`utime(2)`).
///
/// `times == NULL` stamps both fields with the current time (UTIME_NOW).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn utime(path: *const c_char, times: *const Utimbuf) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    if path_bytes.is_empty() {
        return crate::fail(ENOENT); // X/Open requirement
    }
    let (actime, modtime, acnsec, mnsec) = if times.is_null() {
        (0, 0, minix_std::fs::UTIME_NOW, minix_std::fs::UTIME_NOW)
    } else {
        let t = unsafe { &*times };
        (t.actime, t.modtime, 0, 0)
    };
    match unsafe { minix_std::fs::utime(path_bytes, actime, modtime, acnsec, mnsec) } {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Set a file's access/modification times (`utimes(2)`, microsecond).
///
/// `tv == NULL` stamps both fields with the current time.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn utimes(
    path: *const c_char,
    tv: *const [crate::c_time::TimeVal; 2],
) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    if path_bytes.is_empty() {
        return crate::fail(ENOENT); // X/Open requirement
    }
    let (actime, modtime, acnsec, mnsec) = if tv.is_null() {
        (0, 0, minix_std::fs::UTIME_NOW, minix_std::fs::UTIME_NOW)
    } else {
        let t = unsafe { &*tv };
        (
            t[0].tv_sec,
            t[1].tv_sec,
            t[0].tv_usec * 1000,
            t[1].tv_usec * 1000,
        )
    };
    match unsafe { minix_std::fs::utime(path_bytes, actime, modtime, acnsec, mnsec) } {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

// ---- pwd.h / unistd.h credentials ----

/// Real user id (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getuid() -> c_int {
    match minix_std::process::getuid() {
        Ok((ruid, _)) => ruid,
        Err(e) => crate::fail(e.0),
    }
}

/// Effective user id (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn geteuid() -> c_int {
    match minix_std::process::getuid() {
        Ok((_, euid)) => euid,
        Err(e) => crate::fail(e.0),
    }
}

/// Real group id (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getgid() -> c_int {
    match minix_std::process::getgid() {
        Ok((rgid, _)) => rgid,
        Err(e) => crate::fail(e.0),
    }
}

/// Effective group id (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getegid() -> c_int {
    match minix_std::process::getgid() {
        Ok((_, egid)) => egid,
        Err(e) => crate::fail(e.0),
    }
}

/// Process group id of `pid` (0 = self) (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getsid(pid: c_int) -> c_int {
    match minix_std::process::getsid(pid) {
        Ok(sid) => sid,
        Err(e) => crate::fail(e.0),
    }
}

/// True when running with setuid/setgid taint (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn issetugid() -> c_int {
    match minix_std::process::issetugid() {
        Ok(t) => t as c_int,
        Err(e) => crate::fail(e.0),
    }
}

/// Fetch supplemental groups. `size == 0` is a count query (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getgroups(size: c_int, list: *mut c_int) -> c_int {
    if size < 0 {
        return crate::fail(EINVAL);
    }
    if size == 0 || list.is_null() {
        return match minix_std::process::getgroups(&mut []) {
            Ok(n) => n,
            Err(e) => crate::fail(e.0),
        };
    }
    let n = (size as usize).min(minix_std::process::NGROUPS_MAX);
    let mut buf = [0i32; minix_std::process::NGROUPS_MAX];
    match minix_std::process::getgroups(&mut buf[..n]) {
        Ok(count) => {
            for i in 0..count {
                unsafe { *list.add(i as usize) = buf[i as usize] };
            }
            count
        }
        Err(e) => crate::fail(e.0),
    }
}

/// Set the real and effective user id (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setuid(uid: c_int) -> c_int {
    match minix_std::process::setuid(uid) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Set the effective user id only (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn seteuid(uid: c_int) -> c_int {
    match minix_std::process::seteuid(uid) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Set the real and effective group id (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setgid(gid: c_int) -> c_int {
    match minix_std::process::setgid(gid) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Set the effective group id only (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setegid(gid: c_int) -> c_int {
    match minix_std::process::setegid(gid) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// Set supplemental groups (PM; root-only).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setgroups(size: usize, list: *const c_int) -> c_int {
    if size > minix_std::process::NGROUPS_MAX {
        return crate::fail(EINVAL);
    }
    if size > 0 && list.is_null() {
        return crate::fail(EINVAL);
    }
    let mut buf = [0i32; minix_std::process::NGROUPS_MAX];
    for i in 0..size {
        buf[i] = unsafe { *list.add(i) };
    }
    match minix_std::process::setgroups(&buf[..size]) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `struct passwd` — matches pwd.h.
#[repr(C)]
pub struct Passwd {
    pw_name: *mut c_char,
    pw_passwd: *mut c_char,
    pw_uid: c_uint,
    pw_gid: c_uint,
    pw_gecos: *mut c_char,
    pw_dir: *mut c_char,
    pw_shell: *mut c_char,
}

const ENOENT: i32 = 2;
const ERANGE: i32 = 34;
const ENOTTY: i32 = 25;

/// Fill a `Passwd` from a parsed entry whose field slices live in `buf`;
/// NUL-terminates each field in place and points the struct fields at them.
/// Returns ERANGE when a field ends at the buffer's last byte (no room for
/// its NUL terminator).
///
/// # Safety
///
/// `pwd` must be a valid pointer; `buf` must be the buffer the entry's
/// field slices borrow from, with `buflen` its length.
#[cfg(target_os = "minix")]
unsafe fn fill_passwd(
    pwd: *mut Passwd,
    buf: *mut c_char,
    buflen: usize,
    entry: &minix_std::passwd::PasswdEntry,
) -> c_int {
    let base = buf as usize;
    unsafe {
        (*pwd).pw_uid = entry.uid as c_uint;
        (*pwd).pw_gid = entry.gid as c_uint;
    }
    let fields: [(&[u8], *mut *mut c_char); 5] = unsafe {
        [
            (entry.name, core::ptr::addr_of_mut!((*pwd).pw_name)),
            (entry.passwd, core::ptr::addr_of_mut!((*pwd).pw_passwd)),
            (entry.gecos, core::ptr::addr_of_mut!((*pwd).pw_gecos)),
            (entry.dir, core::ptr::addr_of_mut!((*pwd).pw_dir)),
            (entry.shell, core::ptr::addr_of_mut!((*pwd).pw_shell)),
        ]
    };
    for (f, slot) in fields {
        let off = f.as_ptr() as usize - base;
        // NUL-terminate the field (overwrites the ':'/'\n' separator).
        if off + f.len() >= buflen {
            return ERANGE;
        }
        unsafe {
            *buf.add(off + f.len()) = 0;
            *slot = buf.add(off);
        }
    }
    0
}

/// Look up a user by name in /etc/passwd (reentrant POSIX contract).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpwnam_r(
    name: *const c_char,
    pwd: *mut Passwd,
    buf: *mut c_char,
    buflen: usize,
    result: *mut *mut Passwd,
) -> c_int {
    if result.is_null() || pwd.is_null() || buf.is_null() {
        return crate::fail(EINVAL);
    }
    unsafe { *result = core::ptr::null_mut() };
    if name.is_null() {
        return crate::fail(EINVAL);
    }
    let name_bytes = unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes();
    let data = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), buflen) };
    let n = match unsafe { minix_std::passwd::read_passwd(data) } {
        Ok(n) => n,
        Err(e) => return crate::fail(e.0),
    };
    let entry = match minix_std::passwd::find_passwd(&data[..n], name_bytes) {
        Some(e) => e,
        None => return crate::fail(ENOENT),
    };
    let r = unsafe { fill_passwd(pwd, buf, buflen, &entry) };
    if r != 0 {
        return crate::fail(r);
    }
    unsafe { *result = pwd };
    0
}

/// Look up a user by uid in /etc/passwd (reentrant POSIX contract).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpwuid_r(
    uid: c_int,
    pwd: *mut Passwd,
    buf: *mut c_char,
    buflen: usize,
    result: *mut *mut Passwd,
) -> c_int {
    if result.is_null() || pwd.is_null() || buf.is_null() {
        return crate::fail(EINVAL);
    }
    unsafe { *result = core::ptr::null_mut() };
    if uid < 0 {
        return crate::fail(EINVAL);
    }
    let data = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), buflen) };
    let n = match unsafe { minix_std::passwd::read_passwd(data) } {
        Ok(n) => n,
        Err(e) => return crate::fail(e.0),
    };
    let entry = match minix_std::passwd::find_passwd_uid(&data[..n], uid as u32) {
        Some(e) => e,
        None => return crate::fail(ENOENT),
    };
    let r = unsafe { fill_passwd(pwd, buf, buflen, &entry) };
    if r != 0 {
        return crate::fail(r);
    }
    unsafe { *result = pwd };
    0
}

/// Shared storage for the non-reentrant lookups: POSIX defines `getpwuid` and
/// `getpwnam` as returning a pointer into a static area, so they are not
/// thread-safe by contract. One entry and one buffer serve both.
const PWD_BUF_LEN: usize = 1024;
#[cfg(target_os = "minix")]
static mut PWD_BUF: [c_char; PWD_BUF_LEN] = [0; PWD_BUF_LEN];
#[cfg(target_os = "minix")]
static mut PWD_ENTRY: Passwd = Passwd {
    pw_name: core::ptr::null_mut(),
    pw_passwd: core::ptr::null_mut(),
    pw_uid: 0,
    pw_gid: 0,
    pw_gecos: core::ptr::null_mut(),
    pw_dir: core::ptr::null_mut(),
    pw_shell: core::ptr::null_mut(),
};

/// POSIX `getpwuid`: the non-reentrant form, over `getpwuid_r` and the static
/// area above. NULL on error with `errno` set - including ENOENT when there is
/// no such uid - which is exactly what callers test for.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpwuid(uid: c_int) -> *mut Passwd {
    let mut result: *mut Passwd = core::ptr::null_mut();
    let r = unsafe {
        getpwuid_r(
            uid,
            core::ptr::addr_of_mut!(PWD_ENTRY),
            core::ptr::addr_of_mut!(PWD_BUF).cast::<c_char>(),
            PWD_BUF_LEN,
            &mut result,
        )
    };
    if r != 0 {
        core::ptr::null_mut()
    } else {
        result
    }
}

/// POSIX `getpwnam`: the non-reentrant form of `getpwnam_r`, same contract as
/// `getpwuid` above.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpwnam(name: *const c_char) -> *mut Passwd {
    let mut result: *mut Passwd = core::ptr::null_mut();
    let r = unsafe {
        getpwnam_r(
            name,
            core::ptr::addr_of_mut!(PWD_ENTRY),
            core::ptr::addr_of_mut!(PWD_BUF).cast::<c_char>(),
            PWD_BUF_LEN,
            &mut result,
        )
    };
    if r != 0 {
        core::ptr::null_mut()
    } else {
        result
    }
}

/// POSIX `sleep(seconds)`: whole seconds, returning the unslept remainder.
///
/// The port's `usleep` is a busy-wait on the monotonic clock (PM has no
/// nanosleep call yet), so this loops in chunks rather than multiplying a
/// large argument into its microsecond parameter. Nothing interrupts the wait,
/// which is why the return is always 0.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sleep(seconds: c_uint) -> c_uint {
    let mut left = seconds;
    while left > 0 {
        let chunk = if left > 1000 { 1000 } else { left };
        unsafe { usleep(chunk * 1_000_000) };
        left -= chunk;
    }
    0
}

/// POSIX `ttyname(fd)`: the name of the terminal open on `fd`, or NULL with
/// `errno = ENOTTY` when `fd` is not a terminal.
///
/// This OS has one console terminal per process, so the name is `/dev/tty` -
/// the device bash tries first anyway, and the one its `check_dev_tty` falls
/// back to when that open fails.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyname(fd: c_int) -> *mut c_char {
    if unsafe { isatty(fd) } == 0 {
        crate::set_errno(ENOTTY);
        return core::ptr::null_mut();
    }
    static mut TTYNAME_BUF: [c_char; 9] = [47, 100, 101, 118, 47, 116, 116, 121, 0];
    core::ptr::addr_of_mut!(TTYNAME_BUF).cast::<c_char>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Positive, because this is the value `fail` stores in `errno`, not a
    /// syscall return. C `errno.h` puts `ENOSYS` at 78; this had 71.
    #[test]
    fn test_enosys_matches_c() {
        assert_eq!(ENOSYS, 78);
    }

    /// `cwd_prepend` writes downward from the end of the caller's buffer, so
    /// the boundary it must stop at is the buffer's own start: one byte past
    /// it would be a write outside the caller's object.
    #[test]
    fn cwd_prepend_builds_the_path_backwards() {
        // A 5-byte buffer holds "/etc": four characters and the terminator.
        let mut buf = [0u8; 5];
        let p = unsafe { buf.as_mut_ptr().add(4) } as *mut c_char;
        unsafe { *p = 0 };
        let start = unsafe { cwd_prepend(buf.as_mut_ptr() as *mut c_char, p, b"etc") }
            .expect("5 bytes hold /etc");
        assert_eq!(
            unsafe { core::ffi::CStr::from_ptr(start) }.to_bytes(),
            b"/etc"
        );
    }

    /// One byte short of `/etc`. The walk may leave the buffer half-written —
    /// it discovers the overrun only as it reaches it — but it must not write
    /// below the caller's object, which is what the guard bytes here are for.
    #[test]
    fn cwd_prepend_refuses_to_overrun_the_buffer() {
        let mut arena = [0u8; 8];
        // Four guard bytes, then the four the caller offered.
        let buf = unsafe { arena.as_mut_ptr().add(4) } as *mut c_char;
        let p = unsafe { buf.add(3) };
        unsafe { *p = 0 };
        assert_eq!(unsafe { cwd_prepend(buf, p, b"etc") }, None);
        assert_eq!(arena[..4], [0u8; 4], "wrote below the caller's buffer");
    }
}
