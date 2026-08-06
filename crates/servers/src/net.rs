//! net server — ARP/ICMP chardriver for `/dev/ip`, UDP socket chardriver
//! for `/dev/udp`, driving DL_* to virtio_net.
//!
//! The net server is a character driver (like tty) registered with VFS as
//! major [`NET_MAJOR`]; VFS routes `open("/dev/ip")` and subsequent reads
//! and writes here. It is also a *DL client*: it hands virtio_net grants
//! for packet buffers and SENDRECs `DL_*` requests, exactly as MFS's BDEV
//! path works against virtio_blk.
//!
//! Device protocol (this port's minimal subset of the MINIX /dev/ip
//! interface, bounded by the chardev inline payloads):
//!
//! - `write(fd, req[8])` sends an ICMP echo request. The 8 bytes are
//!   big-endian: `dst_ip[4] id[2] seq[2]`. The server ARP-resolves the
//!   destination, frames an Ethernet/IP/ICMP packet, and transmits it via
//!   `DL_WRITEV_S`.
//! - `read(fd, buf)` returns the next matching ICMP echo reply as a raw IP
//!   datagram (fits the 48-byte inline read cap). It polls the NIC via
//!   `DL_READV_S` for a bounded time.
//!
//! `/dev/udp` (minor 1) is a *datagram socket* device: each open is cloned
//! to a fresh minor by VFS, and reads/writes carry one whole UDP datagram
//! per request via vircopy (VFS passes the user VA in m2_l1, the byte
//! count in m2_l2, flagged with `CDEV_DGRAM`). Socket options are set with
//! the MINIX `NWIOSUDPOPT`/`NWIOGUDPOPT` ioctls (`nwio_udpopt_t`), so
//! bind()/connect() follow the reference `/dev/udp` protocol.
//!
//! Inbound ICMP echo requests for this host are answered automatically.
//!
//! Addressing: the guest is 10.0.2.15 on QEMU's SLIRP network; the
//! gateway (and default ARP target) is 10.0.2.2.

//! # Dead-code allowance
//!
//! All functions in this module are used only by the `net` binary target
//! (`src/bin/net.rs`), not by the `servers` library target. Clippy's
//! `dead_code` lint fires for library builds. The `dead_code` allowance is
//! intentional — the binary target does use everything.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use arch_common::com::{
    CDEV_CLONED, CDEV_CLOSE, CDEV_DGRAM, CDEV_DGRAM_OPEN, CDEV_IOCTL, CDEV_OPEN, CDEV_READ,
    CDEV_SELECT, CDEV_WRITE, DL_CONF, DL_CONF_REPLY, DL_NOMODE, DL_READV_S, DL_TASK_REPLY,
    DL_WRITEV_S, VIRTIO_NET_PROC_NR,
};
use arch_common::ipc::Message;
use arch_common::safecopies::{
    CPF_DIRECT, CPF_READ, CPF_USED, CPF_VALID, CPF_WRITE, CpDirect, CpGrant, CpUnion, GRANT_INVALID,
};
use net::{
    NWIOGUDPOPT, NWIOSUDPOPT, NWUO_EN_LOC, NWUO_LP_SEL, NWUO_LP_SET, NWUO_RA_ANY, NWUO_RA_SET,
    NWUO_RP_ANY, NWUO_RP_SET, NWUO_RWDATALL, NwioUdpOpt,
};

/// Our IP on the SLIRP network (10.0.2.15).
const OUR_IP: [u8; 4] = [10, 0, 2, 15];
/// SLIRP gateway — the ARP/ICMP peer a `ping 10.0.2.2` reaches.
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];

/// Ethernet types.
const ETH_TYPE_ARP: u16 = 0x0806;
const ETH_TYPE_IP: u16 = 0x0800;

/// IP protocol number for ICMP.
const IP_PROTO_ICMP: u8 = 1;
/// IP protocol number for UDP.
const IP_PROTO_UDP: u8 = 17;

/// UDP header length (src port, dst port, length, checksum).
const UDP_HDR_LEN: usize = 8;

/// Static (non-cloned) minor for /dev/ip.
const IP_DEV_MINOR: i32 = 0;
/// Static minor for /dev/udp — each open clones to a fresh socket minor.
const UDP_DEV_MINOR: i32 = 1;

/// First clone minor handed out for UDP sockets.
const SOCKET_MINOR_BASE: i32 = 0x10;
/// Number of concurrent UDP sockets.
const NR_SOCKETS: usize = 8;
/// Start of the ephemeral local-port range for auto-bound sockets.
const EPHEMERAL_PORT_BASE: u16 = 32768;

/// ICMP echo request / reply types.
const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY: u8 = 0;

/// Number of RX packet buffers provided per DL_READV_S.
const RX_BUFS: usize = 4;
/// RX buffer size — fits the largest ethernet frame.
const RX_BUF_SIZE: usize = 2048;
/// Bounded DL_READV_S poll rounds per CDEV_READ (~50 ms each in virtio_net).
const READ_POLL_ROUNDS: u32 = 20;

/// IP header length without options (20 bytes).
const IP_HDR_LEN: usize = 20;
/// ICMP echo header length (type, code, checksum, id, seq).
const ICMP_HDR_LEN: usize = 8;

// ---- Grant table (client-side, mirrors fs/src/block_io.rs) ----

/// Number of grants: per DL_READV one iovec array + RX_BUFS buffers, plus a
/// TX iovec array + packet buffer.
const NR_GRANTS: usize = RX_BUFS + 4;

struct GrantTable(UnsafeCell<[CpGrant; NR_GRANTS]>);
unsafe impl Sync for GrantTable {}
impl GrantTable {
    const fn new() -> Self {
        const ENTRY: CpGrant = CpGrant {
            cp_flags: 0,
            cp_u: CpUnion {
                cp_direct: CpDirect {
                    cp_who_to: 0,
                    cp_start: 0,
                    cp_len: 0,
                    cp_reserved: [0u8; 8],
                },
            },
            cp_reserved: [0u8; 8],
        };
        Self(UnsafeCell::new([ENTRY; NR_GRANTS]))
    }

    /// Allocate a direct grant giving `callee` access to `len` bytes at
    /// `addr`. `write` selects CPF_WRITE (callee writes the buffer, used
    /// for RX) vs CPF_READ (callee reads the buffer, used for TX).
    fn grant_direct(&self, callee: i32, addr: u64, len: usize, write: bool) -> i32 {
        unsafe {
            let entries = &mut *self.0.get();
            for (i, entry) in entries.iter_mut().enumerate() {
                if entry.cp_flags == 0 {
                    let access = if write { CPF_WRITE } else { CPF_READ };
                    entry.cp_flags = CPF_USED | CPF_VALID | CPF_DIRECT | access;
                    entry.cp_u.cp_direct = CpDirect {
                        cp_who_to: callee,
                        cp_start: addr,
                        cp_len: len,
                        cp_reserved: [0u8; 8],
                    };
                    return i as i32;
                }
            }
        }
        GRANT_INVALID
    }

    fn revoke(&self, grant_id: i32) {
        if grant_id < 0 || grant_id >= NR_GRANTS as i32 {
            return;
        }
        unsafe {
            let entries = &mut *self.0.get();
            entries[grant_id as usize].cp_flags = 0;
        }
    }
}

static GRANTS: GrantTable = GrantTable::new();

// ---- Static buffers ----

/// TX staging: one ethernet/IP/ICMP packet being transmitted.
#[repr(align(16))]
struct TxBufCell(UnsafeCell<[u8; RX_BUF_SIZE]>);
unsafe impl Sync for TxBufCell {}
impl TxBufCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; RX_BUF_SIZE]))
    }
    fn get(&self) -> *mut [u8; RX_BUF_SIZE] {
        self.0.get()
    }
}
static TX_BUF: TxBufCell = TxBufCell::new();

/// RX buffers for DL_READV_S.
#[repr(align(16))]
struct RxBufsCell(UnsafeCell<[[u8; RX_BUF_SIZE]; RX_BUFS]>);
unsafe impl Sync for RxBufsCell {}
impl RxBufsCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([[0u8; RX_BUF_SIZE]; RX_BUFS]))
    }
    fn get(&self) -> *mut [[u8; RX_BUF_SIZE]; RX_BUFS] {
        self.0.get()
    }
}
static RX_BUFFERS: RxBufsCell = RxBufsCell::new();

/// iovec arrays handed to virtio_net (one per DL_READV_S / DL_WRITEV_S).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IovecGrant {
    iov_grant: i32,
    iov_size: u32,
}

#[repr(align(16))]
struct IovCell(UnsafeCell<[IovecGrant; RX_BUFS]>);
unsafe impl Sync for IovCell {}
impl IovCell {
    const fn new() -> Self {
        const IOV: IovecGrant = IovecGrant {
            iov_grant: 0,
            iov_size: 0,
        };
        Self(UnsafeCell::new([IOV; RX_BUFS]))
    }
    fn get(&self) -> *mut [IovecGrant; RX_BUFS] {
        self.0.get()
    }
}
static IOVS: IovCell = IovCell::new();

/// Reply staging: the IP datagram returned by the next CDEV_READ.
struct ReplyCell(UnsafeCell<[u8; RX_BUF_SIZE]>);
unsafe impl Sync for ReplyCell {}
impl ReplyCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; RX_BUF_SIZE]))
    }
    fn get(&self) -> *mut [u8; RX_BUF_SIZE] {
        self.0.get()
    }
}
static REPLY: ReplyCell = ReplyCell::new();

// ---- Driver state ----

#[repr(C)]
struct NetState {
    mac: [u8; 6],
    /// ARP cache: (ip, mac).
    arp_cache: [([u8; 4], [u8; 6]); 8],
    arp_cache_len: usize,
    /// Last ping request, matched against incoming replies.
    expect_ip: [u8; 4],
    expect_id: u16,
    expect_seq: u16,
    expect_set: bool,
    /// Reply length waiting for CDEV_READ (0 = none).
    reply_len: usize,
    /// Outgoing IP identification counter.
    ip_id: u16,
}

impl NetState {
    const fn new() -> Self {
        Self {
            mac: [0; 6],
            arp_cache: [([0; 4], [0; 6]); 8],
            arp_cache_len: 0,
            expect_ip: [0; 4],
            expect_id: 0,
            expect_seq: 0,
            expect_set: false,
            reply_len: 0,
            ip_id: 0x1234,
        }
    }
}

struct StateCell(UnsafeCell<NetState>);
unsafe impl Sync for StateCell {}
impl StateCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(NetState::new()))
    }
    fn get(&self) -> *mut NetState {
        self.0.get()
    }
}
static STATE: StateCell = StateCell::new();

// ---- UDP socket table ----

/// One UDP socket, keyed by its cloned minor number.
#[repr(C)]
struct UdpSock {
    in_use: bool,
    minor: i32,
    /// Current `nwio_udpopt_t` flags (NWUO_*).
    flags: u32,
    /// Bound local port (0 = not bound).
    loc_port: u16,
    /// Connected remote port (0 = any).
    rem_port: u16,
    /// Bound local address (0.0.0.0 = any).
    loc_addr: [u8; 4],
    /// Connected remote address (0.0.0.0 = any).
    rem_addr: [u8; 4],
    /// One pending received datagram payload (data-only, like NWUO_RWDATONLY).
    rx_len: usize,
    rx_buf: [u8; RX_BUF_SIZE],
}

impl UdpSock {
    const fn init(minor: i32, in_use: bool) -> Self {
        Self {
            in_use,
            minor,
            flags: 0,
            loc_port: 0,
            rem_port: 0,
            loc_addr: [0; 4],
            rem_addr: [0; 4],
            rx_len: 0,
            rx_buf: [0; RX_BUF_SIZE],
        }
    }
}

struct SocketTableCell(UnsafeCell<[UdpSock; NR_SOCKETS]>);
unsafe impl Sync for SocketTableCell {}
impl SocketTableCell {
    const fn new() -> Self {
        const EMPTY: UdpSock = UdpSock::init(0, false);
        Self(UnsafeCell::new([EMPTY; NR_SOCKETS]))
    }
    fn get(&self) -> *mut [UdpSock; NR_SOCKETS] {
        self.0.get()
    }
}
static SOCKETS: SocketTableCell = SocketTableCell::new();

/// Find the live UDP socket for a cloned minor number.
fn socket_for_minor(minor: i32) -> Option<&'static mut UdpSock> {
    unsafe {
        let socks = &mut *SOCKETS.get();
        socks.iter_mut().find(|s| s.in_use && s.minor == minor)
    }
}

// ---- DL protocol helpers ----

/// Build a DL request message and SENDREC it to virtio_net. Returns the
/// sender endpoint (>= 0) or a negative error.
fn dl_sendrec(msg: &mut Message, mtype: u32) -> i32 {
    msg.m_source = 0;
    msg.m_type = mtype as i32;
    unsafe {
        minix_rt::syscall2(
            minix_rt::SENDREC_CALL,
            VIRTIO_NET_PROC_NR as u64,
            msg as *mut Message as u64,
        ) as i32
    }
}

/// Fetch the MAC from virtio_net via DL_CONF. Returns true on success.
fn dl_conf() -> bool {
    let mut msg = Message {
        m_source: 0,
        m_type: DL_CONF as i32,
        m_payload: unsafe { core::mem::zeroed() },
    };
    // mode @ payload 0
    unsafe {
        msg.m_payload.raw[0..4].copy_from_slice(&DL_NOMODE.to_ne_bytes());
    }
    let r = dl_sendrec(&mut msg, DL_CONF);
    if r < 0 || msg.m_type != DL_CONF_REPLY as i32 {
        return false;
    }
    // stat @ payload 0, mac @ payload 4
    let stat = unsafe { i32::from_ne_bytes(msg.m_payload.raw[0..4].try_into().unwrap_or([0; 4])) };
    if stat != 0 {
        return false;
    }
    unsafe {
        (*STATE.get())
            .mac
            .copy_from_slice(&msg.m_payload.raw[4..10]);
    }
    true
}

/// Transmit one full ethernet frame via DL_WRITEV_S. Returns 0 on success.
fn dl_write_frame(packet: &[u8]) -> i32 {
    let st = unsafe { &*STATE.get() };
    // iovec array (read by virtio_net) + the packet buffer (read by
    // virtio_net). The driver reads `count * sizeof(IovecGrant)` bytes
    // from the array, so grant the whole array.
    let iov_grant = GRANTS.grant_direct(
        VIRTIO_NET_PROC_NR,
        IOVS.get() as u64,
        RX_BUFS * core::mem::size_of::<IovecGrant>(),
        false,
    );
    if iov_grant == GRANT_INVALID {
        return -1;
    }
    let pkt_grant = GRANTS.grant_direct(
        VIRTIO_NET_PROC_NR,
        packet.as_ptr() as u64,
        packet.len(),
        false,
    );
    if pkt_grant == GRANT_INVALID {
        GRANTS.revoke(iov_grant);
        return -1;
    }
    unsafe {
        let iovs = &mut *IOVS.get();
        iovs[0] = IovecGrant {
            iov_grant: pkt_grant,
            iov_size: packet.len() as u32,
        };
    }

    let mut msg = Message {
        m_source: 0,
        m_type: DL_WRITEV_S as i32,
        m_payload: unsafe { core::mem::zeroed() },
    };
    unsafe {
        msg.m_payload.raw[0..4].copy_from_slice(&iov_grant.to_ne_bytes());
        msg.m_payload.raw[4..8].copy_from_slice(&1i32.to_ne_bytes());
    }
    let r = dl_sendrec(&mut msg, DL_WRITEV_S);
    GRANTS.revoke(pkt_grant);
    GRANTS.revoke(iov_grant);
    let _ = st;
    if r < 0 {
        return r;
    }
    if msg.m_type != DL_TASK_REPLY as i32 {
        return -5;
    }
    0
}

/// Receive one ethernet frame via DL_READV_S. Returns the number of
/// bytes copied into the RX buffer, or 0 if the driver had no frame
/// within its poll window.
fn dl_read_frames() -> usize {
    // iovec array (read by virtio_net) + one RX buffer (written by
    // virtio_net). The driver serves one packet per request and replies
    // with the number of bytes copied.
    let iov_grant = GRANTS.grant_direct(
        VIRTIO_NET_PROC_NR,
        IOVS.get() as u64,
        RX_BUFS * core::mem::size_of::<IovecGrant>(),
        false,
    );
    if iov_grant == GRANT_INVALID {
        return 0;
    }
    let buf_grant = GRANTS.grant_direct(
        VIRTIO_NET_PROC_NR,
        RX_BUFFERS.get() as u64,
        RX_BUF_SIZE,
        true, // virtio_net writes the packet into our buffer
    );
    if buf_grant == GRANT_INVALID {
        GRANTS.revoke(iov_grant);
        return 0;
    }
    unsafe {
        (*IOVS.get())[0] = IovecGrant {
            iov_grant: buf_grant,
            iov_size: RX_BUF_SIZE as u32,
        };
    }

    let mut msg = Message {
        m_source: 0,
        m_type: DL_READV_S as i32,
        m_payload: unsafe { core::mem::zeroed() },
    };
    unsafe {
        msg.m_payload.raw[0..4].copy_from_slice(&iov_grant.to_ne_bytes());
        msg.m_payload.raw[4..8].copy_from_slice(&1i32.to_ne_bytes());
    }
    let r = dl_sendrec(&mut msg, DL_READV_S);
    GRANTS.revoke(buf_grant);
    GRANTS.revoke(iov_grant);
    if r < 0 || msg.m_type != DL_TASK_REPLY as i32 {
        return 0;
    }
    // Reply: bytes copied @ payload 0.
    unsafe { i32::from_ne_bytes(msg.m_payload.raw[0..4].try_into().unwrap_or([0; 4])) as usize }
}

// ---- Ethernet / IP / ICMP packet helpers ----

fn read_be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn write_be16(b: &mut [u8], v: u16) {
    b[..2].copy_from_slice(&v.to_be_bytes());
}

/// Internet checksum over `data` (RFC 1071, one's complement).
fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    csum_add(&mut sum, data);
    csum_done(sum)
}

/// Accumulate a big-endian 16-bit one's-complement sum over `data`.
/// Each call must start on a 16-bit boundary of the overall stream.
fn csum_add(sum: &mut u32, data: &[u8]) {
    let mut i = 0;
    while i + 1 < data.len() {
        *sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        *sum += (data[i] as u32) << 8;
    }
}

/// Fold a running sum into the final one's-complement value.
fn csum_done(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn ip_addr_eq(a: &[u8; 4], b: &[u8]) -> bool {
    a == b
}

/// Demux an inbound UDP datagram to the socket bound to its destination
/// port (honoring the socket's remote address/port filter). The payload is
/// stored data-only in the socket's RX slot for the next CDEV_READ.
fn udp_demux(pkt: &[u8], ihl: usize) {
    let udp = &pkt[ihl..];
    if udp.len() < UDP_HDR_LEN {
        return;
    }
    let src_port = read_be16(&udp[0..2]);
    let dst_port = read_be16(&udp[2..4]);
    let udp_len = read_be16(&udp[4..6]) as usize;
    if udp_len < UDP_HDR_LEN || udp_len > udp.len() {
        return;
    }
    let src_ip: [u8; 4] = pkt[12..16].try_into().unwrap_or([0; 4]);
    let payload = &udp[UDP_HDR_LEN..udp_len];
    unsafe {
        let socks = &mut *SOCKETS.get();
        for s in socks.iter_mut() {
            if !s.in_use || s.rx_len > 0 {
                continue;
            }
            if s.loc_port != dst_port {
                continue;
            }
            if s.rem_port != 0 && s.rem_port != src_port {
                continue;
            }
            if s.rem_addr != [0; 4] && s.rem_addr != src_ip {
                continue;
            }
            let n = payload.len().min(RX_BUF_SIZE);
            s.rx_buf[..n].copy_from_slice(&payload[..n]);
            s.rx_len = n;
            return;
        }
    }
}

/// Convert a network-order u32 (as stored in `nwio_udpopt_t`) to an IP.
fn u32_to_ip(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// Convert an IP to the network-order u32 used by `nwio_udpopt_t`.
fn ip_to_u32(ip: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*ip)
}

/// Next IP identification value for outgoing datagrams.
fn next_ip_id() -> u16 {
    let st = unsafe { &mut *STATE.get() };
    st.ip_id = st.ip_id.wrapping_add(1);
    if st.ip_id == 0 {
        st.ip_id = 1;
    }
    st.ip_id
}

/// Build an IP/UDP datagram into `out` (IP header + UDP header + payload
/// already present after byte 28). Returns nothing; `out` must be at least
/// `IP_HDR_LEN + UDP_HDR_LEN + payload_len` bytes.
fn build_udp_datagram(out: &mut [u8], s: &UdpSock, payload_len: usize) {
    let total = IP_HDR_LEN + UDP_HDR_LEN + payload_len;
    let src_ip = if s.loc_addr == [0; 4] {
        OUR_IP
    } else {
        s.loc_addr
    };

    // IP header.
    out[0] = 0x45; // version 4, IHL 5
    out[1] = 0;
    write_be16(&mut out[2..4], total as u16);
    write_be16(&mut out[4..6], next_ip_id());
    write_be16(&mut out[6..8], 0); // frag
    out[8] = 64; // TTL
    out[9] = IP_PROTO_UDP;
    write_be16(&mut out[10..12], 0); // checksum (filled below)
    out[12..16].copy_from_slice(&src_ip);
    out[16..20].copy_from_slice(&s.rem_addr);
    let ip_csum = checksum(&out[..IP_HDR_LEN]);
    write_be16(&mut out[10..12], ip_csum);

    // UDP header (checksum field starts zero, filled below).
    let u = IP_HDR_LEN;
    write_be16(&mut out[u..u + 2], s.loc_port);
    write_be16(&mut out[u + 2..u + 4], s.rem_port);
    write_be16(&mut out[u + 4..u + 6], (UDP_HDR_LEN + payload_len) as u16);
    write_be16(&mut out[u + 6..u + 8], 0);

    // UDP checksum over the pseudo-header + UDP header + payload.
    let mut sum = 0u32;
    csum_add(&mut sum, &src_ip);
    csum_add(&mut sum, &s.rem_addr);
    csum_add(&mut sum, &[0, IP_PROTO_UDP]); // zero byte + protocol
    csum_add(
        &mut sum,
        &(UDP_HDR_LEN as u16 + payload_len as u16).to_be_bytes(),
    );
    csum_add(&mut sum, &out[u..u + UDP_HDR_LEN]);
    csum_add(&mut sum, &out[u + UDP_HDR_LEN..total]);
    let udp_csum = csum_done(sum);
    write_be16(&mut out[u + 6..u + 8], udp_csum);
}

/// Build an ethernet frame for `payload` (ARP or IP) into `out`.
/// Returns the frame length.
fn build_eth_frame(out: &mut [u8], dst_mac: &[u8; 6], ethertype: u16, payload: &[u8]) -> usize {
    let st = unsafe { &*STATE.get() };
    out[..6].copy_from_slice(dst_mac);
    out[6..12].copy_from_slice(&st.mac);
    write_be16(&mut out[12..14], ethertype);
    out[14..14 + payload.len()].copy_from_slice(payload);
    14 + payload.len()
}

/// Build an ICMP echo request IP datagram for `dst_ip` into `out`.
/// Returns the datagram length.
fn build_echo_request(out: &mut [u8], dst_ip: &[u8; 4], id: u16, seq: u16) -> usize {
    // IP header (20 bytes, no options).
    out[0] = 0x45; // version 4, IHL 5
    out[1] = 0; // DSCP/ECN
    let total_len = (IP_HDR_LEN + ICMP_HDR_LEN) as u16;
    write_be16(&mut out[2..4], total_len);
    write_be16(&mut out[4..6], 0x1234); // id
    write_be16(&mut out[6..8], 0); // frag
    out[8] = 64; // TTL
    out[9] = IP_PROTO_ICMP;
    write_be16(&mut out[10..12], 0); // header checksum (filled below)
    out[12..16].copy_from_slice(&OUR_IP);
    out[16..20].copy_from_slice(dst_ip);
    let ip_csum = checksum(&out[..IP_HDR_LEN]);
    write_be16(&mut out[10..12], ip_csum);

    // ICMP echo request.
    out[IP_HDR_LEN] = ICMP_ECHO_REQUEST;
    out[IP_HDR_LEN + 1] = 0; // code
    write_be16(&mut out[IP_HDR_LEN + 2..IP_HDR_LEN + 4], 0); // checksum
    write_be16(&mut out[IP_HDR_LEN + 4..IP_HDR_LEN + 6], id);
    write_be16(&mut out[IP_HDR_LEN + 6..IP_HDR_LEN + 8], seq);
    let icmp_csum = checksum(&out[IP_HDR_LEN..IP_HDR_LEN + ICMP_HDR_LEN]);
    write_be16(&mut out[IP_HDR_LEN + 2..IP_HDR_LEN + 4], icmp_csum);

    IP_HDR_LEN + ICMP_HDR_LEN
}

/// Parse a received ethernet frame and act on it. Returns true if a reply
/// for the last ping was captured into REPLY.
fn handle_frame(frame: &[u8]) -> bool {
    if frame.len() < 14 {
        return false;
    }
    let st = unsafe { &mut *STATE.get() };
    let ethertype = read_be16(&frame[12..14]);
    let payload = &frame[14..];
    match ethertype {
        ETH_TYPE_ARP => handle_arp(st, payload),
        ETH_TYPE_IP => handle_ip(st, payload),
        _ => false,
    }
}

fn handle_arp(st: &mut NetState, pkt: &[u8]) -> bool {
    if pkt.len() < 28 {
        return false;
    }
    // htype(2) ptype(2) hlen(1) plen(1) op(2) sha(6) spa(4) tha(6) tpa(4)
    let op = read_be16(&pkt[6..8]);
    let sha: [u8; 6] = pkt[8..14].try_into().unwrap_or([0; 6]);
    let spa: [u8; 4] = pkt[14..18].try_into().unwrap_or([0; 4]);
    let tpa: [u8; 4] = pkt[24..28].try_into().unwrap_or([0; 4]);
    if !ip_addr_eq(&tpa, &OUR_IP) {
        return false;
    }
    if op == 1 {
        // ARP request for us — reply with our MAC.
        let tx = unsafe { &mut *TX_BUF.get() };
        let mut arp = [0u8; 28];
        write_be16(&mut arp[0..2], 1); // htype
        write_be16(&mut arp[2..4], 0x0800); // ptype
        arp[4] = 6;
        arp[5] = 4;
        write_be16(&mut arp[6..8], 2); // op: reply
        arp[8..14].copy_from_slice(&st.mac); // sha
        arp[14..18].copy_from_slice(&OUR_IP); // spa
        arp[18..24].copy_from_slice(&sha); // tha
        arp[24..28].copy_from_slice(&spa); // tpa
        let n = build_eth_frame(tx, &sha, ETH_TYPE_ARP, &arp);
        let _ = dl_write_frame(&tx[..n]);
    } else if op == 2 {
        // ARP reply — cache the sender.
        cache_arp(st, spa, sha);
    }
    false
}

fn cache_arp(st: &mut NetState, ip: [u8; 4], mac: [u8; 6]) {
    for i in 0..st.arp_cache_len {
        if ip_addr_eq(&st.arp_cache[i].0, &ip) {
            st.arp_cache[i].1 = mac;
            return;
        }
    }
    if st.arp_cache_len < st.arp_cache.len() {
        st.arp_cache[st.arp_cache_len] = (ip, mac);
        st.arp_cache_len += 1;
    }
}

fn lookup_arp(st: &NetState, ip: &[u8; 4]) -> Option<[u8; 6]> {
    for i in 0..st.arp_cache_len {
        if ip_addr_eq(&st.arp_cache[i].0, ip) {
            return Some(st.arp_cache[i].1);
        }
    }
    None
}

/// Send an ARP request for `ip` and poll for the reply.
fn arp_resolve(ip: &[u8; 4]) -> Option<[u8; 6]> {
    let st = unsafe { &mut *STATE.get() };
    if let Some(mac) = lookup_arp(st, ip) {
        return Some(mac);
    }
    if st.mac == [0; 6] {
        return None; // no NIC
    }

    // Broadcast ARP request.
    let mut arp = [0u8; 28];
    write_be16(&mut arp[0..2], 1);
    write_be16(&mut arp[2..4], 0x0800);
    arp[4] = 6;
    arp[5] = 4;
    write_be16(&mut arp[6..8], 1); // op: request
    arp[8..14].copy_from_slice(&st.mac);
    arp[14..18].copy_from_slice(&OUR_IP);
    arp[24..28].copy_from_slice(ip);
    let broadcast = [0xFF; 6];
    let tx = unsafe { &mut *TX_BUF.get() };
    let n = build_eth_frame(tx, &broadcast, ETH_TYPE_ARP, &arp);
    if dl_write_frame(&tx[..n]) != 0 {
        return None;
    }

    // Poll for the ARP reply.
    for _ in 0..READ_POLL_ROUNDS {
        let got = dl_read_frames();
        if got == 0 {
            continue;
        }
        unsafe {
            let bufs = &*RX_BUFFERS.get();
            handle_frame(&bufs[0][..got]);
        }
        if let Some(mac) = lookup_arp(st, ip) {
            return Some(mac);
        }
    }
    None
}

/// Handle a received IP datagram. Auto-replies to echo requests; captures
/// a matching echo reply for the last ping.
fn handle_ip(st: &mut NetState, pkt: &[u8]) -> bool {
    if pkt.len() < IP_HDR_LEN {
        return false;
    }
    let ihl = (pkt[0] & 0x0F) as usize * 4;
    if pkt.len() < ihl + ICMP_HDR_LEN {
        return false;
    }
    if pkt[9] == IP_PROTO_UDP {
        udp_demux(pkt, ihl);
        return false;
    }
    if pkt[9] != IP_PROTO_ICMP {
        return false;
    }
    // dst must be us (or broadcast).
    let dst_ip: [u8; 4] = pkt[16..20].try_into().unwrap_or([0; 4]);
    if !ip_addr_eq(&dst_ip, &OUR_IP) {
        return false;
    }
    let icmp = &pkt[ihl..];
    let icmp_type = icmp[0];
    let id = read_be16(&icmp[4..6]);
    let seq = read_be16(&icmp[6..8]);
    let total = ihl + icmp.len();

    if icmp_type == ICMP_ECHO_REQUEST {
        // Reply automatically: swap src/dst, set type to reply.
        let tx = unsafe { &mut *TX_BUF.get() };
        // Reuse the received IP header, swapping addresses.
        tx[..total].copy_from_slice(&pkt[..total]);
        tx[8] = 64; // TTL
        tx[12..16].copy_from_slice(&dst_ip); // src = original dst (us)
        let src_ip: [u8; 4] = pkt[12..16].try_into().unwrap_or([0; 4]);
        tx[16..20].copy_from_slice(&src_ip); // dst = original src
        tx[ihl] = ICMP_ECHO_REPLY;
        // Recompute checksums.
        let ip_csum = checksum(&tx[..ihl]);
        write_be16(&mut tx[10..12], ip_csum);
        let icmp_csum = checksum(&tx[ihl..total]);
        write_be16(&mut tx[ihl + 2..ihl + 4], icmp_csum);
        // Frame and send to the original source MAC (resolve via ARP).
        if let Some(src_mac) = arp_resolve(&src_ip) {
            let mut frame = [0u8; RX_BUF_SIZE];
            let n = build_eth_frame(&mut frame, &src_mac, ETH_TYPE_IP, &tx[..total]);
            let _ = dl_write_frame(&frame[..n]);
        }
        return false;
    }

    if icmp_type == ICMP_ECHO_REPLY && st.expect_set {
        let matched =
            ip_addr_eq(&st.expect_ip, &pkt[12..16]) && id == st.expect_id && seq == st.expect_seq;
        if matched {
            unsafe {
                let reply = &mut *REPLY.get();
                reply[..total].copy_from_slice(&pkt[..total]);
                (*STATE.get()).reply_len = total;
            }
            return true;
        }
    }
    false
}

// ---- CDEV handlers ----

/// CDEV_WRITE: 8-byte `{dst_ip[4] id[2] seq[2]}` → send an ICMP echo.
/// Returns the bytes accepted (8) or a negative error.
fn cdev_write(msg: &Message) -> i32 {
    // m2_l3 @ payload 32 carries the inline data (VFS writes it at
    // absolute message byte 40, which is m_payload.raw[32]; raw[40..]
    // would read 8 bytes past the data).
    let data = unsafe { &msg.m_payload.raw[32..40] };
    let dst_ip: [u8; 4] = data[0..4].try_into().unwrap_or([0; 4]);
    let id = read_be16(&data[4..6]);
    let seq = read_be16(&data[6..8]);

    let mac = match arp_resolve(&dst_ip) {
        Some(m) => m,
        None => return -5, // EIO: no route / no NIC
    };

    // Build the IP datagram into a local, then frame it into TX_BUF (the
    // eth frame and its payload cannot alias the same buffer).
    let mut ip_pkt = [0u8; RX_BUF_SIZE];
    let ip_len = build_echo_request(&mut ip_pkt, &dst_ip, id, seq);
    let tx = unsafe { &mut *TX_BUF.get() };
    let n = build_eth_frame(tx, &mac, ETH_TYPE_IP, &ip_pkt[..ip_len]);
    if dl_write_frame(&tx[..n]) != 0 {
        return -5;
    }

    let st = unsafe { &mut *STATE.get() };
    st.expect_ip = dst_ip;
    st.expect_id = id;
    st.expect_seq = seq;
    st.expect_set = true;
    st.reply_len = 0;
    8
}

/// CDEV_READ: return the next matching echo reply as an IP datagram.
/// Polls the NIC for a bounded time; returns the reply length or 0.
fn cdev_read(msg: &mut Message) -> i32 {
    let mut captured = false;
    for _ in 0..READ_POLL_ROUNDS {
        unsafe {
            if (*STATE.get()).reply_len > 0 {
                captured = true;
                break;
            }
        }
        let got = dl_read_frames();
        if got == 0 {
            continue;
        }
        unsafe {
            let bufs = &*RX_BUFFERS.get();
            if handle_frame(&bufs[0][..got]) {
                captured = true;
            }
        }
        if captured {
            break;
        }
    }
    unsafe {
        let len = (*STATE.get()).reply_len;
        if len == 0 || !captured {
            return 0;
        }
        (*STATE.get()).reply_len = 0;
        // Copy the reply datagram inline at payload 0, where VFS's
        // CDEV_READ handler reads it back to the user.
        let reply = &*REPLY.get();
        msg.m_payload.raw[..len].copy_from_slice(&reply[..len]);
        len as i32
    }
}

/// CDEV message field readers (payload-relative offsets; the payload
/// starts at absolute message byte 8). Minor @ 0, flags/request @ 4, user
/// endpoint @ 8, VA @ 16, byte count @ 24 — see `vfs/device.rs`.
fn msg_i32(msg: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        unsafe { &msg.m_payload.raw[off..][..4] }
            .try_into()
            .unwrap_or([0; 4]),
    )
}
fn msg_u32(msg: &Message, off: usize) -> u32 {
    u32::from_ne_bytes(
        unsafe { &msg.m_payload.raw[off..][..4] }
            .try_into()
            .unwrap_or([0; 4]),
    )
}
fn msg_u64(msg: &Message, off: usize) -> u64 {
    u64::from_ne_bytes(
        unsafe { &msg.m_payload.raw[off..][..8] }
            .try_into()
            .unwrap_or([0; 8]),
    )
}

/// Handle a CDEV request and write the reply into `msg`.
fn handle_cdev_request(msg: &mut Message, call_type: u32) -> i32 {
    let minor = msg_i32(msg, 0);
    match call_type {
        CDEV_OPEN => cdev_open_net(msg),
        CDEV_CLOSE => cdev_close_net(minor),
        CDEV_READ => {
            if msg_u32(msg, 4) & CDEV_DGRAM != 0 {
                cdev_read_dgram(msg)
            } else {
                cdev_read(msg)
            }
        }
        CDEV_WRITE => {
            if msg_u32(msg, 4) & CDEV_DGRAM != 0 {
                cdev_write_dgram(msg)
            } else {
                cdev_write(msg)
            }
        }
        CDEV_IOCTL => cdev_ioctl_net(msg),
        CDEV_SELECT => 0,
        _ => -22, // EINVAL
    }
}

// ---- UDP socket CDEV handlers ----

/// CDEV_OPEN: minor 0 (/dev/ip) is a plain open; minor 1 (/dev/udp)
/// allocates a socket slot and replies with a cloned minor, flagged as a
/// datagram channel.
fn cdev_open_net(msg: &mut Message) -> i32 {
    let minor = msg_i32(msg, 0);
    match minor {
        IP_DEV_MINOR => 0,
        UDP_DEV_MINOR => unsafe {
            let socks = &mut *SOCKETS.get();
            for (i, slot) in socks.iter_mut().enumerate() {
                if !slot.in_use {
                    let clone_minor = SOCKET_MINOR_BASE + i as i32;
                    *slot = UdpSock::init(clone_minor, true);
                    return (CDEV_CLONED | CDEV_DGRAM_OPEN | clone_minor as u32) as i32;
                }
            }
            -24 // EMFILE
        },
        _ => -19, // ENODEV
    }
}

/// CDEV_CLOSE: free the socket slot for a cloned minor.
fn cdev_close_net(minor: i32) -> i32 {
    if minor == IP_DEV_MINOR {
        return 0;
    }
    match socket_for_minor(minor) {
        Some(s) => {
            s.in_use = false;
            0
        }
        None => -9, // EBADF
    }
}

/// CDEV_IOCTL: NWIOSUDPOPT / NWIOGUDPOPT. The `nwio_udpopt_t` struct
/// travels at payload bytes 16..32 (VFS's m2_l1 data area).
fn cdev_ioctl_net(msg: &mut Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let request = msg_u32(msg, 4);
    let s = match socket_for_minor(minor) {
        Some(s) => s,
        None => return -9, // EBADF
    };
    match request {
        NWIOSUDPOPT => {
            let opt = NwioUdpOpt::read_from(unsafe { &msg.m_payload.raw[16..32] });
            udp_setopt(s, &opt)
        }
        NWIOGUDPOPT => {
            udp_getopt(s).write_to(unsafe { &mut msg.m_payload.raw[16..32] });
            0
        }
        _ => -25, // ENOTTY
    }
}

/// Apply a NWIOSUDPOPT option struct to a socket (bind/connect).
/// Flag groups update only the fields whose mask bits are set, so a
/// connect() after bind() only touches the remote address/port.
fn udp_setopt(s: &mut UdpSock, opt: &NwioUdpOpt) -> i32 {
    let f = opt.nwuo_flags;
    if f & NWUO_LP_SEL != 0 {
        // Auto local port: unique per socket slot.
        s.loc_port = EPHEMERAL_PORT_BASE + (s.minor - SOCKET_MINOR_BASE) as u16;
    } else if f & NWUO_LP_SET != 0 {
        s.loc_port = opt.nwuo_locport;
    }
    if f & NWUO_EN_LOC != 0 {
        s.loc_addr = u32_to_ip(opt.nwuo_locaddr);
    }
    if f & NWUO_RP_SET != 0 {
        s.rem_port = opt.nwuo_remport;
    } else if f & NWUO_RP_ANY != 0 {
        s.rem_port = 0;
    }
    if f & NWUO_RA_SET != 0 {
        s.rem_addr = u32_to_ip(opt.nwuo_remaddr);
    } else if f & NWUO_RA_ANY != 0 {
        s.rem_addr = [0; 4];
    }
    if f & NWUO_RWDATALL != 0 {
        return -95; // EOPNOTSUPP: header-inclusive datagrams are not supported
    }
    s.flags = f;
    0
}

/// Build the current options struct for NWIOGUDPOPT.
fn udp_getopt(s: &UdpSock) -> NwioUdpOpt {
    NwioUdpOpt {
        nwuo_flags: s.flags,
        nwuo_locport: s.loc_port,
        nwuo_remport: s.rem_port,
        nwuo_locaddr: ip_to_u32(&s.loc_addr),
        nwuo_remaddr: ip_to_u32(&s.rem_addr),
    }
}

/// CDEV_WRITE (datagram): copy the user's payload, frame it as
/// IP/UDP/Ethernet and transmit. Requires a bound, connected socket.
fn cdev_write_dgram(msg: &Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let user = msg_i32(msg, 8);
    let va = msg_u64(msg, 16);
    let len = msg_u64(msg, 24);
    let s = match socket_for_minor(minor) {
        Some(s) => s,
        None => return -9, // EBADF
    };
    if s.rem_addr == [0; 4] || s.rem_port == 0 || s.loc_port == 0 {
        return -107; // ENOTCONN
    }
    let max_payload = (RX_BUF_SIZE - IP_HDR_LEN - UDP_HDR_LEN) as u64;
    if len == 0 || len > max_payload {
        return -90; // EMSGSIZE
    }
    let total = IP_HDR_LEN + UDP_HDR_LEN + len as usize;

    let mut ip_pkt = [0u8; RX_BUF_SIZE];
    let copy_r = minix_rt::sys_vircopy(
        user,
        va,
        minix_rt::SELF,
        ip_pkt.as_mut_ptr() as u64 + (IP_HDR_LEN + UDP_HDR_LEN) as u64,
        len as usize,
    );
    if copy_r != 0 {
        return copy_r;
    }

    let mac = match arp_resolve(&s.rem_addr) {
        Some(m) => m,
        None => return -5, // EIO: ARP resolution failed / no NIC
    };
    build_udp_datagram(&mut ip_pkt[..total], s, len as usize);
    let tx = unsafe { &mut *TX_BUF.get() };
    let n = build_eth_frame(tx, &mac, ETH_TYPE_IP, &ip_pkt[..total]);
    if dl_write_frame(&tx[..n]) != 0 {
        return -5;
    }
    len as i32
}

/// CDEV_READ (datagram): poll the NIC until a datagram for this socket
/// arrives, then vircopy the payload into the user's buffer.
fn cdev_read_dgram(msg: &mut Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let user = msg_i32(msg, 8);
    let va = msg_u64(msg, 16);
    let count = msg_u64(msg, 24);
    if count == 0 {
        return 0;
    }

    for _ in 0..READ_POLL_ROUNDS {
        let (ready, n) = {
            let s = match socket_for_minor(minor) {
                Some(s) => s,
                None => return -9, // EBADF
            };
            (s.rx_len > 0, s.rx_len.min(count as usize))
        };
        if ready {
            let s = match socket_for_minor(minor) {
                Some(s) => s,
                None => return -9,
            };
            let r = minix_rt::sys_vircopy(minix_rt::SELF, s.rx_buf.as_ptr() as u64, user, va, n);
            s.rx_len = 0;
            if r != 0 {
                return r;
            }
            return n as i32;
        }
        let got = dl_read_frames();
        if got == 0 {
            continue;
        }
        unsafe {
            let bufs = &*RX_BUFFERS.get();
            handle_frame(&bufs[0][..got]);
        }
    }
    // Final check after the poll window (a datagram may have arrived in
    // the last round without a read waking).
    let s = match socket_for_minor(minor) {
        Some(s) => s,
        None => return -9,
    };
    if s.rx_len > 0 {
        let n = s.rx_len.min(count as usize);
        let r = minix_rt::sys_vircopy(minix_rt::SELF, s.rx_buf.as_ptr() as u64, user, va, n);
        s.rx_len = 0;
        if r != 0 {
            return r;
        }
        return n as i32;
    }
    0
}

/// Register the grant table with the kernel (SYS_SETGRANT).
fn register_grants() {
    let mut msg = [0u8; 64];
    msg[8..16].copy_from_slice(&(GRANTS.0.get() as u64).to_le_bytes());
    msg[16..20].copy_from_slice(&(NR_GRANTS as i32).to_le_bytes());
    let _ = minix_rt::kernel_call(34, &mut msg); // SYS_SETGRANT
}

/// Main entry point for the net server process.
pub fn net_server_main() {
    #[cfg(target_os = "none")]
    {
        const ANY: i32 = 0x0000ffff;

        register_grants();
        let _ = dl_conf();

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };

            let src = unsafe {
                minix_rt::syscall2(
                    minix_rt::RECEIVE_CALL,
                    ANY as u64,
                    &mut msg as *mut Message as u64,
                )
            };
            if src < 0 {
                continue;
            }
            let src_ep = src as i32;
            let call_type = msg.m_type as u32;

            if arch_common::com::is_cdev_rq(call_type) {
                let result = handle_cdev_request(&mut msg, call_type);
                msg.m_type = result;
                unsafe {
                    minix_rt::syscall2(
                        minix_rt::SEND_CALL,
                        src_ep as u64,
                        &mut msg as *mut Message as u64,
                    );
                }
            }
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        // No-op on host builds — dispatch is tested directly.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Probe anchors for the QEMU-monitor dumps of the net server's
    /// statics (see tools/udp_netstate_probe.py). Keep in sync with the
    /// struct definitions.
    #[test]
    fn probe_offsets() {
        assert_eq!(core::mem::offset_of!(NetState, mac), 0);
        assert_eq!(core::mem::offset_of!(NetState, arp_cache), 6);
        assert_eq!(core::mem::offset_of!(NetState, arp_cache_len), 88);
        assert_eq!(core::mem::offset_of!(NetState, ip_id), 120);

        assert_eq!(core::mem::offset_of!(UdpSock, in_use), 0);
        assert_eq!(core::mem::offset_of!(UdpSock, minor), 4);
        assert_eq!(core::mem::offset_of!(UdpSock, flags), 8);
        assert_eq!(core::mem::offset_of!(UdpSock, loc_port), 12);
        assert_eq!(core::mem::offset_of!(UdpSock, rem_port), 14);
        assert_eq!(core::mem::offset_of!(UdpSock, loc_addr), 16);
        assert_eq!(core::mem::offset_of!(UdpSock, rem_addr), 20);
        assert_eq!(core::mem::offset_of!(UdpSock, rx_len), 24);
        assert_eq!(core::mem::size_of::<UdpSock>(), 2080);
    }
}
