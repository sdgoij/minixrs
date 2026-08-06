//! MINIX network protocol definitions shared across the port.
//!
//! This is the Rust home of the reference `minix/net/` wire and ioctl
//! protocol (16.1): the `/dev/udp` `nwio_udpopt_t` option struct, the
//! `NWIOSUDPOPT`/`NWIOGUDPOPT` ioctl request codes and the `NWUO_*` flag
//! bits, plus the NetBSD-style ioctl encoding helpers VFS uses to size the
//! arg-struct copy.
//!
//! Both sides of every protocol live in this port (minix-std's socket API
//! and the net server's UDP implementation), so the struct layouts only
//! need to be mutually consistent — but they are kept byte-identical to
//! the reference `include/net/gen/*.h` so reference userland can be built
//! against the same protocol later.

#![no_std]

mod nwio;
mod tcp;

pub use nwio::{
    NWIOGUDPOPT, NWIOSUDPOPT, NWUO_ACC_MASK, NWUO_BROAD_MASK, NWUO_COPY, NWUO_DI_BROAD,
    NWUO_DI_IPOPT, NWUO_DI_LOC, NWUO_EN_BROAD, NWUO_EN_IPOPT, NWUO_EN_LOC, NWUO_EXCL,
    NWUO_IPOPT_MASK, NWUO_LOCADDR_MASK, NWUO_LOCPORT_MASK, NWUO_LP_ANY, NWUO_LP_SEL, NWUO_LP_SET,
    NWUO_NOFLAGS, NWUO_RA_ANY, NWUO_RA_SET, NWUO_REMADDR_MASK, NWUO_REMPORT_MASK, NWUO_RP_ANY,
    NWUO_RP_SET, NWUO_RW_MASK, NWUO_RWDATALL, NWUO_RWDATONLY, NWUO_SHARED, NwioUdpOpt,
};
pub use tcp::{
    NWIOGTCPCONF, NWIOGTCPCOOKIE, NWIOSTCPCONF, NWIOTCPACCEPTTO, NWIOTCPCONN, NWIOTCPLISTENQ,
    NWTC_ACC_MASK, NWTC_COPY, NWTC_EXCL, NWTC_LOCPORT_MASK, NWTC_LP_SEL, NWTC_LP_SET,
    NWTC_LP_UNSET, NWTC_NOFLAGS, NWTC_REMADDR_MASK, NWTC_REMPORT_MASK, NWTC_SET_RA, NWTC_SET_RP,
    NWTC_SHARED, NWTC_UNSET_RA, NWTC_UNSET_RP, NwioTcpCl, NwioTcpConf, TCF_ASYNCH, TCF_DEFAULT,
    TcpCookie,
};

/// IPv4 address, network byte order (matches `ipaddr_t`).
pub type IpAddr = [u8; 4];

/// UDP/TCP port number (big-endian on the wire, matches `udpport_t`).
pub type Port = u16;

/// NetBSD-style ioctl encoding (`.refs/minix-3.3.0/sys/sys/ioctl.h`).
mod ioc {
    /// `IOC_OUT` — device writes data to the user (read ioctl, `_IOR`).
    pub const IOC_OUT: u32 = 0x4000_0000;
    /// `IOC_IN` — user writes data to the device (write ioctl, `_IOW`).
    pub const IOC_IN: u32 = 0x8000_0000;
    /// `IOC_INOUT` — bidirectional.
    pub const IOC_INOUT: u32 = 0xC000_0000;
    /// `IOCPARM_MASK` — size field width.
    pub const IOCPARM_MASK: u32 = 0x1fff;
}

/// Encode an ioctl request: `_IOW`/`_IOR` with the NetBSD layout
/// (direction in bits 30-31, size in bits 16-28, group in bits 8-15,
/// number in bits 0-7).
pub const fn ioc_encode(dir: u32, group: u8, num: u8, size: usize) -> u32 {
    dir | ((size as u32) & ioc::IOCPARM_MASK) << 16 | ((group as u32) << 8) | (num as u32)
}

/// Byte size of the arg struct encoded in an ioctl request.
pub const fn ioc_size(request: u32) -> usize {
    ((request >> 16) & ioc::IOCPARM_MASK) as usize
}

/// True for `_IOR` ioctls: the device returns data to the user.
pub const fn ioc_is_out(request: u32) -> bool {
    request & 0xC000_0000 == ioc::IOC_OUT || request & 0xC000_0000 == ioc::IOC_INOUT
}

/// True for `_IOW` ioctls: the user passes data to the device.
pub const fn ioc_is_in(request: u32) -> bool {
    request & 0xC000_0000 == ioc::IOC_IN || request & 0xC000_0000 == ioc::IOC_INOUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_ioctl_codes_match_reference_encoding() {
        // _IOW('n', 64, struct nwio_udpopt) / _IOR('n', 65, ...) with a
        // 16-byte struct: direction + (16 << 16) + ('n' << 8) + number.
        assert_eq!(
            NWIOSUDPOPT,
            0x8000_0000 | (16 << 16) | ((b'n' as u32) << 8) | 64
        );
        assert_eq!(
            NWIOGUDPOPT,
            0x4000_0000 | (16 << 16) | ((b'n' as u32) << 8) | 65
        );
    }

    #[test]
    fn ioctl_size_and_direction_are_decoded() {
        assert_eq!(ioc_size(NWIOSUDPOPT), 16);
        assert!(ioc_is_in(NWIOSUDPOPT));
        assert!(!ioc_is_out(NWIOSUDPOPT));
        assert!(ioc_is_out(NWIOGUDPOPT));
        assert!(!ioc_is_in(NWIOGUDPOPT));
        assert_eq!(ioc_size(0), 0);
    }
}
