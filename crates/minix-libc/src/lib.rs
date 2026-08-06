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

// Sockets

/// C `struct sockaddr_in` (reference `net/gen/socket.h`): length, family,
/// network-order port and address, then padding. 16 bytes. The port/address
/// fields are byte arrays so the network-order layout is endianness-explicit
/// (layout-identical to the C `u16`/`u32` fields).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SockAddrIn {
    sin_len: u8,
    sin_family: u8,
    sin_port: [u8; 2], // network byte order
    sin_addr: [u8; 4], // network byte order
    sin_zero: [u8; 8],
}

/// Decode a C `struct sockaddr_in` into the address bytes + host-order port
/// the `minix-std` net layer expects. Errors are negated errnos
/// (EAFNOSUPPORT for a non-AF_INET family or a too-short buffer).
///
/// # Safety
///
/// `addr` must point to at least `addrlen` bytes of a `sockaddr_in`.
unsafe fn decode_sockaddr_in(addr: *const u8, addrlen: u32) -> Result<([u8; 4], u16), i32> {
    if addr.is_null() || addrlen < 16 {
        return Err(-97); // EAFNOSUPPORT
    }
    let s = unsafe { &*(addr as *const SockAddrIn) };
    if s.sin_family != 2 {
        // AF_INET
        return Err(-97); // EAFNOSUPPORT
    }
    Ok((s.sin_addr, u16::from_be_bytes(s.sin_port)))
}

/// Fill a C `struct sockaddr_in` (reference getpeername semantics: family
/// AF_INET, network-order port/address, sin_len = 16; copy at most
/// `*addrlen` bytes and update it).
///
/// # Safety
///
/// `addr`/`addrlen` must be valid for the sizes the C caller declared.
unsafe fn encode_sockaddr_in(addr: *mut u8, addrlen: *mut u32, ip: [u8; 4], port: u16) {
    if addr.is_null() || addrlen.is_null() {
        return;
    }
    let len = unsafe { *addrlen }.min(16);
    let s = SockAddrIn {
        sin_len: 16,
        sin_family: 2, // AF_INET
        sin_port: port.to_be_bytes(),
        sin_addr: ip,
        sin_zero: [0; 8],
    };
    unsafe {
        core::ptr::copy_nonoverlapping(&s as *const SockAddrIn as *const u8, addr, len as usize);
        *addrlen = len;
    }
}

/// `socket(2)`: create an endpoint — AF_INET/SOCK_STREAM → `/dev/tcp`,
/// AF_INET/SOCK_DGRAM → `/dev/udp`, AF_INET/SOCK_RAW+ICMP → `/dev/ip`.
/// Returns the file descriptor.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn socket(domain: c_int, type_: c_int, protocol: c_int) -> c_int {
    match minix_std::net::socket(domain, type_, protocol) {
        Ok(fd) => fd,
        Err(e) => -(e.0),
    }
}

/// `bind(2)`: bind a socket to a `struct sockaddr_in` (AF_INET).
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn bind(sock: c_int, address: *const c_void, address_len: u32) -> c_int {
    let (ip, port) = match unsafe { decode_sockaddr_in(address as *const u8, address_len) } {
        Ok(a) => a,
        Err(e) => return e,
    };
    match minix_std::net::bind(sock, ip, port) {
        Ok(()) => 0,
        Err(e) => -(e.0),
    }
}

/// `connect(2)`: run the TCP three-way handshake, or set the UDP default
/// destination and receive filter.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn connect(sock: c_int, address: *const c_void, address_len: u32) -> c_int {
    let (ip, port) = match unsafe { decode_sockaddr_in(address as *const u8, address_len) } {
        Ok(a) => a,
        Err(e) => return e,
    };
    match minix_std::net::connect(sock, ip, port) {
        Ok(()) => 0,
        Err(e) => -(e.0),
    }
}

/// `sendto(2)`: send one datagram, optionally to an explicit destination
/// (a `struct sockaddr_in`). Only `flags == 0` is supported; on a
/// connected socket `dest_addr` is ignored.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sendto(
    sock: c_int,
    buf: *const c_void,
    len: usize,
    flags: c_int,
    dest_addr: *const c_void,
    dest_len: u32,
) -> isize {
    if flags != 0 {
        return -95; // EOPNOTSUPP
    }
    if buf.is_null() {
        return -22; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, len) };
    let dest = if dest_addr.is_null() {
        None
    } else {
        match unsafe { decode_sockaddr_in(dest_addr as *const u8, dest_len) } {
            Ok((ip, port)) => Some(minix_std::net::SocketAddr { ip, port }),
            Err(e) => return e,
        }
    };
    match unsafe { minix_std::net::sendto(sock, slice, dest) } {
        Ok(n) => n as isize,
        Err(e) => -(e.0 as isize),
    }
}

/// `recvfrom(2)`: receive one datagram and, when `src_addr`/`src_len` are
/// given, the sender's `struct sockaddr_in`. Only `flags == 0` is
/// supported.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn recvfrom(
    sock: c_int,
    buf: *mut c_void,
    len: usize,
    flags: c_int,
    src_addr: *mut c_void,
    src_len: *mut u32,
) -> isize {
    if flags != 0 {
        return -95; // EOPNOTSUPP
    }
    if buf.is_null() {
        return -22; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len) };
    match unsafe { minix_std::net::recvfrom(sock, slice) } {
        Ok((n, addr)) => {
            if !src_addr.is_null() && !src_len.is_null() && n > 0 {
                unsafe { encode_sockaddr_in(src_addr as *mut u8, src_len, addr.ip, addr.port) };
            }
            n as isize
        }
        Err(e) => -(e.0 as isize),
    }
}

/// `shutdown(2)`: SHUT_WR/SHUT_RDWR send our FIN and keep reading;
/// SHUT_RD is ENOSYS (the net server cannot close the read half).
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn shutdown(sock: c_int, how: c_int) -> c_int {
    match minix_std::net::shutdown(sock, how) {
        Ok(()) => 0,
        Err(e) => -(e.0),
    }
}

/// `send(2)`: write the byte stream (TCP) or one datagram (UDP). Only
/// `flags == 0` is supported.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn send(sock: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize {
    if flags != 0 {
        return -95; // EOPNOTSUPP
    }
    if buf.is_null() {
        return -22; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, len) };
    match unsafe { minix_std::net::send(sock, slice) } {
        Ok(n) => n as isize,
        Err(e) => -(e.0 as isize),
    }
}

/// `recv(2)`: read the byte stream (TCP) or one datagram (UDP). Only
/// `flags == 0` is supported; 0 means EOF for a stream.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn recv(sock: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize {
    if flags != 0 {
        return -95; // EOPNOTSUPP
    }
    if buf.is_null() {
        return -22; // EINVAL
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len) };
    match unsafe { minix_std::net::recv(sock, slice) } {
        Ok(n) => n as isize,
        Err(e) => -(e.0 as isize),
    }
}

/// `listen(2)`: mark a bound socket as a listener with the given backlog.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn listen(sock: c_int, backlog: c_int) -> c_int {
    match minix_std::net::listen(sock, backlog) {
        Ok(()) => 0,
        Err(e) => -(e.0),
    }
}

/// `accept(2)`: accept the next pending connection. Fills `address` (a
/// `struct sockaddr_in`) when non-NULL, like the reference accept(2).
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn accept(sock: c_int, address: *mut c_void, address_len: *mut u32) -> c_int {
    let newfd = match minix_std::net::accept(sock) {
        Ok(fd) => fd,
        Err(e) => return -(e.0),
    };
    if !address.is_null() && !address_len.is_null() {
        if let Ok((ip, port)) = minix_std::net::getpeername(newfd) {
            unsafe { encode_sockaddr_in(address as *mut u8, address_len, ip, port) };
        }
    }
    newfd
}

/// `getpeername(2)`: fill `address` with the connected peer.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn getpeername(sock: c_int, address: *mut c_void, address_len: *mut u32) -> c_int {
    if address.is_null() || address_len.is_null() {
        return -22; // EINVAL
    }
    match minix_std::net::getpeername(sock) {
        Ok((ip, port)) => {
            unsafe { encode_sockaddr_in(address as *mut u8, address_len, ip, port) };
            0
        }
        Err(e) => -(e.0),
    }
}

/// `getsockname(2)`: fill `address` with the local bound address.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn getsockname(sock: c_int, address: *mut c_void, address_len: *mut u32) -> c_int {
    if address.is_null() || address_len.is_null() {
        return -22; // EINVAL
    }
    match minix_std::net::getsockname(sock) {
        Ok((ip, port)) => {
            unsafe { encode_sockaddr_in(address as *mut u8, address_len, ip, port) };
            0
        }
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
    use super::*;

    // Host-runnable: the sockaddr_in byte math (network-order port/addr).
    #[test]
    fn sockaddr_in_decode_matches_c_network_order_layout() {
        // sin_len=16, sin_family=AF_INET(2), sin_port=18080 (BE),
        // sin_addr=10.0.2.2 (BE bytes), sin_zero.
        let bytes: [u8; 16] = [16, 2, 0x46, 0xA0, 10, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0];
        let (ip, port) = unsafe { decode_sockaddr_in(bytes.as_ptr(), 16) }.unwrap();
        assert_eq!(ip, [10, 0, 2, 2]);
        assert_eq!(port, 18080);
    }

    #[test]
    fn sockaddr_in_encode_writes_c_network_order() {
        let mut bytes = [0xFFu8; 16];
        let mut len: u32 = 16;
        unsafe {
            encode_sockaddr_in(bytes.as_mut_ptr(), &mut len, [10, 0, 2, 2], 20000);
        }
        // 20000 = 0x4E20.
        assert_eq!(
            bytes,
            [16, 2, 0x4E, 0x20, 10, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(len, 16);
    }

    #[test]
    fn sockaddr_in_decode_rejects_bad_family_and_short_len() {
        let bad_family: [u8; 16] = [16, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            unsafe { decode_sockaddr_in(bad_family.as_ptr(), 16) },
            Err(-97)
        );
        assert_eq!(
            unsafe { decode_sockaddr_in(bad_family.as_ptr(), 8) },
            Err(-97)
        );
    }

    #[test]
    fn sockaddr_in_encode_respects_caller_buffer_size() {
        let mut bytes = [0xFFu8; 16];
        let mut len: u32 = 8;
        unsafe {
            encode_sockaddr_in(bytes.as_mut_ptr(), &mut len, [10, 0, 2, 2], 20000);
        }
        assert_eq!(len, 8); // truncated, like the reference getpeername
        assert_eq!(&bytes[..4], &[16, 2, 0x4E, 0x20]);
    }

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

    #[cfg(target_os = "none")]
    #[test]
    fn test_socket_signatures() {
        fn _socket(f: extern "C" fn(c_int, c_int, c_int) -> c_int) {
            let _ = f;
        }
        fn _addr_in(f: extern "C" fn(c_int, *const c_void, u32) -> c_int) {
            let _ = f;
        }
        fn _addr_out(f: extern "C" fn(c_int, *mut c_void, *mut u32) -> c_int) {
            let _ = f;
        }
        fn _io(f: unsafe extern "C" fn(c_int, *const c_void, usize, c_int) -> isize) {
            let _ = f;
        }
        fn _iomut(f: unsafe extern "C" fn(c_int, *mut c_void, usize, c_int) -> isize) {
            let _ = f;
        }
        fn _sendto(
            f: unsafe extern "C" fn(
                c_int,
                *const c_void,
                usize,
                c_int,
                *const c_void,
                u32,
            ) -> isize,
        ) {
            let _ = f;
        }
        fn _recvfrom(
            f: unsafe extern "C" fn(
                c_int,
                *mut c_void,
                usize,
                c_int,
                *mut c_void,
                *mut u32,
            ) -> isize,
        ) {
            let _ = f;
        }
        fn _shutdown(f: extern "C" fn(c_int, c_int) -> c_int) {
            let _ = f;
        }
        _socket(socket);
        _addr_in(bind);
        _addr_in(connect);
        _sendto(sendto);
        _recvfrom(recvfrom);
        _shutdown(shutdown);
        _io(send);
        _iomut(recv);
        _addr_in(listen);
        _addr_out(accept);
        _addr_out(getpeername);
        _addr_out(getsockname);
    }
}
