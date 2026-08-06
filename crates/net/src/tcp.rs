//! `/dev/tcp` ioctl protocol — `nwio_tcpconf_t`, `nwio_tcpcl_t` and flags.
//!
//! Mirrors `.refs/minix-3.3.0/minix/include/net/gen/tcp_io.h` byte-for-byte
//! (both structs are 16/8 bytes on the reference's ILP32 ABI).

use crate::ioc_encode;

/// TCP socket configuration (reference `struct nwio_tcpconf`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NwioTcpConf {
    pub nwtc_flags: u32,
    pub nwtc_locaddr: u32,
    pub nwtc_remaddr: u32,
    pub nwtc_locport: u16,
    pub nwtc_remport: u16,
}

impl NwioTcpConf {
    /// Wire size: 4 + 4 + 4 + 2 + 2 = 16 bytes.
    pub const SIZE: usize = 16;

    /// Serialize into `out` (at least [`NwioTcpConf::SIZE`] bytes).
    pub fn write_to(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.nwtc_flags.to_ne_bytes());
        out[4..8].copy_from_slice(&self.nwtc_locaddr.to_ne_bytes());
        out[8..12].copy_from_slice(&self.nwtc_remaddr.to_ne_bytes());
        out[12..14].copy_from_slice(&self.nwtc_locport.to_ne_bytes());
        out[14..16].copy_from_slice(&self.nwtc_remport.to_ne_bytes());
    }

    /// Parse from `src` (at least [`NwioTcpConf::SIZE`] bytes).
    pub fn read_from(src: &[u8]) -> Self {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&src[0..4]);
        let flags = u32::from_ne_bytes(bytes);
        bytes.copy_from_slice(&src[4..8]);
        let locaddr = u32::from_ne_bytes(bytes);
        bytes.copy_from_slice(&src[8..12]);
        let remaddr = u32::from_ne_bytes(bytes);
        Self {
            nwtc_flags: flags,
            nwtc_locaddr: locaddr,
            nwtc_remaddr: remaddr,
            nwtc_locport: u16::from_ne_bytes([src[12], src[13]]),
            nwtc_remport: u16::from_ne_bytes([src[14], src[15]]),
        }
    }
}

/// TCP connect parameters (reference `struct nwio_tcpcl`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NwioTcpCl {
    pub nwtcl_flags: u32,
    pub nwtcl_ttl: u32,
}

impl NwioTcpCl {
    /// Wire size: 4 + 4 = 8 bytes.
    pub const SIZE: usize = 8;

    pub fn write_to(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.nwtcl_flags.to_ne_bytes());
        out[4..8].copy_from_slice(&self.nwtcl_ttl.to_ne_bytes());
    }

    pub fn read_from(src: &[u8]) -> Self {
        Self {
            nwtcl_flags: u32::from_ne_bytes([src[0], src[1], src[2], src[3]]),
            nwtcl_ttl: u32::from_ne_bytes([src[4], src[5], src[6], src[7]]),
        }
    }
}

// ---- ioctl request codes ----

/// `_IOW('n', 48, struct nwio_tcpconf)` — set TCP configuration.
pub const NWIOSTCPCONF: u32 = ioc_encode(0x8000_0000, b'n', 48, NwioTcpConf::SIZE);
/// `_IOR('n', 49, struct nwio_tcpconf)` — get TCP configuration.
pub const NWIOGTCPCONF: u32 = ioc_encode(0x4000_0000, b'n', 49, NwioTcpConf::SIZE);
/// `_IOW('n', 50, struct nwio_tcpcl)` — initiate a connection.
pub const NWIOTCPCONN: u32 = ioc_encode(0x8000_0000, b'n', 50, NwioTcpCl::SIZE);
/// `_IOW('n', 57, int)` — listen with a backlog.
pub const NWIOTCPLISTENQ: u32 = ioc_encode(0x8000_0000, b'n', 57, 4);
/// `_IOR('n', 58, struct tcp_cookie)` — get an accept cookie for this
/// (fresh) socket.
pub const NWIOGTCPCOOKIE: u32 = ioc_encode(0x4000_0000, b'n', 58, TcpCookie::SIZE);
/// `_IOW('n', 59, struct tcp_cookie)` — on the listening socket, transfer
/// the next pending connection to the cookie-identified socket.
pub const NWIOTCPACCEPTTO: u32 = ioc_encode(0x8000_0000, b'n', 59, TcpCookie::SIZE);
/// `_IO('n', 53)` — shutdown (send FIN); the libc only calls it for
/// SHUT_WR/SHUT_RDWR.
pub const NWIOTCPSHUTDOWN: u32 = ioc_encode(0, b'n', 53, 0);
/// `_IO('n', 56)` — push buffered data (no-op here: we transmit immediately).
pub const NWIOTCPPUSH: u32 = ioc_encode(0, b'n', 56, 0);
/// `_IOR('n', 60, int)` — fetch and clear the socket error (SO_ERROR).
pub const NWIOTCPGERROR: u32 = ioc_encode(0x4000_0000, b'n', 60, 4);

/// Accept cookie (reference `struct tcp_cookie`): a per-socket handle the
/// listener uses to name the fresh socket that accept() opened.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TcpCookie {
    pub tc_ref: u32,
    pub tc_secret: [u8; 12],
}

impl TcpCookie {
    /// Wire size: 4 + 12 = 16 bytes.
    pub const SIZE: usize = 16;

    pub fn write_to(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.tc_ref.to_ne_bytes());
        out[4..16].copy_from_slice(&self.tc_secret);
    }

    pub fn read_from(src: &[u8]) -> Self {
        let mut secret = [0u8; 12];
        secret.copy_from_slice(&src[4..16]);
        Self {
            tc_ref: u32::from_ne_bytes([src[0], src[1], src[2], src[3]]),
            tc_secret: secret,
        }
    }
}

// ---- NWTC_* flag bits (reference `net/gen/tcp_io.h`) ----

pub const NWTC_NOFLAGS: u32 = 0x0000;
pub const NWTC_ACC_MASK: u32 = 0x0003;
pub const NWTC_EXCL: u32 = 0x0001;
pub const NWTC_SHARED: u32 = 0x0002;
pub const NWTC_COPY: u32 = 0x0003;
pub const NWTC_LOCPORT_MASK: u32 = 0x0030;
pub const NWTC_LP_UNSET: u32 = 0x0010;
pub const NWTC_LP_SET: u32 = 0x0020;
pub const NWTC_LP_SEL: u32 = 0x0030;
pub const NWTC_REMADDR_MASK: u32 = 0x0100;
pub const NWTC_SET_RA: u32 = 0x0100;
pub const NWTC_UNSET_RA: u32 = 0x0100_0000;
pub const NWTC_REMPORT_MASK: u32 = 0x0200;
pub const NWTC_SET_RP: u32 = 0x0200;
pub const NWTC_UNSET_RP: u32 = 0x0200_0000;

// ---- TCF_* connect flags ----

/// Blocking connect: return once the connection is established.
pub const TCF_DEFAULT: u32 = 0;
/// Asynchronous (non-blocking) connect.
pub const TCF_ASYNCH: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcpconf_round_trips_through_bytes() {
        let conf = NwioTcpConf {
            nwtc_flags: NWTC_LP_SEL | NWTC_SET_RA | NWTC_SET_RP,
            nwtc_locaddr: 0,
            nwtc_remaddr: 0x0a00_0202, // 10.0.2.2
            nwtc_locport: 0,
            nwtc_remport: 18080,
        };
        let mut bytes = [0u8; NwioTcpConf::SIZE];
        conf.write_to(&mut bytes);
        assert_eq!(NwioTcpConf::read_from(&bytes), conf);
    }

    #[test]
    fn tcpconf_byte_layout_matches_c_struct() {
        assert_eq!(core::mem::size_of::<NwioTcpConf>(), 16);
        assert_eq!(core::mem::size_of::<NwioTcpCl>(), 8);
    }

    #[test]
    fn cookie_round_trips_through_bytes() {
        let cookie = TcpCookie {
            tc_ref: 0x0102_0304,
            tc_secret: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        };
        let mut bytes = [0u8; TcpCookie::SIZE];
        cookie.write_to(&mut bytes);
        assert_eq!(TcpCookie::read_from(&bytes), cookie);
        assert_eq!(core::mem::size_of::<TcpCookie>(), 16);
    }

    #[test]
    fn tcp_ioctl_codes_match_reference_encoding() {
        // _IOW('n', 48, ...) size 16, _IOR('n', 49, ...) size 16,
        // _IOW('n', 50, ...) size 8.
        assert_eq!(
            NWIOSTCPCONF,
            0x8000_0000 | (16 << 16) | ((b'n' as u32) << 8) | 48
        );
        assert_eq!(
            NWIOGTCPCONF,
            0x4000_0000 | (16 << 16) | ((b'n' as u32) << 8) | 49
        );
        assert_eq!(
            NWIOTCPCONN,
            0x8000_0000 | (8 << 16) | ((b'n' as u32) << 8) | 50
        );
        // listen(2) backlog (int, 4 bytes) and the accept cookie (16 bytes).
        assert_eq!(
            NWIOTCPLISTENQ,
            0x8000_0000 | (4 << 16) | ((b'n' as u32) << 8) | 57
        );
        assert_eq!(
            NWIOGTCPCOOKIE,
            0x4000_0000 | (16 << 16) | ((b'n' as u32) << 8) | 58
        );
        assert_eq!(
            NWIOTCPACCEPTTO,
            0x8000_0000 | (16 << 16) | ((b'n' as u32) << 8) | 59
        );
        // shutdown (no arg), push (no arg) and SO_ERROR (int).
        assert_eq!(NWIOTCPSHUTDOWN, ((b'n' as u32) << 8) | 53);
        assert_eq!(NWIOTCPPUSH, ((b'n' as u32) << 8) | 56);
        assert_eq!(
            NWIOTCPGERROR,
            0x4000_0000 | (4 << 16) | ((b'n' as u32) << 8) | 60
        );
    }
}
