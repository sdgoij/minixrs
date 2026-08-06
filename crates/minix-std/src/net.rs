//! Socket API over the MINIX `/dev/udp` (and `/dev/ip`) datagram devices.
//!
//! Phase 16.1: real `socket`/`bind`/`connect`/`send`/`recv` over the net
//! server's clone-minor `/dev/udp` device. A socket is an
//! `open("/dev/udp")`, `bind()`/`connect()` are `NWIOSUDPOPT` ioctls
//! carrying a `nwio_udpopt_t`, and `send`/`recv` are `write`/`read` of
//! whole datagrams (NWUO_RWDATONLY — data only), mirroring the reference
//! libc socket layer (`minix/lib/libc/sys/socket.c`).
//!
//! The address-family / socket-type / protocol constants match the
//! reference `<sys/socket.h>` and `<netinet/in.h>`.

use crate::fs::{O_RDWR, ioctl, open, read, write};
use crate::{EIO, MinixErr};
use net::{
    NWIOSUDPOPT, NWUO_EN_LOC, NWUO_LP_SEL, NWUO_LP_SET, NWUO_RA_SET, NWUO_RP_SET, NWUO_RWDATONLY,
    NwioUdpOpt,
};

// ---- Address families (sys/socket.h) ----

pub const AF_UNSPEC: i32 = 0;
pub const AF_LOCAL: i32 = 1;
pub const AF_UNIX: i32 = AF_LOCAL;
pub const AF_INET: i32 = 2;
pub const AF_INET6: i32 = 10;

// ---- Socket types (sys/socket.h) ----

pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;
pub const SOCK_RAW: i32 = 3;
pub const SOCK_RDM: i32 = 4;
pub const SOCK_SEQPACKET: i32 = 5;

// ---- IP protocols (netinet/in.h) ----

pub const IPPROTO_IP: i32 = 0;
pub const IPPROTO_ICMP: i32 = 1;
pub const IPPROTO_TCP: i32 = 6;
pub const IPPROTO_UDP: i32 = 17;
pub const IPPROTO_RAW: i32 = 255;

// ---- Socket options (sys/socket.h) ----

pub const SOL_SOCKET: i32 = 1;
pub const SOL_TCP: i32 = 6;
pub const SOL_UDP: i32 = 17;

pub const SO_REUSEADDR: i32 = 0x0004;
pub const SO_KEEPALIVE: i32 = 0x0008;
pub const SO_BROADCAST: i32 = 0x0020;
pub const SO_LINGER: i32 = 0x0080;
pub const SO_OOBINLINE: i32 = 0x0100;
pub const SO_SNDBUF: i32 = 0x1001;
pub const SO_RCVBUF: i32 = 0x1002;
pub const SO_ERROR: i32 = 0x1007;

// ---- Socket creation ----

/// Create a socket. Only the protocols the net server implements are
/// accepted:
///
/// - `(AF_INET, SOCK_DGRAM, IPPROTO_UDP|0)` — a `/dev/udp` socket, implicitly
///   bound to an ephemeral local port on the local address
/// - `(AF_INET, SOCK_RAW, IPPROTO_ICMP|0)` — a raw `/dev/ip` descriptor (the
///   ping-style protocol: write an 8-byte `{dst_ip,id,seq}` request, read the
///   matching reply as a raw IP datagram)
///
/// `AF_INET`/`SOCK_STREAM` (TCP) is not implemented yet.
///
/// Returns the file descriptor.
pub fn socket(domain: i32, type_: i32, protocol: i32) -> Result<i32, MinixErr> {
    let sock_type = type_ & !0xF;
    match (domain, sock_type, protocol) {
        (AF_INET, SOCK_DGRAM, IPPROTO_UDP | IPPROTO_IP) => udp_socket(),
        (AF_INET, SOCK_RAW, IPPROTO_ICMP | IPPROTO_IP) => raw_icmp_socket(),
        (AF_INET, SOCK_STREAM, _) => Err(MinixErr::from_i32(crate::EPROTONOSUPPORT)),
        (_, _, _) => Err(MinixErr::from_i32(crate::EAFNOSUPPORT)),
    }
}

/// Open a UDP socket and implicitly bind it to an ephemeral local port on
/// the local address (like `socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)`).
/// Returns the file descriptor.
pub fn udp_socket() -> Result<i32, MinixErr> {
    let fd = unsafe { open("/dev/udp", O_RDWR, 0) }?;
    let opt = NwioUdpOpt {
        nwuo_flags: NWUO_LP_SEL | NWUO_EN_LOC | NWUO_RWDATONLY,
        nwuo_locport: 0,
        nwuo_remport: 0,
        nwuo_locaddr: 0, // INADDR_ANY
        nwuo_remaddr: 0,
    };
    if unsafe { ioctl(fd, NWIOSUDPOPT, &opt as *const NwioUdpOpt as *mut u8) }.is_err() {
        let _ = crate::fs::close(fd);
        return Err(MinixErr::from_i32(EIO));
    }
    Ok(fd)
}

/// Open a raw ICMP socket (`/dev/ip`). Reads return the raw IP datagram of
/// the next matching echo reply; writes take the 8-byte
/// `{dst_ip[4] id[2] seq[2]}` echo request used by `/bin/ping`.
fn raw_icmp_socket() -> Result<i32, MinixErr> {
    let fd = unsafe { open("/dev/ip", O_RDWR, 0) }?;
    Ok(fd)
}

/// Bind a UDP socket to `addr:port` (`addr` = `[0; 4]` for INADDR_ANY).
pub fn bind(fd: i32, addr: [u8; 4], port: u16) -> Result<(), MinixErr> {
    let opt = NwioUdpOpt {
        nwuo_flags: NWUO_LP_SET | NWUO_EN_LOC | NWUO_RWDATONLY,
        nwuo_locport: port,
        nwuo_remport: 0,
        nwuo_locaddr: u32::from_be_bytes(addr),
        nwuo_remaddr: 0,
    };
    unsafe { ioctl(fd, NWIOSUDPOPT, &opt as *const NwioUdpOpt as *mut u8) }?;
    Ok(())
}

/// Connect a UDP socket to `addr:port`: the default destination for
/// [`send`] and the filter for datagrams delivered by [`recv`].
pub fn connect(fd: i32, addr: [u8; 4], port: u16) -> Result<(), MinixErr> {
    let opt = NwioUdpOpt {
        nwuo_flags: NWUO_RP_SET | NWUO_RA_SET | NWUO_RWDATONLY,
        nwuo_locport: 0,
        nwuo_remport: port,
        nwuo_locaddr: 0,
        nwuo_remaddr: u32::from_be_bytes(addr),
    };
    unsafe { ioctl(fd, NWIOSUDPOPT, &opt as *const NwioUdpOpt as *mut u8) }?;
    Ok(())
}

/// Send one datagram to the connected peer. The whole buffer is one
/// datagram; returns the byte count on success.
///
/// # Safety
///
/// `data` must be a valid byte slice.
pub unsafe fn send(fd: i32, data: &[u8]) -> Result<i64, MinixErr> {
    unsafe { write(fd, data) }
}

/// Receive one datagram payload (data only). Returns the byte count.
///
/// # Safety
///
/// `buf` must be a valid mutable byte slice.
pub unsafe fn recv(fd: i32, buf: &mut [u8]) -> Result<i64, MinixErr> {
    unsafe { read(fd, buf) }
}

/// Close a socket (frees the net server's socket slot).
pub fn close(fd: i32) -> Result<(), MinixErr> {
    crate::fs::close(fd)
}
