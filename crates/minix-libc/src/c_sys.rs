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

const EINVAL: i32 = 22;
const ENOMEM: i32 = 12;
const ENOSYS: i32 = 71;

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
#[repr(C)]
pub struct Dirent {
    d_ino: c_ulong,
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
    loop {
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
        return &mut d.cur;
    }
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
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn execve(
    path: *const c_char,
    argv: *const *const c_char,
    _envp: *const *const c_char,
) -> c_int {
    if path.is_null() || argv.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let mut argc = 0usize;
    while !unsafe { *argv.add(argc) }.is_null() {
        argc += 1;
    }
    let argv_slice = unsafe { core::slice::from_raw_parts(argv as *const *const u8, argc) };
    match minix_std::process::exec(path_bytes, argv_slice) {
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

/// Query system configuration values.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sysconf(name: c_int) -> c_long {
    match name {
        SC_ARG_MAX => 131_072,
        SC_PAGE_SIZE => 4096,
        SC_OPEN_MAX => OPEN_MAX as c_long,
        SC_CLK_TCK => 60,
        SC_GETPW_R_SIZE_MAX => 16_384,
        _ => {
            crate::set_errno(EINVAL);
            -1
        }
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

/// Get the current working directory. VFS has no getcwd call yet.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getcwd(_buf: *mut c_char, _size: usize) -> *mut c_char {
    crate::set_errno(ENOSYS);
    core::ptr::null_mut()
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

/// Change permissions on an open fd. VFS has no fd-based chmod call yet.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fchmod(_fd: c_int, _mode: c_uint) -> c_int {
    crate::fail(ENOSYS)
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

/// Fill the system identification struct (best effort: the kernel exposes
/// no version information to userland yet).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uname(buf: *mut Utsname) -> c_int {
    if buf.is_null() {
        return crate::fail(EINVAL);
    }
    let u = unsafe { &mut *buf };
    set_str(&mut u.sysname, b"Minix");
    set_str(&mut u.nodename, b"minix");
    set_str(&mut u.release, b"");
    set_str(&mut u.version, b"");
    set_str(&mut u.machine, b"x86_64");
    set_str(&mut u.domainname, b"");
    0
}

// ---- pwd.h ----

/// User id. Minix has no user accounts; everything runs as root.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getuid() -> c_int {
    0
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

/// Look up a user by name. Minix has no passwd database: no such user.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpwnam_r(
    _name: *const c_char,
    _pwd: *mut Passwd,
    _buf: *mut c_char,
    _buflen: usize,
    result: *mut *mut Passwd,
) -> c_int {
    if result.is_null() {
        return crate::fail(EINVAL);
    }
    unsafe { *result = core::ptr::null_mut() };
    crate::fail(ENOENT)
}

/// Look up a user by uid. Minix has no passwd database: no such user.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpwuid_r(
    _uid: c_int,
    _pwd: *mut Passwd,
    _buf: *mut c_char,
    _buflen: usize,
    result: *mut *mut Passwd,
) -> c_int {
    if result.is_null() {
        return crate::fail(EINVAL);
    }
    unsafe { *result = core::ptr::null_mut() };
    crate::fail(ENOENT)
}
