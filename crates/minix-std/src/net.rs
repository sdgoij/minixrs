//! Networking — LWIP/DL protocol wrappers.
//!
//! Provides socket operations (`socket`, `bind`, `listen`, `accept`, `connect`,
//! `send`, `recv`, `getsockopt`, `setsockopt`) via the LWIP network stack.
//!
//! The networking stack is not yet ported (Phase 16). All functions are stubs
//! returning ENOSYS. Real implementations will communicate with the LWIP driver
//! via the NWQ (Network Work Queue) protocol.

#![allow(dead_code)]

use crate::MinixErr;

// ── Address families ────────────────────────────────────────────────────

pub const AF_UNSPEC: i32 = 0;
pub const AF_LOCAL: i32 = 1;
pub const AF_UNIX: i32 = AF_LOCAL;
pub const AF_INET: i32 = 2;
pub const AF_INET6: i32 = 10;

// ── Socket types ────────────────────────────────────────────────────────

pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;
pub const SOCK_RAW: i32 = 3;
pub const SOCK_RDM: i32 = 4;
pub const SOCK_SEQPACKET: i32 = 5;

// ── Socket protocol numbers ─────────────────────────────────────────────

pub const IPPROTO_IP: i32 = 0;
pub const IPPROTO_ICMP: i32 = 1;
pub const IPPROTO_TCP: i32 = 6;
pub const IPPROTO_UDP: i32 = 17;
pub const IPPROTO_RAW: i32 = 255;

// ── Socket option levels ────────────────────────────────────────────────

pub const SOL_SOCKET: i32 = 1;
pub const SOL_TCP: i32 = 6;
pub const SOL_UDP: i32 = 17;

// ── Socket option names ─────────────────────────────────────────────────

pub const SO_REUSEADDR: i32 = 0x0004;
pub const SO_KEEPALIVE: i32 = 0x0008;
pub const SO_BROADCAST: i32 = 0x0020;
pub const SO_LINGER: i32 = 0x0080;
pub const SO_OOBINLINE: i32 = 0x0100;
pub const SO_SNDBUF: i32 = 0x1001;
pub const SO_RCVBUF: i32 = 0x1002;
pub const SO_ERROR: i32 = 0x1007;

// ── Socket operations ───────────────────────────────────────────────────

/// Create a socket endpoint.
pub fn socket(domain: i32, type_: i32, protocol: i32) -> Result<i32, MinixErr> {
    let _ = (domain, type_, protocol);
    Err(MinixErr::ENOSYS)
}

/// Bind a name to a socket.
pub fn bind(sockfd: i32, _addr: &[u8]) -> Result<(), MinixErr> {
    let _ = sockfd;
    Err(MinixErr::ENOSYS)
}

/// Listen for connections on a socket.
pub fn listen(sockfd: i32, backlog: i32) -> Result<(), MinixErr> {
    let _ = (sockfd, backlog);
    Err(MinixErr::ENOSYS)
}

/// Accept a connection on a socket.
pub fn accept(sockfd: i32, _addr: &mut [u8]) -> Result<i32, MinixErr> {
    let _ = sockfd;
    Err(MinixErr::ENOSYS)
}

/// Initiate a connection on a socket.
pub fn connect(sockfd: i32, _addr: &[u8]) -> Result<(), MinixErr> {
    let _ = sockfd;
    Err(MinixErr::ENOSYS)
}

/// Send a message on a socket.
pub fn send(sockfd: i32, buf: &[u8], flags: i32) -> Result<usize, MinixErr> {
    let _ = (sockfd, buf, flags);
    Err(MinixErr::ENOSYS)
}

/// Receive a message from a socket.
pub fn recv(sockfd: i32, buf: &mut [u8], flags: i32) -> Result<usize, MinixErr> {
    let _ = (sockfd, buf, flags);
    Err(MinixErr::ENOSYS)
}

/// Get socket options.
pub fn getsockopt(sockfd: i32, level: i32, optname: i32) -> Result<i32, MinixErr> {
    let _ = (sockfd, level, optname);
    Err(MinixErr::ENOSYS)
}

/// Set socket options.
pub fn setsockopt(sockfd: i32, level: i32, optname: i32, optval: i32) -> Result<(), MinixErr> {
    let _ = (sockfd, level, optname, optval);
    Err(MinixErr::ENOSYS)
}

// ── sockaddr_in structure ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SockAddrIn {
    pub sin_family: u16,   // AF_INET
    pub sin_port: u16,     // port in network byte order
    pub sin_addr: [u8; 4], // IPv4 address
    pub sin_zero: [u8; 8], // padding
}

impl Default for SockAddrIn {
    fn default() -> Self {
        Self::new()
    }
}

impl SockAddrIn {
    pub const fn new() -> Self {
        Self {
            sin_family: AF_INET as u16,
            sin_port: 0,
            sin_addr: [0; 4],
            sin_zero: [0; 8],
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_families() {
        assert_eq!(AF_UNSPEC, 0);
        assert_eq!(AF_UNIX, 1);
        assert_eq!(AF_INET, 2);
        assert_eq!(AF_INET6, 10);
    }

    #[test]
    fn test_socket_types() {
        assert_eq!(SOCK_STREAM, 1);
        assert_eq!(SOCK_DGRAM, 2);
        assert_eq!(SOCK_RAW, 3);
    }

    #[test]
    fn test_protocols() {
        assert_eq!(IPPROTO_TCP, 6);
        assert_eq!(IPPROTO_UDP, 17);
        assert_eq!(IPPROTO_ICMP, 1);
    }

    #[test]
    fn test_socket_options() {
        assert_eq!(SO_REUSEADDR, 0x0004);
        assert_eq!(SO_KEEPALIVE, 0x0008);
        assert_eq!(SO_SNDBUF, 0x1001);
        assert_eq!(SO_RCVBUF, 0x1002);
    }

    #[test]
    fn test_sockaddr_in_layout() {
        assert_eq!(core::mem::size_of::<SockAddrIn>(), 16);
        let sa = SockAddrIn::new();
        assert_eq!(sa.sin_family, 2); // AF_INET
    }

    #[test]
    fn test_socket_returns_enosys() {
        assert!(socket(AF_INET, SOCK_STREAM, IPPROTO_TCP).is_err());
    }

    #[test]
    fn test_bind_returns_enosys() {
        assert!(bind(0, &[]).is_err());
    }

    #[test]
    fn test_listen_returns_enosys() {
        assert!(listen(0, 5).is_err());
    }

    #[test]
    fn test_accept_returns_enosys() {
        assert!(accept(0, &mut []).is_err());
    }

    #[test]
    fn test_connect_returns_enosys() {
        assert!(connect(0, &[]).is_err());
    }

    #[test]
    fn test_send_returns_enosys() {
        assert!(send(0, b"hello", 0).is_err());
    }

    #[test]
    fn test_recv_returns_enosys() {
        assert!(recv(0, &mut [], 0).is_err());
    }

    #[test]
    fn test_getsockopt_returns_enosys() {
        assert!(getsockopt(0, SOL_SOCKET, SO_ERROR).is_err());
    }

    #[test]
    fn test_setsockopt_returns_enosys() {
        assert!(setsockopt(0, SOL_SOCKET, SO_REUSEADDR, 1).is_err());
    }

    #[test]
    fn test_sockaddr_in_fields() {
        let mut sa = SockAddrIn::new();
        sa.sin_port = 0x5000; // port 80 in network byte order
        sa.sin_addr = [127, 0, 0, 1];
        assert_eq!(sa.sin_family, AF_INET as u16);
        assert_eq!(sa.sin_port, 0x5000);
        assert_eq!(sa.sin_addr, [127, 0, 0, 1]);
    }
}
