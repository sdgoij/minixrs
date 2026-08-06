//! `/dev/udp` ioctl protocol — `nwio_udpopt_t` and the `NWUO_*` flags.
//!
//! Mirrors `.refs/minix-3.3.0/minix/include/net/gen/udp_io.h`. The struct
//! is byte-identical to the reference (flags is `unsigned long` on the
//! reference's ILP32 ABI, so 4 bytes; the remaining fields are `u16`/`u32`).

use crate::ioc_encode;

/// UDP socket options (reference `struct nwio_udpopt`).
///
/// Addresses are in network byte order; `0` (INADDR_ANY) means unset.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NwioUdpOpt {
    pub nwuo_flags: u32,
    pub nwuo_locport: u16,
    pub nwuo_remport: u16,
    pub nwuo_locaddr: u32,
    pub nwuo_remaddr: u32,
}

impl NwioUdpOpt {
    /// Size of the wire struct (16 bytes — 4 + 2 + 2 + 4 + 4).
    pub const SIZE: usize = 16;

    /// Serialize into `out` (must be at least [`NwioUdpOpt::SIZE`] bytes).
    pub fn write_to(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.nwuo_flags.to_ne_bytes());
        out[4..6].copy_from_slice(&self.nwuo_locport.to_ne_bytes());
        out[6..8].copy_from_slice(&self.nwuo_remport.to_ne_bytes());
        out[8..12].copy_from_slice(&self.nwuo_locaddr.to_ne_bytes());
        out[12..16].copy_from_slice(&self.nwuo_remaddr.to_ne_bytes());
    }

    /// Parse from `src` (must be at least [`NwioUdpOpt::SIZE`] bytes).
    pub fn read_from(src: &[u8]) -> Self {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&src[0..4]);
        let flags = u32::from_ne_bytes(bytes);
        let locport = u16::from_ne_bytes([src[4], src[5]]);
        let remport = u16::from_ne_bytes([src[6], src[7]]);
        bytes.copy_from_slice(&src[8..12]);
        let locaddr = u32::from_ne_bytes(bytes);
        bytes.copy_from_slice(&src[12..16]);
        let remaddr = u32::from_ne_bytes(bytes);
        Self {
            nwuo_flags: flags,
            nwuo_locport: locport,
            nwuo_remport: remport,
            nwuo_locaddr: locaddr,
            nwuo_remaddr: remaddr,
        }
    }
}

/// UDP datagram I/O header (reference `struct udp_io_hdr`, 16 bytes).
///
/// Prefixed to each datagram when the socket is in `NWUO_RWDATALL` mode:
/// the sender fills the destination (and optionally source) fields, the
/// receiver reads the source. Addresses are raw network-order bytes; ports
/// and the two length fields are big-endian on the wire (the reference's
/// `CONF_UDP_IO_NW_BYTE_ORDER` layout).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UdpIoHdr {
    pub uih_src_addr: [u8; 4],
    pub uih_dst_addr: [u8; 4],
    pub uih_src_port: u16,
    pub uih_dst_port: u16,
    pub uih_ip_opt_len: u16,
    pub uih_data_len: u16,
}

impl UdpIoHdr {
    /// Wire size: 4 + 4 + 2 + 2 + 2 + 2 = 16 bytes.
    pub const SIZE: usize = 16;

    /// Serialize into `out` (must be at least [`UdpIoHdr::SIZE`] bytes).
    pub fn write_to(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.uih_src_addr);
        out[4..8].copy_from_slice(&self.uih_dst_addr);
        out[8..10].copy_from_slice(&self.uih_src_port.to_be_bytes());
        out[10..12].copy_from_slice(&self.uih_dst_port.to_be_bytes());
        out[12..14].copy_from_slice(&self.uih_ip_opt_len.to_be_bytes());
        out[14..16].copy_from_slice(&self.uih_data_len.to_be_bytes());
    }

    /// Parse from `src` (must be at least [`UdpIoHdr::SIZE`] bytes).
    pub fn read_from(src: &[u8]) -> Self {
        let mut src_addr = [0u8; 4];
        src_addr.copy_from_slice(&src[0..4]);
        let mut dst_addr = [0u8; 4];
        dst_addr.copy_from_slice(&src[4..8]);
        Self {
            uih_src_addr: src_addr,
            uih_dst_addr: dst_addr,
            uih_src_port: u16::from_be_bytes([src[8], src[9]]),
            uih_dst_port: u16::from_be_bytes([src[10], src[11]]),
            uih_ip_opt_len: u16::from_be_bytes([src[12], src[13]]),
            uih_data_len: u16::from_be_bytes([src[14], src[15]]),
        }
    }
}

// ---- ioctl request codes ----

/// `_IOW('n', 64, struct nwio_udpopt)` — set UDP socket options.
pub const NWIOSUDPOPT: u32 = ioc_encode(0x8000_0000, b'n', 64, NwioUdpOpt::SIZE);
/// `_IOR('n', 65, struct nwio_udpopt)` — get UDP socket options.
pub const NWIOGUDPOPT: u32 = ioc_encode(0x4000_0000, b'n', 65, NwioUdpOpt::SIZE);

/// `_IOR('f', 1, int)` — bytes available for reading (generic, reference
/// `sys/ioc_file.h`).
pub const FIONREAD: u32 = ioc_encode(0x4000_0000, b'f', 1, 4);

// ---- NWUO_* flag bits (reference `net/gen/udp_io.h`) ----

pub const NWUO_NOFLAGS: u32 = 0x0000;
pub const NWUO_ACC_MASK: u32 = 0x0003;
pub const NWUO_EXCL: u32 = 0x0001;
pub const NWUO_SHARED: u32 = 0x0002;
pub const NWUO_COPY: u32 = 0x0003;
pub const NWUO_LOCPORT_MASK: u32 = 0x000c;
pub const NWUO_LP_SEL: u32 = 0x0004;
pub const NWUO_LP_SET: u32 = 0x0008;
pub const NWUO_LP_ANY: u32 = 0x000c;
pub const NWUO_LOCADDR_MASK: u32 = 0x0010;
pub const NWUO_EN_LOC: u32 = 0x0010;
pub const NWUO_DI_LOC: u32 = 0x0010_0000;
pub const NWUO_BROAD_MASK: u32 = 0x0020;
pub const NWUO_EN_BROAD: u32 = 0x0020;
pub const NWUO_DI_BROAD: u32 = 0x0020_0000;
pub const NWUO_REMPORT_MASK: u32 = 0x0100;
pub const NWUO_RP_SET: u32 = 0x0100;
pub const NWUO_RP_ANY: u32 = 0x0100_0000;
pub const NWUO_REMADDR_MASK: u32 = 0x0200;
pub const NWUO_RA_SET: u32 = 0x0200;
pub const NWUO_RA_ANY: u32 = 0x0200_0000;
pub const NWUO_RW_MASK: u32 = 0x1000;
pub const NWUO_RWDATONLY: u32 = 0x0000_1000;
pub const NWUO_RWDATALL: u32 = 0x1000_0000;
pub const NWUO_IPOPT_MASK: u32 = 0x2000;
pub const NWUO_EN_IPOPT: u32 = 0x2000;
pub const NWUO_DI_IPOPT: u32 = 0x2000_0000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_io_hdr_round_trips_through_bytes() {
        let hdr = UdpIoHdr {
            uih_src_addr: [10, 0, 2, 15],
            uih_dst_addr: [10, 0, 2, 3],
            uih_src_port: 12345,
            uih_dst_port: 53,
            uih_ip_opt_len: 0,
            uih_data_len: 512,
        };
        let mut bytes = [0u8; UdpIoHdr::SIZE];
        hdr.write_to(&mut bytes);
        // Ports and lengths are big-endian on the wire.
        assert_eq!(&bytes[8..10], &[0x30, 0x39]); // 12345
        assert_eq!(&bytes[10..12], &[0x00, 0x35]); // 53
        assert_eq!(&bytes[14..16], &[0x02, 0x00]); // 512
        assert_eq!(UdpIoHdr::read_from(&bytes), hdr);
        assert_eq!(core::mem::size_of::<UdpIoHdr>(), 16);
    }

    #[test]
    fn udpopt_round_trips_through_bytes() {
        let opt = NwioUdpOpt {
            nwuo_flags: NWUO_LP_SET | NWUO_RP_SET | NWUO_RA_SET | NWUO_RWDATONLY,
            nwuo_locport: 1234,
            nwuo_remport: 53,
            nwuo_locaddr: 0x0a00_020f, // 10.0.2.15
            nwuo_remaddr: 0x0a00_0203, // 10.0.2.3
        };
        let mut bytes = [0u8; NwioUdpOpt::SIZE];
        opt.write_to(&mut bytes);
        let parsed = NwioUdpOpt::read_from(&bytes);
        assert_eq!(opt, parsed);
    }

    #[test]
    fn udpopt_byte_layout_matches_c_struct() {
        // C: { unsigned long flags; udpport_t locport; udpport_t remport;
        //      ipaddr_t locaddr; ipaddr_t remaddr; } = 4+2+2+4+4 = 16.
        assert_eq!(core::mem::size_of::<NwioUdpOpt>(), 16);
        let mut bytes = [0u8; 16];
        let opt = NwioUdpOpt {
            nwuo_flags: 0xdead_beef,
            nwuo_locport: 0x1234,
            nwuo_remport: 0x5678,
            nwuo_locaddr: 0x0a00_020f,
            nwuo_remaddr: 0x0a00_0203,
        };
        opt.write_to(&mut bytes);
        assert_eq!(&bytes[0..4], &[0xef, 0xbe, 0xad, 0xde]); // native-endian flags
        assert_eq!(&bytes[4..6], &[0x34, 0x12]);
        assert_eq!(&bytes[6..8], &[0x78, 0x56]);
        assert_eq!(&bytes[8..12], &[0x0f, 0x02, 0x00, 0x0a]); // 10.0.2.15 LE
        assert_eq!(&bytes[12..16], &[0x03, 0x02, 0x00, 0x0a]); // 10.0.2.3 LE
    }
}
