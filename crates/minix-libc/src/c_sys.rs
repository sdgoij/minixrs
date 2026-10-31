//! C POSIX system surface for LLVM's Unix support layer: `sys/mman.h`,
//! `dirent.h`, `sys/resource.h`, `sys/socket.h`, and the `unistd.h`
//! process functions.
//!
//! Everything that maps to a real minix syscall is implemented (mmap/
//! munmap/mprotect via VM, fork/exec via PM, directory reading via VFS
//! getdents). The rest (sockets, rlimits, setsid) returns ENOSYS — minix
//! has no equivalent for these yet, and rustc's `--gc-sections` link drops
//! the LLVM code paths that call them.

use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_ushort, c_void};

use crate::c_time::TimeT;

const EINVAL: i32 = 22;
const ENOMEM: i32 = 12;
const EFAULT: i32 = 14;
const EBADF: i32 = 9;
const EIO: i32 = 5;
/// C `errno.h` value, positive because this is what `fail` stores in `errno`.
const ENOSYS: i32 = 78;

// ---- sys/mman.h ----

// mmap/munmap are implemented in lib.rs (minix_std::vmem); only the
// unsupported calls live here.

/// Change mapping protections (VM's `VM_MPROTECT`, see `minix_std::vmem`).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mprotect(addr: *mut c_void, length: usize, prot: c_int) -> c_int {
    match unsafe { minix_std::vmem::mprotect(addr as *mut u8, length, prot) } {
        Ok(()) => 0,
        Err(e) => crate::fail(-e),
    }
}

// ---- sys/resource.h ----

/// POSIX `struct rlimit`.
#[repr(C)]
pub struct Rlimit {
    rlim_cur: c_ulong,
    rlim_max: c_ulong,
}

const RLIM_INFINITY: c_ulong = c_ulong::MAX;
/// Highest resource number `getrlimit` accepts (`RLIMIT_RTTIME`). Minix
/// enforces none of them; the numbers above 9 exist because `rustix`'s
/// `Rlimit` table names them, in the header's numbering.
const RLIMIT_LAST: c_int = 15;

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

/// POSIX `scandir()`: the whole directory as a malloc'd array of `struct
/// dirent *`, each a copy (readdir reuses its buffer), filtered and sorted.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scandir(
    dirpath: *const c_char,
    namelist: *mut *mut *mut Dirent,
    filter: Option<unsafe extern "C" fn(*const Dirent) -> c_int>,
    compar: Option<unsafe extern "C" fn(*const *const Dirent, *const *const Dirent) -> c_int>,
) -> c_int {
    if dirpath.is_null() || namelist.is_null() {
        return crate::fail(EINVAL);
    }
    let dir = unsafe { opendir(dirpath) };
    if dir.is_null() {
        return -1;
    }
    let mut entries: *mut *mut Dirent = core::ptr::null_mut();
    let mut count = 0usize;
    let mut cap = 0usize;
    loop {
        let de = unsafe { readdir(dir) };
        if de.is_null() {
            break;
        }
        if let Some(f) = filter {
            if unsafe { f(de) } == 0 {
                continue;
            }
        }
        if count == cap {
            let newcap = if cap == 0 { 16 } else { cap * 2 };
            let bytes = newcap * core::mem::size_of::<*mut Dirent>();
            let new = unsafe { crate::realloc(entries as *mut c_void, bytes) } as *mut *mut Dirent;
            if new.is_null() {
                unsafe { free_dirent_array(entries, count) };
                let _ = unsafe { closedir(dir) };
                return -1;
            }
            entries = new;
            cap = newcap;
        }
        let copy = unsafe { crate::malloc(core::mem::size_of::<Dirent>()) } as *mut Dirent;
        if copy.is_null() {
            unsafe { free_dirent_array(entries, count) };
            let _ = unsafe { closedir(dir) };
            return -1;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                de as *const u8,
                copy as *mut u8,
                core::mem::size_of::<Dirent>(),
            );
            *entries.add(count) = copy;
        }
        count += 1;
    }
    let _ = unsafe { closedir(dir) };

    if let Some(cmp) = compar {
        let mut i = 1usize;
        while i < count {
            let key = unsafe { *entries.add(i) };
            let mut j = i;
            while j > 0 {
                let prev = unsafe { *entries.add(j - 1) };
                let a = core::ptr::addr_of!(prev).cast::<*const Dirent>();
                let b = core::ptr::addr_of!(key).cast::<*const Dirent>();
                if unsafe { cmp(a, b) } <= 0 {
                    break;
                }
                unsafe { *entries.add(j) = prev };
                j -= 1;
            }
            unsafe { *entries.add(j) = key };
            i += 1;
        }
    }

    unsafe { *namelist = entries };
    count as c_int
}

/// Free a `scandir` result on the error path.
#[cfg(target_os = "minix")]
unsafe fn free_dirent_array(entries: *mut *mut Dirent, count: usize) {
    for i in 0..count {
        unsafe { crate::free(*entries.add(i) as *mut c_void) };
    }
    unsafe { crate::free(entries as *mut c_void) };
}

/// POSIX `alphasort()`: compare two directory entries by name.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn alphasort(a: *const *const Dirent, b: *const *const Dirent) -> c_int {
    unsafe { crate::c_string::strcmp((**a).d_name.as_ptr(), (**b).d_name.as_ptr()) }
}

// ---- sys/socket.h ----

// The socket family (socket/bind/connect/listen/accept/shutdown/send/
// recv/...) is implemented in lib.rs over minix_std::net; `setsockopt` is
// unsupported (the net server stores no options) and `getsockopt` answers only
// the one question the stack can: the socket's type.

/// `SOL_SOCKET` as the port's header spells it (`sys/sys/socket.h`), not
/// Linux's 1.
#[cfg(target_os = "minix")]
const SOL_SOCKET: c_int = 0xffff;
/// `SO_TYPE`
#[cfg(target_os = "minix")]
const SO_TYPE: c_int = 3;

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

/// `getsockopt(2)`.
///
/// The net server keeps no socket options, so this answers exactly one:
/// `SO_TYPE`, which is discoverable from the ioctls each socket family responds
/// to (`NWIOGTCPCONF` for TCP, `NWIOGUDPOPT` for UDP). Everything else is
/// `ENOSYS`, matching `setsockopt` — there is no value to report, and claiming
/// `ENOPROTOOPT` would say the option is unknown rather than unimplemented.
///
/// # Safety
///
/// `optval` must be null or writable for `*optlen` bytes and `optlen` must be a
/// valid pointer.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getsockopt(
    fd: c_int,
    level: c_int,
    optname: c_int,
    optval: *mut c_void,
    optlen: *mut c_uint,
) -> c_int {
    if level == SOL_SOCKET && optname == SO_TYPE {
        let kind = match minix_std::net::socket_type(fd) {
            Ok(k) => k,
            Err(e) => return crate::fail(e.0),
        };
        if optval.is_null() || optlen.is_null() {
            return crate::fail(EFAULT);
        }
        if unsafe { *optlen } < 4 {
            return crate::fail(EINVAL);
        }
        unsafe {
            *(optval as *mut c_int) = kind;
            *optlen = 4;
        }
        return 0;
    }
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

/// POSIX `sync()`: flush filesystem caches. The VFS writes through to disk, so
/// there is nothing buffered to push.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn sync() {}

/// `posix_fadvise(2)`: declare an expected access pattern for an open file.
///
/// The VFS keeps no page cache of its own, so there is no advice to store and
/// no readahead to prime; every advice value is accepted as a no-op.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn posix_fadvise(
    _fd: c_int,
    _offset: c_long,
    _len: c_long,
    _advice: c_int,
) -> c_int {
    0
}

/// BSD `flock()`: advisory whole-file locking. The VFS has no file locks, so the
/// lock is granted unconditionally; the caller is Mesa's on-disk cache DB, which
/// the Mesa build compiles out with `-Dshader-cache=disabled`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flock(_fd: c_int, _operation: c_int) -> c_int {
    0
}

const SC_ARG_MAX: c_int = 0;
const SC_PAGE_SIZE: c_int = 30;
const SC_OPEN_MAX: c_int = 4;
const SC_CLK_TCK: c_int = 2;
const SC_GETPW_R_SIZE_MAX: c_int = 69;
const SC_PHYS_PAGES: c_int = 85;
const SC_AVPHYS_PAGES: c_int = 86;
const OPEN_MAX: c_int = 32;

/// The physical-memory answers: PM reports no RAM figure, so `_SC_PHYS_PAGES`
/// and `_SC_AVPHYS_PAGES` name the QEMU default of 256 MiB in 4 KiB pages rather
/// than fail. Callers (Mesa's `os_get_total_physical_memory`) treat a
/// non-positive answer as "unknown", which would be worse than a figure that is
/// merely conservative.
const PHYS_PAGES: c_long = 65_536;

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
        SC_PHYS_PAGES | SC_AVPHYS_PAGES => PHYS_PAGES,
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
    match minix_std::fs::statvfs_bytes(path_bytes) {
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

/// POSIX `getcwd(3)`: the absolute pathname of the current directory.
///
/// The walk itself lives in `minix-std`, which is where the cwd is held. A NULL
/// `buf` asks for the result to be `malloc`ed, as glibc allows and as bash uses
/// (`getcwd(0, PATH_MAX)`); the caller frees it.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getcwd(buf: *mut c_char, size: usize) -> *mut c_char {
    if !buf.is_null() {
        if size <= 1 {
            crate::set_errno(EINVAL);
            return core::ptr::null_mut();
        }
        let out = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, size) };
        return match minix_std::fs::getcwd(out) {
            Ok(_) => buf,
            Err(e) => {
                crate::set_errno(e.0);
                core::ptr::null_mut()
            }
        };
    }

    // The allocate form. Grow on ERANGE the way glibc does, up to the path
    // ceiling: past that the walk itself could not have produced the longer
    // result it is asking for.
    let mut cap = if size == 0 {
        256
    } else {
        size.min(minix_std::fs::PATH_MAX)
    };
    loop {
        let alloc = unsafe { crate::malloc(cap) } as *mut c_char;
        if alloc.is_null() {
            crate::set_errno(ENOMEM);
            return core::ptr::null_mut();
        }
        let out = unsafe { core::slice::from_raw_parts_mut(alloc as *mut u8, cap) };
        match minix_std::fs::getcwd(out) {
            Ok(_) => return alloc,
            Err(e) if e.0 == ERANGE && cap < minix_std::fs::PATH_MAX => {
                unsafe { crate::free(alloc as *mut c_void) };
                cap = (cap * 2).min(minix_std::fs::PATH_MAX);
            }
            Err(e) => {
                unsafe { crate::free(alloc as *mut c_void) };
                crate::set_errno(e.0);
                return core::ptr::null_mut();
            }
        }
    }
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

/// Create a symbolic link: 0, or -1 with `errno` set. `target` is stored as given and is
/// never resolved, so it may name something that does not exist yet.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn symlink(target: *const c_char, linkpath: *const c_char) -> c_int {
    if target.is_null() || linkpath.is_null() {
        return crate::fail(EINVAL);
    }
    let target_bytes = unsafe { core::ffi::CStr::from_ptr(target) }.to_bytes();
    let linkpath_bytes = unsafe { core::ffi::CStr::from_ptr(linkpath) }.to_bytes();
    match minix_std::fs::symlink(target_bytes, linkpath_bytes) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `AT_FDCWD` from `tools/c-include/fcntl.h`: resolve relative to the cwd.
#[cfg(target_os = "minix")]
const AT_FDCWD: c_int = -100;

/// `AT_SYMLINK_FOLLOW` from `tools/c-include/fcntl.h`.
#[cfg(target_os = "minix")]
const AT_SYMLINK_FOLLOW: c_int = 0x400;

/// `AT_SYMLINK_NOFOLLOW` from `tools/c-include/fcntl.h`.
#[cfg(target_os = "minix")]
const AT_SYMLINK_NOFOLLOW: c_int = 0x200;

/// `AT_REMOVEDIR` from `tools/c-include/fcntl.h`.
#[cfg(target_os = "minix")]
const AT_REMOVEDIR: c_int = 0x800;

/// `AT_EACCESS` from `tools/c-include/fcntl.h`.
#[cfg(target_os = "minix")]
const AT_EACCESS: c_int = 0x100;

#[cfg(target_os = "minix")]
fn is_absolute(path: &[u8]) -> bool {
    path.first() == Some(&b'/')
}

/// Run `f` with the process cwd moved to `dirfd`, then put it back.
///
/// The VFS has only path-based calls, so a directory descriptor handed to an
/// `*at` function becomes an `fchdir` for the duration of one call. The cwd is
/// process-wide, so a caller that reaches the move is the only thread whose
/// relative paths are in flight.
#[cfg(target_os = "minix")]
unsafe fn with_dirfd<R>(
    dirfd: c_int,
    path: &[u8],
    f: impl FnOnce() -> Result<R, minix_std::MinixErr>,
) -> Result<R, minix_std::MinixErr> {
    use minix_std::MinixErr;

    if dirfd == AT_FDCWD || is_absolute(path) {
        return f();
    }
    if dirfd < 0 {
        return Err(MinixErr(EBADF));
    }
    // `getcwd(NULL, 0)` allocates, so the saved path is freed below.
    let saved = unsafe { getcwd(core::ptr::null_mut(), 0) };
    if saved.is_null() {
        return Err(MinixErr(unsafe { *crate::__errno_location() }));
    }
    if let Err(e) = minix_std::fs::fchdir(dirfd) {
        unsafe { crate::free(saved.cast()) };
        return Err(e);
    }
    let out = f();
    let restored = minix_std::fs::chdir(unsafe { core::ffi::CStr::from_ptr(saved) }.to_bytes());
    unsafe { crate::free(saved.cast()) };
    if restored.is_err() {
        // The call already ran. A cwd that cannot be put back would misdirect
        // every later relative path, so report it rather than continue.
        return Err(MinixErr(EIO));
    }
    out
}

/// Resolve the two names of a `linkat`/`renameat`-shaped call.
///
/// The VFS takes both names in one call, so a pair that needs two different
/// directories cannot be served by the single `fchdir` the port has; those
/// pairs fail rather than half-apply. A name that was relative to the cwd also
/// changes meaning under the move, so a cwd-relative name mixed with a
/// dirfd-relative one is refused for the same reason.
#[cfg(target_os = "minix")]
unsafe fn two_paths<R>(
    olddirfd: c_int,
    old: &[u8],
    newdirfd: c_int,
    new: &[u8],
    f: impl FnOnce() -> Result<R, minix_std::MinixErr>,
) -> Result<R, minix_std::MinixErr> {
    use minix_std::MinixErr;

    let old_via_fd = !is_absolute(old) && olddirfd != AT_FDCWD;
    let new_via_fd = !is_absolute(new) && newdirfd != AT_FDCWD;
    let old_via_cwd = !is_absolute(old) && olddirfd == AT_FDCWD;
    let new_via_cwd = !is_absolute(new) && newdirfd == AT_FDCWD;

    if old_via_fd && new_via_fd {
        if olddirfd != newdirfd {
            return Err(MinixErr(ENOSYS));
        }
        return unsafe { with_dirfd(olddirfd, old, f) };
    }
    if (old_via_fd && new_via_cwd) || (new_via_fd && old_via_cwd) {
        return Err(MinixErr(ENOSYS));
    }
    if old_via_fd {
        return unsafe { with_dirfd(olddirfd, old, f) };
    }
    if new_via_fd {
        return unsafe { with_dirfd(newdirfd, new, f) };
    }
    f()
}

/// `linkat(2)`: hard link `oldpath` to `newpath`, resolving either name
/// against its `dirfd` when the name is relative.
///
/// The VFS link call never dereferences the old name, so `AT_SYMLINK_FOLLOW`
/// has nothing to map onto and fails rather than linking the wrong inode.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn linkat(
    olddirfd: c_int,
    oldpath: *const c_char,
    newdirfd: c_int,
    newpath: *const c_char,
    flags: c_int,
) -> c_int {
    if oldpath.is_null() || newpath.is_null() {
        return crate::fail(EINVAL);
    }
    if flags & !AT_SYMLINK_FOLLOW != 0 {
        return crate::fail(EINVAL);
    }
    if flags & AT_SYMLINK_FOLLOW != 0 {
        return crate::fail(ENOSYS);
    }
    let old = unsafe { core::ffi::CStr::from_ptr(oldpath) }.to_bytes();
    let new = unsafe { core::ffi::CStr::from_ptr(newpath) }.to_bytes();
    let result = unsafe {
        two_paths(olddirfd, old, newdirfd, new, || {
            minix_std::fs::link(old, new)
        })
    };
    match result {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `symlinkat(2)`: `linkpath` is resolved against `newdirfd` when it is
/// relative; `target` is stored as given and never resolved.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn symlinkat(
    target: *const c_char,
    newdirfd: c_int,
    linkpath: *const c_char,
) -> c_int {
    if target.is_null() || linkpath.is_null() {
        return crate::fail(EINVAL);
    }
    let target_bytes = unsafe { core::ffi::CStr::from_ptr(target) }.to_bytes();
    let linkpath_bytes = unsafe { core::ffi::CStr::from_ptr(linkpath) }.to_bytes();
    let result = unsafe {
        with_dirfd(newdirfd, linkpath_bytes, || {
            minix_std::fs::symlink(target_bytes, linkpath_bytes)
        })
    };
    match result {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
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

/// `openat(2)`: `path` is resolved against `dirfd` when it is relative.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openat(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mode: c_uint,
) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let result = unsafe {
        with_dirfd(dirfd, path_bytes, || {
            minix_std::fs::open(path_bytes, flags, mode)
        })
    };
    match result {
        Ok(fd) => fd,
        Err(e) => crate::fail(e.0),
    }
}

/// `renameat(2)`: `oldpath` and `newpath` are resolved against their own
/// `dirfd` when relative; see [`two_paths`] for the pairs that cannot be.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn renameat(
    olddirfd: c_int,
    oldpath: *const c_char,
    newdirfd: c_int,
    newpath: *const c_char,
) -> c_int {
    if oldpath.is_null() || newpath.is_null() {
        return crate::fail(EINVAL);
    }
    let old = unsafe { core::ffi::CStr::from_ptr(oldpath) }.to_bytes();
    let new = unsafe { core::ffi::CStr::from_ptr(newpath) }.to_bytes();
    let result = unsafe {
        two_paths(olddirfd, old, newdirfd, new, || {
            minix_std::fs::rename(old, new)
        })
    };
    match result {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `unlinkat(2)`: remove a name, or a directory when `AT_REMOVEDIR` is set.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unlinkat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    if flags & !AT_REMOVEDIR != 0 {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let result = unsafe {
        with_dirfd(dirfd, path_bytes, || {
            if flags & AT_REMOVEDIR != 0 {
                minix_std::fs::rmdir(path_bytes)
            } else {
                minix_std::fs::unlink(path_bytes)
            }
        })
    };
    match result {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `mkdirat(2)`: create a directory relative to `dirfd`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mkdirat(dirfd: c_int, path: *const c_char, mode: c_uint) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let result =
        unsafe { with_dirfd(dirfd, path_bytes, || minix_std::fs::mkdir(path_bytes, mode)) };
    match result {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `faccessat(2)`: check the accessibility of a name relative to `dirfd`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn faccessat(
    dirfd: c_int,
    path: *const c_char,
    mode: c_int,
    flags: c_int,
) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    // The VFS access check always follows the final component, so no flag the
    // caller can pass has anything to change.
    if flags & !AT_EACCESS != 0 {
        return crate::fail(ENOSYS);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let result = unsafe {
        with_dirfd(dirfd, path_bytes, || {
            minix_std::fs::access(path_bytes, mode as u32)
        })
    };
    match result {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `fchmodat(2)`: change the mode of a name relative to `dirfd`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fchmodat(
    dirfd: c_int,
    path: *const c_char,
    mode: c_uint,
    flags: c_int,
) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    if flags != 0 {
        return crate::fail(ENOSYS);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let result =
        unsafe { with_dirfd(dirfd, path_bytes, || minix_std::fs::chmod(path_bytes, mode)) };
    match result {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `fchownat(2)`: change the owner of a name relative to `dirfd`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fchownat(
    dirfd: c_int,
    path: *const c_char,
    owner: c_uint,
    group: c_uint,
    flags: c_int,
) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    if flags != 0 {
        return crate::fail(ENOSYS);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let result = unsafe {
        with_dirfd(dirfd, path_bytes, || {
            minix_std::fs::chown(path_bytes, owner, group)
        })
    };
    match result {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// `readlinkat(2)`: read a symlink's contents, relative to `dirfd`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn readlinkat(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut c_char,
    bufsiz: usize,
) -> isize {
    if path.is_null() || (buf.is_null() && bufsiz != 0) {
        return crate::fail(EINVAL) as isize;
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    // A null buffer with a zero size is the one shape POSIX leaves unspecified;
    // there is nothing to write and nothing to build a slice from.
    let out = if bufsiz == 0 {
        &mut [][..]
    } else {
        unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, bufsiz) }
    };
    let result = unsafe {
        with_dirfd(dirfd, path_bytes, || {
            minix_std::fs::readlink(path_bytes, out)
        })
    };
    match result {
        Ok(n) => n as isize,
        Err(e) => crate::fail(e.0) as isize,
    }
}

/// `fstatat(2)`: metadata for a name relative to `dirfd`.
///
/// `AT_SYMLINK_NOFOLLOW` selects the lstat form, which is the only flag the
/// VFS can honour; anything else is refused rather than silently ignored.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstatat(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut c_void,
    flags: c_int,
) -> c_int {
    if path.is_null() || buf.is_null() {
        return crate::fail(EINVAL);
    }
    if flags & !AT_SYMLINK_NOFOLLOW != 0 {
        return crate::fail(ENOSYS);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let stat_fn = if flags & AT_SYMLINK_NOFOLLOW != 0 {
        minix_std::fs::lstat_bytes
    } else {
        minix_std::fs::stat_bytes
    };
    let result = unsafe {
        with_dirfd(dirfd, path_bytes, || {
            Ok(crate::stat_path_impl(stat_fn, path, buf))
        })
    };
    match result {
        Ok(rc) => rc,
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

/// `posix_madvise(3)`: the same advice vocabulary as `madvise`, which the port
/// accepts and ignores — there is no readahead to schedule and no page cache to
/// drop, so every advice is already the case.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn posix_madvise(
    _addr: *mut c_void,
    _length: usize,
    _advice: c_int,
) -> c_int {
    0
}

/// `mlock(2)`: the port has no pageable backing store, so every mapped page is
/// resident for as long as the mapping exists and there is nothing to pin.
/// Success is therefore the honest answer; the deviation is that POSIX's
/// `ENOMEM` for a range that is not mapped is not reported.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlock(_addr: *const c_void, _length: usize) -> c_int {
    0
}

/// `munlock(2)`: see `mlock` — nothing was pinned, so there is nothing to
/// release.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn munlock(_addr: *const c_void, _length: usize) -> c_int {
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

/// Read a symbolic link's target: the number of bytes written into `buf`, not
/// NUL-terminated and truncated to `bufsiz`, or -1 with `errno` set.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn readlink(path: *const c_char, buf: *mut c_char, bufsiz: usize) -> isize {
    if path.is_null() || (buf.is_null() && bufsiz != 0) {
        return crate::fail(EINVAL) as isize;
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    // A null buffer with a zero size is the one shape POSIX leaves unspecified, and slicing
    // from it would be undefined: there is nothing to write and nothing to build a slice from.
    let out = if bufsiz == 0 {
        &mut [][..]
    } else {
        unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, bufsiz) }
    };
    match minix_std::fs::readlink(path_bytes, out) {
        Ok(n) => n as isize,
        Err(e) => crate::fail(e.0) as isize,
    }
}

// ---- libgen.h: basename/dirname ----

/// POSIX `basename()`: the last component of a path, found in place, so `path`
/// itself may be modified (and shorter). Empty and all-slash inputs give `"."`
/// and `"/"` respectively.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basename(path: *mut c_char) -> *mut c_char {
    static DOT: [u8; 2] = [b'.', 0];
    static ROOT: [u8; 2] = [b'/', 0];
    if path.is_null() {
        return DOT.as_ptr() as *mut c_char;
    }
    let s = path as *mut u8;
    if unsafe { *s } == 0 {
        return DOT.as_ptr() as *mut c_char;
    }
    let mut n = 0usize;
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    let all_slash = (0..n).all(|k| unsafe { *s.add(k) } == b'/');
    if all_slash {
        return ROOT.as_ptr() as *mut c_char;
    }
    let mut i = n - 1;
    while i > 0 && unsafe { *s.add(i) } == b'/' {
        unsafe { *s.add(i) = 0 };
        i -= 1;
    }
    while i > 0 && unsafe { *s.add(i - 1) } != b'/' {
        i -= 1;
    }
    unsafe { s.add(i) as *mut c_char }
}

/// POSIX `dirname()`: everything before the last component, found in place, so
/// `path` itself may be modified. A path with no `/` gives `"."`; a root path
/// gives `"/"`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dirname(path: *mut c_char) -> *mut c_char {
    static DOT: [u8; 2] = [b'.', 0];
    static ROOT: [u8; 2] = [b'/', 0];
    if path.is_null() {
        return DOT.as_ptr() as *mut c_char;
    }
    let s = path as *mut u8;
    if unsafe { *s } == 0 {
        return DOT.as_ptr() as *mut c_char;
    }
    let leading = unsafe { *s } == b'/';
    let mut n = 0usize;
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    let all_slash = (0..n).all(|k| unsafe { *s.add(k) } == b'/');
    if all_slash {
        return ROOT.as_ptr() as *mut c_char;
    }
    let mut i = 0usize;
    while unsafe { *s.add(i) } == b'/' {
        i += 1;
    }
    while unsafe { *s.add(i) } != 0 {
        i += 1;
    }
    while i > 0 && unsafe { *s.add(i - 1) } == b'/' {
        unsafe { *s.add(i - 1) = 0 };
        i -= 1;
    }
    while i > 0 && unsafe { *s.add(i - 1) } != b'/' {
        i -= 1;
    }
    while i > 0 && unsafe { *s.add(i - 1) } == b'/' {
        unsafe { *s.add(i - 1) = 0 };
        i -= 1;
    }
    if i == 0 {
        return if leading {
            ROOT.as_ptr() as *mut c_char
        } else {
            DOT.as_ptr() as *mut c_char
        };
    }
    s as *mut c_char
}

/// C `chown(2)`: change a path's owner and group (VFS_CHOWN).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chown(path: *const c_char, owner: c_uint, group: c_uint) -> c_int {
    if path.is_null() {
        return crate::fail(EINVAL);
    }
    let path_bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    match minix_std::fs::chown(path_bytes, owner, group) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
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

/// `PRIO_PROCESS` — the only selector PM implements.
const PRIO_PROCESS: c_int = 0;

/// Process group id of `pid` (0 = self) (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpgid(pid: c_int) -> c_int {
    match minix_std::process::getpgid(pid) {
        Ok(pgid) => pgid,
        Err(e) => crate::fail(e.0),
    }
}

/// Caller's process group id (PM).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpgrp() -> c_int {
    match minix_std::process::getpgrp() {
        Ok(pgid) => pgid,
        Err(e) => crate::fail(e.0),
    }
}

/// POSIX `getpriority`. A successful `-1` cannot be told from a failure by the
/// return value alone; callers clear `errno` first, as POSIX requires. This
/// does not touch `errno` on success.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpriority(which: c_int, who: c_int) -> c_int {
    match minix_std::process::getpriority(which, who) {
        Ok(v) => v,
        Err(e) => crate::fail(e.0),
    }
}

/// POSIX `setpriority`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setpriority(which: c_int, who: c_int, prio: c_int) -> c_int {
    match minix_std::process::setpriority(which, who, prio) {
        Ok(()) => 0,
        Err(e) => crate::fail(e.0),
    }
}

/// POSIX `nice`: raise the caller's nice value by `inc`, clamped to the
/// `PRIO_MIN..=PRIO_MAX` range, and return the new value.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nice(inc: c_int) -> c_int {
    let current = match minix_std::process::getpriority(PRIO_PROCESS, 0) {
        Ok(v) => v,
        Err(e) => return crate::fail(e.0),
    };
    let new = current
        .saturating_add(inc)
        .clamp(minix_std::process::PRIO_MIN, minix_std::process::PRIO_MAX);
    match minix_std::process::setpriority(PRIO_PROCESS, 0, new) {
        Ok(()) => new,
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
}
