//! Minimal libc for FFI — C ABI wrappers over `minix-std` primitives.
//!
//! Provides `extern "C"` functions that wrap the Rust-native `minix-std` and
//! `minix-rt` APIs so that any remaining C code can link against them.
//!
//! All functions follow the POSIX convention: return -1 on error and set
//! `errno` (stored in thread-local or a static). For simplicity in this
//! minimal implementation, functions return the negated errno directly
//! (MINIX kernel convention) or 0/positive on success.

#![no_std]
#![allow(dead_code)]

#[cfg(target_os = "none")]
use core::ffi::{c_char, c_int, c_void};

// File I/O

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: c_int) -> c_int {
    if path.is_null() {
        return -22; // EINVAL
    }
    let path_str = unsafe { core::ffi::CStr::from_ptr(path) };
    let path = match path_str.to_str() {
        Ok(s) => s,
        Err(_) => return -22, // EINVAL
    };
    match unsafe { minix_std::fs::open(path, flags, mode as u32) } {
        Ok(fd) => fd,
        Err(e) => -(e.0),
    }
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize {
    if buf.is_null() {
        return -(22); // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, count) };
    match unsafe { minix_std::fs::read(fd, slice) } {
        Ok(n) => n as isize,
        Err(e) => -(e.0 as isize),
    }
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: usize) -> isize {
    if buf.is_null() {
        return -(22); // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
    match unsafe { minix_std::fs::write(fd, slice) } {
        Ok(n) => n as isize,
        Err(e) => -(e.0 as isize),
    }
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn close(fd: c_int) -> c_int {
    match minix_std::fs::close(fd) {
        Ok(()) => 0,
        Err(e) => -(e.0),
    }
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64 {
    match minix_std::fs::lseek(fd, offset, whence) {
        Ok(pos) => pos,
        Err(e) => -(e.0 as i64),
    }
}

// Process lifecycle

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fork() -> c_int {
    match unsafe { minix_std::process::fork() } {
        Ok(pid) => pid,
        Err(e) => -(e.0),
    }
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn exit(status: c_int) -> ! {
    minix_std::process::exit(status);
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn getpid() -> c_int {
    match minix_std::process::getpid() {
        Ok((pid, _ppid)) => pid,
        Err(_) => -1,
    }
}

// Memory management

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mmap(
    addr: *mut c_void,
    length: usize,
    prot: c_int,
    flags: c_int,
    fd: c_int,
    offset: i64,
) -> *mut c_void {
    unsafe {
        minix_std::vmem::mmap(addr as *mut u8, length, prot, flags, fd, offset) as *mut c_void
    }
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn munmap(addr: *mut c_void, length: usize) -> c_int {
    unsafe { minix_std::vmem::munmap(addr as *mut u8, length) }
}

// Time

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn clock_gettime(clock_id: c_int, tp: *mut minix_std::time::TimeSpec) -> c_int {
    if tp.is_null() {
        return -(22); // EINVAL
    }
    match minix_std::time::clock_gettime(clock_id) {
        Ok(ts) => {
            unsafe { *tp = ts };
            0
        }
        Err(e) => -(e.0),
    }
}

// Signals

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn kill(pid: c_int, sig: c_int) -> c_int {
    match minix_std::time::kill(pid, sig) {
        Ok(()) => 0,
        Err(e) => -(e.0),
    }
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn sigprocmask(how: c_int, set: u64) -> c_int {
    match minix_std::time::sigprocmask(how, set) {
        Ok(()) => 0,
        Err(e) => -(e.0),
    }
}

/// POSIX `signal()`: set the disposition of `signum` to `handler`
/// (SIG_DFL=0, SIG_IGN=1, or a handler address).
///
/// Returns the previous disposition on success (not fetched — the
/// registration path only sets), or SIG_ERR (all-ones) on error.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn signal(signum: c_int, handler: u64) -> u64 {
    match minix_std::time::signal(signum, handler) {
        Ok(()) => 0,  // SIG_DFL
        Err(_) => !0, // SIG_ERR
    }
}

/// POSIX `sigaction()`: set a signal action from a C `struct sigaction`
/// (handler u64@0, mask 16 bytes@8, flags i32@24 — 28 bytes).
///
/// `oldact` is accepted but not filled (the old action is not fetched).
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigaction(
    signum: c_int,
    act: *const c_void,
    _oldact: *mut c_void,
) -> c_int {
    if act.is_null() {
        return 0; // query-only; oldact not fetched
    }
    let bytes = unsafe { core::slice::from_raw_parts(act as *const u8, 28) };
    let act_arr: [u8; 28] = bytes.try_into().unwrap();
    let (handler, mask, flags) = minix_std::time::decode_action(&act_arr);
    match minix_std::time::sigaction(signum, handler, mask, flags) {
        Ok(()) => 0,
        Err(e) => -(e.0),
    }
}

// Utility

/// Simple strlen for C strings.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    let mut len = 0;
    while unsafe { *s.add(len) } != 0 {
        len += 1;
    }
    len
}

/// Simple memset.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(s: *mut c_void, c: c_int, n: usize) -> *mut c_void {
    if s.is_null() {
        return s;
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(s as *mut u8, n) };
    for byte in slice.iter_mut() {
        *byte = c as u8;
    }
    s
}

/// Simple memcpy.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    if dest.is_null() || src.is_null() {
        return dest;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(src as *const u8, dest as *mut u8, n);
    }
    dest
}

/// Simple memmove (handles overlap).
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    if dest.is_null() || src.is_null() {
        return dest;
    }
    unsafe {
        core::ptr::copy(src as *const u8, dest as *mut u8, n);
    }
    dest
}

// Tests

#[cfg(test)]
mod tests {
    #[cfg(target_os = "none")]
    use super::*;

    #[cfg(target_os = "none")]
    #[test]
    fn test_strlen() {
        unsafe {
            let s = b"hello\0";
            assert_eq!(strlen(s.as_ptr() as *const c_char), 5);
            assert_eq!(strlen(core::ptr::null()), 0);
            let empty = b"\0";
            assert_eq!(strlen(empty.as_ptr() as *const c_char), 0);
        }
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_memset() {
        let mut buf = [0xFFu8; 10];
        unsafe {
            memset(buf.as_mut_ptr() as *mut c_void, 0, 10);
        }
        assert_eq!(buf, [0; 10]);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_memcpy() {
        let src = [1u8, 2, 3, 4, 5];
        let mut dst = [0u8; 5];
        unsafe {
            memcpy(
                dst.as_mut_ptr() as *mut c_void,
                src.as_ptr() as *const c_void,
                5,
            );
        }
        assert_eq!(dst, [1, 2, 3, 4, 5]);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_memmove() {
        let mut buf = [1u8, 2, 3, 4, 5];
        // Overlapping: move bytes 0..3 to bytes 2..5
        unsafe {
            memmove(
                buf.as_mut_ptr().add(2) as *mut c_void,
                buf.as_ptr() as *const c_void,
                3,
            );
        }
        assert_eq!(buf, [1, 2, 1, 2, 3]);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_open_signature() {
        fn _check(f: unsafe extern "C" fn(*const c_char, c_int, c_int) -> c_int) {
            let _ = f;
        }
        _check(open);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_read_signature() {
        fn _check(f: unsafe extern "C" fn(c_int, *mut c_void, usize) -> isize) {
            let _ = f;
        }
        _check(read);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_write_signature() {
        fn _check(f: unsafe extern "C" fn(c_int, *const c_void, usize) -> isize) {
            let _ = f;
        }
        _check(write);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_close_signature() {
        fn _check(f: extern "C" fn(c_int) -> c_int) {
            let _ = f;
        }
        _check(close);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_fork_signature() {
        fn _check(f: unsafe extern "C" fn() -> c_int) {
            let _ = f;
        }
        _check(fork);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_exit_signature() {
        fn _check(f: extern "C" fn(c_int) -> !) {
            let _ = f;
        }
        _check(exit);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_mmap_signature() {
        fn _check(
            f: unsafe extern "C" fn(*mut c_void, usize, c_int, c_int, c_int, i64) -> *mut c_void,
        ) {
            let _ = f;
        }
        _check(mmap);
    }

    #[cfg(target_os = "none")]
    #[test]
    fn test_kill_signature() {
        fn _check(f: extern "C" fn(c_int, c_int) -> c_int) {
            let _ = f;
        }
        _check(kill);
    }
}
