//! Socket API over the MINIX `/dev/udp`, `/dev/tcp` and `/dev/ip` devices.
//!
//! Phase 16.1: real `socket`/`bind`/`connect`/`send`/`recv` over the net
//! server's clone-minor devices. A socket is an `open("/dev/udp")` or
//! `open("/dev/tcp")`; `bind()`/`connect()` are the reference `NWIO*`
//! ioctls carrying `nwio_udpopt_t`/`nwio_tcpconf_t`; `send`/`recv` are
//! `write`/`read` (whole datagrams for UDP, a byte stream for TCP),
//! mirroring the reference libc socket layer
//! (`minix/lib/libc/sys/socket.c`).
//!
//! The address-family / socket-type / protocol constants match the
//! reference `<sys/socket.h>` and `<netinet/in.h>`.

use crate::fs::{O_RDWR, ioctl, open, read, write};
use crate::{EAGAIN, EIO, EMSGSIZE, ENOTCONN, MinixErr};
use net::{
    NWIOGTCPCONF, NWIOGTCPCOOKIE, NWIOGUDPOPT, NWIOSTCPCONF, NWIOSUDPOPT, NWIOTCPACCEPTTO,
    NWIOTCPCONN, NWIOTCPLISTENQ, NWIOTCPSHUTDOWN, NWTC_LP_SEL, NWTC_LP_SET, NWTC_SET_RA,
    NWTC_SET_RP, NWUO_EN_LOC, NWUO_LP_SEL, NWUO_LP_SET, NWUO_RA_ANY, NWUO_RA_SET, NWUO_RP_ANY,
    NWUO_RP_SET, NWUO_RWDATALL, NWUO_RWDATONLY, NwioTcpCl, NwioTcpConf, NwioUdpOpt, TCF_DEFAULT,
    TcpCookie, UdpIoHdr,
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

// ---- shutdown(2) `how` values (sys/socket.h) ----

/// Disable further reads (not supported by the net server — the reference
/// returns ENOSYS for SHUT_RD).
pub const SHUT_RD: i32 = 0;
/// Disable further writes: send our FIN, keep reading.
pub const SHUT_WR: i32 = 1;
/// Disable both directions (mapped to SHUT_WR here).
pub const SHUT_RDWR: i32 = 2;

// ---- Socket creation ----

/// Create a socket. Only the protocols the net server implements are
/// accepted:
///
/// - `(AF_INET, SOCK_DGRAM, IPPROTO_UDP|0)` — a `/dev/udp` socket, implicitly
///   bound to an ephemeral local port on the local address
/// - `(AF_INET, SOCK_STREAM, IPPROTO_TCP|0)` — a `/dev/tcp` socket; the
///   local port is picked at connect time
/// - `(AF_INET, SOCK_RAW, IPPROTO_ICMP|0)` — a raw `/dev/ip` descriptor (the
///   ping-style protocol: write an 8-byte `{dst_ip,id,seq}` request, read the
///   matching reply as a raw IP datagram)
///
/// Returns the file descriptor.
pub fn socket(domain: i32, type_: i32, protocol: i32) -> Result<i32, MinixErr> {
    let sock_type = type_ & !0xF;
    match (domain, sock_type, protocol) {
        (AF_INET, SOCK_DGRAM, IPPROTO_UDP | IPPROTO_IP) => udp_socket(),
        (AF_INET, SOCK_STREAM, IPPROTO_TCP | IPPROTO_IP) => tcp_socket(),
        (AF_INET, SOCK_RAW, IPPROTO_ICMP | IPPROTO_IP) => raw_icmp_socket(),
        (_, _, _) => Err(MinixErr::from_i32(crate::EPROTONOSUPPORT)),
    }
}

/// Open a TCP socket (`/dev/tcp`). The local port is auto-assigned when
/// the socket is connected.
pub fn tcp_socket() -> Result<i32, MinixErr> {
    let fd = unsafe { open("/dev/tcp", O_RDWR, 0) }?;
    Ok(fd)
}

/// Open a UDP socket and implicitly bind it to an ephemeral local port on
/// the local address (like `socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)`).
/// The socket starts unconnected in `NWUO_RWDATALL` mode, so `sendto`/
/// `recvfrom` work without a prior `connect` (the reference `socket(2)`
/// defaults); `connect` switches it to `NWUO_RWDATONLY`. Returns the file
/// descriptor.
pub fn udp_socket() -> Result<i32, MinixErr> {
    let fd = unsafe { open("/dev/udp", O_RDWR, 0) }?;
    let opt = NwioUdpOpt {
        nwuo_flags: NWUO_LP_SEL | NWUO_EN_LOC | NWUO_RWDATALL | NWUO_RA_ANY | NWUO_RP_ANY,
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

/// Bind a socket to `addr:port` (`addr` = `[0; 4]` for INADDR_ANY). The
/// socket type is discovered the reference way: probe `NWIOGTCPCONF`, fall
/// back to the UDP option ioctl on ENOTTY.
pub fn bind(fd: i32, addr: [u8; 4], port: u16) -> Result<(), MinixErr> {
    let mut probe = NwioTcpConf::default();
    let r = unsafe { ioctl(fd, NWIOGTCPCONF, &mut probe as *mut NwioTcpConf as *mut u8) };
    match r {
        Ok(_) => {
            let conf = NwioTcpConf {
                nwtc_flags: NWTC_LP_SET,
                nwtc_locaddr: 0,
                nwtc_remaddr: 0,
                nwtc_locport: port,
                nwtc_remport: 0,
            };
            unsafe { ioctl(fd, NWIOSTCPCONF, &conf as *const NwioTcpConf as *mut u8) }?;
            Ok(())
        }
        Err(e) if e.0 == 25 => {
            // ENOTTY — a UDP socket. Only the local-port group is touched
            // (the reference `bind(2)` semantics); the socket's RW mode and
            // remote ANY filters from socket()/connect() are preserved.
            let opt = NwioUdpOpt {
                nwuo_flags: NWUO_LP_SET | NWUO_EN_LOC,
                nwuo_locport: port,
                nwuo_remport: 0,
                nwuo_locaddr: u32::from_be_bytes(addr),
                nwuo_remaddr: 0,
            };
            unsafe { ioctl(fd, NWIOSUDPOPT, &opt as *const NwioUdpOpt as *mut u8) }?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Connect a socket to `addr:port`. For TCP this runs the three-way
/// handshake (blocking until established); for UDP it sets the default
/// destination and the filter for received datagrams. The socket type is
/// discovered the reference way: probe `NWIOGTCPCONF`, fall back to the
/// UDP option ioctl on ENOTTY.
pub fn connect(fd: i32, addr: [u8; 4], port: u16) -> Result<(), MinixErr> {
    let mut probe = NwioTcpConf::default();
    let r = unsafe { ioctl(fd, NWIOGTCPCONF, &mut probe as *mut NwioTcpConf as *mut u8) };
    match r {
        Ok(_) => {
            // TCP: set the remote address/port (auto local port), then
            // run the handshake.
            let conf = NwioTcpConf {
                nwtc_flags: NWTC_LP_SEL | NWTC_SET_RA | NWTC_SET_RP,
                nwtc_locaddr: 0,
                nwtc_remaddr: u32::from_be_bytes(addr),
                nwtc_locport: 0,
                nwtc_remport: port,
            };
            unsafe { ioctl(fd, NWIOSTCPCONF, &conf as *const NwioTcpConf as *mut u8) }?;
            let cl = NwioTcpCl {
                nwtcl_flags: TCF_DEFAULT,
                nwtcl_ttl: 0,
            };
            unsafe { ioctl(fd, NWIOTCPCONN, &cl as *const NwioTcpCl as *mut u8) }?;
            Ok(())
        }
        Err(e) if e.0 == 25 => {
            // ENOTTY — a UDP socket.
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
        Err(e) => Err(e),
    }
}

/// Read the current UDP socket options (NWIOGUDPOPT). `sendto`/`recvfrom`
/// use the RW mode to pick between the connected (RWDATONLY) and header
/// (RWDATALL) wire protocols.
fn udp_getopt(fd: i32) -> Result<NwioUdpOpt, MinixErr> {
    let mut opt = NwioUdpOpt::default();
    unsafe { ioctl(fd, NWIOGUDPOPT, &mut opt as *mut NwioUdpOpt as *mut u8) }?;
    Ok(opt)
}

/// `sendto(2)`: send one datagram, optionally to an explicit destination.
///
/// On a connected socket (`NWUO_RWDATONLY`) `dest` is ignored and the data
/// goes to the connected peer; on an unconnected socket the reference
/// protocol prefixes a 16-byte `udp_io_hdr_t` naming the destination, and
/// `dest` is required (ENOTCONN otherwise).
///
/// # Safety
///
/// `data` must be a valid byte slice.
pub unsafe fn sendto(fd: i32, data: &[u8], dest: Option<SocketAddr>) -> Result<i64, MinixErr> {
    let opt = udp_getopt(fd)?;
    if opt.nwuo_flags & NWUO_RWDATONLY != 0 {
        return unsafe { write(fd, data) };
    }
    let dest = match dest {
        Some(d) => d,
        None => return Err(MinixErr::from_i32(ENOTCONN)),
    };
    const MAX_PAYLOAD: usize = 2020; // RX_BUF_SIZE - IP - UDP headers
    if data.len() > MAX_PAYLOAD {
        return Err(MinixErr::from_i32(EMSGSIZE));
    }
    let mut buf = [0u8; UdpIoHdr::SIZE + MAX_PAYLOAD];
    let mut hdr = UdpIoHdr {
        uih_src_addr: [0; 4],
        uih_dst_addr: [0; 4],
        uih_src_port: 0,
        uih_dst_port: 0,
        uih_ip_opt_len: 0,
        uih_data_len: 0,
    };
    if opt.nwuo_flags & NWUO_RA_ANY != 0 {
        hdr.uih_dst_addr = dest.ip;
    }
    if opt.nwuo_flags & NWUO_RP_ANY != 0 {
        hdr.uih_dst_port = dest.port;
    }
    hdr.write_to(&mut buf[..UdpIoHdr::SIZE]);
    buf[UdpIoHdr::SIZE..UdpIoHdr::SIZE + data.len()].copy_from_slice(data);
    unsafe { write(fd, &buf[..UdpIoHdr::SIZE + data.len()]) }
}

/// `recvfrom(2)`: receive one datagram; returns the payload and the sender.
///
/// On a connected socket (`NWUO_RWDATONLY`) the sender is the connected
/// peer; on an unconnected socket the reference protocol prefixes a 16-byte
/// `udp_io_hdr_t` carrying the sender's address, which is stripped here.
/// `Ok((0, _))` means no datagram arrived within the server's poll window.
///
/// # Safety
///
/// `buf` must be a valid mutable byte slice.
pub unsafe fn recvfrom(fd: i32, buf: &mut [u8]) -> Result<(i64, SocketAddr), MinixErr> {
    let opt = udp_getopt(fd)?;
    if opt.nwuo_flags & NWUO_RWDATONLY != 0 {
        let n = unsafe { read(fd, buf) }?;
        let src = SocketAddr {
            ip: u32::to_be_bytes(opt.nwuo_remaddr),
            port: opt.nwuo_remport,
        };
        return Ok((n, src));
    }
    let mut scratch = [0u8; UdpIoHdr::SIZE + 2048];
    let n = unsafe { read(fd, &mut scratch) }?;
    if n < UdpIoHdr::SIZE as i64 {
        return Ok((0, SocketAddr::ANY));
    }
    let hdr = UdpIoHdr::read_from(&scratch[..UdpIoHdr::SIZE]);
    let n = ((n as usize) - UdpIoHdr::SIZE).min(buf.len());
    buf[..n].copy_from_slice(&scratch[UdpIoHdr::SIZE..UdpIoHdr::SIZE + n]);
    let src = SocketAddr {
        ip: hdr.uih_src_addr,
        port: hdr.uih_src_port,
    };
    Ok((n as i64, src))
}

/// `shutdown(2)`: SHUT_WR/SHUT_RDWR send our FIN (the socket keeps
/// reading); SHUT_RD is not supported by the net server (ENOSYS, like the
/// reference `_tcp_shutdown`).
pub fn shutdown(fd: i32, how: i32) -> Result<(), MinixErr> {
    match how {
        SHUT_WR | SHUT_RDWR => {
            unsafe { ioctl(fd, NWIOTCPSHUTDOWN, core::ptr::null_mut()) }.map(|_| ())
        }
        _ => Err(MinixErr::from_i32(crate::ENOSYS)),
    }
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

/// Mark a TCP socket as a listener with the given backlog, mirroring the
/// reference `listen(2)` (`NWIOTCPLISTENQ`). The socket must already be
/// bound to a local port.
pub fn listen(fd: i32, backlog: i32) -> Result<(), MinixErr> {
    unsafe { ioctl(fd, NWIOTCPLISTENQ, &backlog as *const i32 as *mut u8) }?;
    Ok(())
}

/// Accept the next pending connection on a listening TCP socket, mirroring
/// the reference `accept(2)`: open a fresh `/dev/tcp` fd, obtain its accept
/// cookie, and transfer the pending connection to it. Blocks (retrying
/// EAGAIN, which the net server returns when its bounded accept poll
/// expires) until a connection arrives. Returns the new socket fd.
pub fn accept(fd: i32) -> Result<i32, MinixErr> {
    let s1 = tcp_socket()?;
    let mut cookie = TcpCookie::default();
    let r = unsafe { ioctl(s1, NWIOGTCPCOOKIE, &mut cookie as *mut TcpCookie as *mut u8) };
    if let Err(e) = r {
        let _ = crate::fs::close(s1);
        return Err(e);
    }
    loop {
        match unsafe { ioctl(fd, NWIOTCPACCEPTTO, &cookie as *const TcpCookie as *mut u8) } {
            Ok(_) => return Ok(s1),
            // MinixErr stores errno positive; EAGAIN = -11 raw.
            Err(e) if e.0 == -EAGAIN => continue,
            Err(e) => {
                let _ = crate::fs::close(s1);
                return Err(e);
            }
        }
    }
}

/// Return the peer address of a connected TCP socket, mirroring the
/// reference `getpeername(2)` (reads `NWIOGTCPCONF` back).
pub fn getpeername(fd: i32) -> Result<([u8; 4], u16), MinixErr> {
    let mut conf = NwioTcpConf::default();
    unsafe { ioctl(fd, NWIOGTCPCONF, &mut conf as *mut NwioTcpConf as *mut u8) }?;
    Ok((u32::to_be_bytes(conf.nwtc_remaddr), conf.nwtc_remport))
}

/// Return the local address of a socket, mirroring the reference
/// `getsockname(2)` (reads `NWIOGTCPCONF` back; the local address is
/// `0.0.0.0` when bound to INADDR_ANY).
pub fn getsockname(fd: i32) -> Result<([u8; 4], u16), MinixErr> {
    let mut conf = NwioTcpConf::default();
    unsafe { ioctl(fd, NWIOGTCPCONF, &mut conf as *mut NwioTcpConf as *mut u8) }?;
    Ok((u32::to_be_bytes(conf.nwtc_locaddr), conf.nwtc_locport))
}

/// Close a socket (frees the net server's socket slot).
pub fn close(fd: i32) -> Result<(), MinixErr> {
    crate::fs::close(fd)
}

// ---- Application-facing wrappers ----

/// IPv4 socket address: network-order address bytes + host-order port,
/// mirroring `sockaddr_in` semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SocketAddr {
    pub ip: [u8; 4],
    pub port: u16,
}

/// Bound retries when the net server reports "no data" (its bounded RX
/// poll expired). The server returns 0 both for that and for peer FIN, so
/// the stream wrapper retries the no-data case a few times before
/// reporting EOF.
const READ_RETRIES: u32 = 4;
/// Short-write retries before `TcpStream::write` reports a partial write
/// (the single-segment send window returns 0 while unacked data is in
/// flight; a short recv poll drives the ACKs).
const WRITE_STALL_MAX: u32 = 8;

impl SocketAddr {
    /// INADDR_ANY: bind to all local addresses (with port 0 = ephemeral).
    pub const ANY: SocketAddr = SocketAddr {
        ip: [0; 4],
        port: 0,
    };

    pub const fn new(ip: [u8; 4], port: u16) -> Self {
        Self { ip, port }
    }

    /// Parse `"10.0.2.2:18080"`, or a bare `"10.0.2.2"` using
    /// `default_port`. Returns `None` on malformed input.
    pub fn parse(s: &str, default_port: u16) -> Option<Self> {
        let (ip_part, port_part) = match s.rsplit_once(':') {
            Some((ip, port)) => (ip, Some(port)),
            None => (s, None),
        };
        let ip = parse_ipv4(ip_part)?;
        let port = match port_part {
            Some(p) => p.parse::<u16>().ok()?,
            None => default_port,
        };
        Some(Self { ip, port })
    }

    pub fn is_any(&self) -> bool {
        self.ip == [0; 4]
    }
}

impl core::fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}:{}",
            self.ip[0], self.ip[1], self.ip[2], self.ip[3], self.port
        )
    }
}

/// Parse a dotted-quad IPv4 string into four bytes.
fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut part = 0usize;
    let mut val = 0u32;
    let mut digits = false;
    for &b in s.as_bytes() {
        match b {
            b'.' => {
                if !digits || val > 255 || part >= 3 {
                    return None;
                }
                out[part] = val as u8;
                part += 1;
                val = 0;
                digits = false;
            }
            b'0'..=b'9' => {
                val = val * 10 + (b - b'0') as u32;
                if val > 255 {
                    return None;
                }
                digits = true;
            }
            _ => return None,
        }
    }
    if !digits || part != 3 {
        return None;
    }
    out[3] = val as u8;
    Some(out)
}

/// A connected TCP byte stream.
#[derive(Debug)]
pub struct TcpStream {
    fd: i32,
}

impl TcpStream {
    /// Open a `/dev/tcp` socket and run the three-way handshake to `addr`
    /// (blocking until established).
    pub fn connect(addr: SocketAddr) -> Result<Self, MinixErr> {
        let fd = tcp_socket()?;
        if let Err(e) = crate::net::connect(fd, addr.ip, addr.port) {
            let _ = crate::net::close(fd);
            return Err(e);
        }
        Ok(Self { fd })
    }

    /// Read up to `buf.len()` bytes; `Ok(0)` means the peer closed (EOF).
    /// The server's bounded RX poll makes a no-data recv return 0 too, so
    /// the no-data case is retried a few times before EOF is reported.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, MinixErr> {
        for _ in 0..READ_RETRIES {
            match unsafe { crate::net::recv(self.fd, buf) } {
                Ok(0) => continue,
                Ok(n) => return Ok(n as usize),
                Err(e) => return Err(e),
            }
        }
        Ok(0)
    }

    /// Write all of `buf`. The single-segment send window can report 0
    /// while a previous segment is unacked, so short writes are retried
    /// (a short recv poll drives the ACKs); after `WRITE_STALL_MAX` stalls
    /// the partial count is returned.
    pub fn write(&self, buf: &[u8]) -> Result<usize, MinixErr> {
        let mut off = 0usize;
        let mut stalls = 0u32;
        while off < buf.len() {
            match unsafe { crate::net::send(self.fd, &buf[off..]) } {
                Ok(n) if n > 0 => {
                    off += n as usize;
                    stalls = 0;
                }
                Ok(_) => {
                    stalls += 1;
                    if stalls > WRITE_STALL_MAX {
                        break;
                    }
                    let mut tmp = [0u8; 64];
                    let _ = unsafe { crate::net::recv(self.fd, &mut tmp) };
                }
                Err(e) => return Err(e),
            }
        }
        Ok(off)
    }

    /// The peer's address.
    pub fn peer_addr(&self) -> Result<SocketAddr, MinixErr> {
        getpeername(self.fd).map(|(ip, port)| SocketAddr { ip, port })
    }

    /// Half-close the write side (`shutdown(2)` with SHUT_WR): send our
    /// FIN but keep reading the peer's remaining data.
    pub fn shutdown(&self) -> Result<(), MinixErr> {
        crate::net::shutdown(self.fd, SHUT_WR)
    }

    /// Close the stream (sends FIN; the net server completes the close
    /// handshake in the background).
    pub fn close(self) -> Result<(), MinixErr> {
        crate::net::close(self.fd)
    }
}

/// A listening TCP socket.
#[derive(Debug)]
pub struct TcpListener {
    fd: i32,
}

impl TcpListener {
    /// Open a `/dev/tcp` socket and bind it to `addr` (use
    /// [`SocketAddr::ANY`]-style `ip = [0; 4]` for INADDR_ANY).
    pub fn bind(addr: SocketAddr) -> Result<Self, MinixErr> {
        let fd = tcp_socket()?;
        if let Err(e) = crate::net::bind(fd, addr.ip, addr.port) {
            let _ = crate::net::close(fd);
            return Err(e);
        }
        Ok(Self { fd })
    }

    /// Put the socket into the listening state with the given backlog.
    pub fn listen(&self, backlog: i32) -> Result<(), MinixErr> {
        crate::net::listen(self.fd, backlog)
    }

    /// Block until a connection arrives; returns the accepted stream and
    /// its peer address (via `getpeername`, like the reference
    /// `accept(2)`).
    pub fn accept(&self) -> Result<(TcpStream, SocketAddr), MinixErr> {
        let fd = accept(self.fd)?;
        let peer = match getpeername(fd) {
            Ok((ip, port)) => SocketAddr { ip, port },
            Err(_) => SocketAddr::ANY,
        };
        Ok((TcpStream { fd }, peer))
    }

    pub fn close(self) -> Result<(), MinixErr> {
        crate::net::close(self.fd)
    }
}

/// A UDP datagram socket.
#[derive(Debug)]
pub struct UdpSocket {
    fd: i32,
}

impl UdpSocket {
    /// Open a `/dev/udp` socket bound to `addr` (`ip = [0; 4]` for
    /// INADDR_ANY).
    pub fn bind(addr: SocketAddr) -> Result<Self, MinixErr> {
        let fd = udp_socket()?;
        if let Err(e) = crate::net::bind(fd, addr.ip, addr.port) {
            let _ = crate::net::close(fd);
            return Err(e);
        }
        Ok(Self { fd })
    }

    /// Set the default destination and the filter for received datagrams.
    pub fn connect(&self, addr: SocketAddr) -> Result<(), MinixErr> {
        crate::net::connect(self.fd, addr.ip, addr.port)
    }

    /// Send one whole datagram to the connected peer. On an unconnected
    /// socket this is ENOTCONN (no destination — use [`UdpSocket::send_to`]).
    pub fn send(&self, data: &[u8]) -> Result<usize, MinixErr> {
        unsafe { crate::net::sendto(self.fd, data, None) }.map(|n| n as usize)
    }

    /// Receive one datagram payload (from the connected peer, or from any
    /// sender on an unconnected socket). `Ok(0)` means no datagram arrived
    /// within the server's bounded poll window.
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize, MinixErr> {
        unsafe { crate::net::recvfrom(self.fd, buf) }.map(|(n, _)| n as usize)
    }

    /// Send one whole datagram to `dest` (`sendto(2)` — works without
    /// `connect`).
    pub fn send_to(&self, data: &[u8], dest: SocketAddr) -> Result<usize, MinixErr> {
        unsafe { crate::net::sendto(self.fd, data, Some(dest)) }.map(|n| n as usize)
    }

    /// Receive one datagram and the sender's address (`recvfrom(2)`).
    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), MinixErr> {
        unsafe { crate::net::recvfrom(self.fd, buf) }.map(|(n, a)| (n as usize, a))
    }

    pub fn close(self) -> Result<(), MinixErr> {
        crate::net::close(self.fd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_addr_parses_dotted_quad_with_port() {
        assert_eq!(
            SocketAddr::parse("10.0.2.2:18080", 0),
            Some(SocketAddr {
                ip: [10, 0, 2, 2],
                port: 18080,
            })
        );
    }

    #[test]
    fn socket_addr_parses_bare_ip_with_default_port() {
        assert_eq!(
            SocketAddr::parse("10.0.2.2", 20000),
            Some(SocketAddr {
                ip: [10, 0, 2, 2],
                port: 20000,
            })
        );
    }

    #[test]
    fn socket_addr_rejects_malformed_input() {
        assert_eq!(SocketAddr::parse("", 1), None);
        assert_eq!(SocketAddr::parse("10.0.2", 1), None);
        assert_eq!(SocketAddr::parse("10.0.2.2.3", 1), None);
        assert_eq!(SocketAddr::parse("10.0.2.256", 1), None);
        assert_eq!(SocketAddr::parse("10.0.2.2:70000", 1), None);
        assert_eq!(SocketAddr::parse("a.b.c.d", 1), None);
        assert_eq!(SocketAddr::parse("10.0.2.2:", 1), None);
    }

    #[test]
    fn socket_addr_formats_and_compares() {
        struct FmtBuf([u8; 32], usize);
        impl core::fmt::Write for FmtBuf {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                let n = s.len().min(self.0.len() - self.1);
                self.0[self.1..self.1 + n].copy_from_slice(s.as_bytes());
                self.1 += n;
                Ok(())
            }
        }
        let a = SocketAddr::new([10, 0, 2, 15], 20000);
        let mut b = FmtBuf([0; 32], 0);
        core::fmt::Write::write_fmt(&mut b, format_args!("{a}")).unwrap();
        assert_eq!(&b.0[..b.1], b"10.0.2.15:20000");
        assert!(!a.is_any());
        assert!(SocketAddr::ANY.is_any());
        assert_eq!(a, SocketAddr::parse("10.0.2.15:20000", 0).unwrap());
    }
}
