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
    NWIOGTCPCONF, NWIOGTCPCOOKIE, NWIOGUDPOPT, NWIOSTCPCONF, NWIOSUDPOPT, NWIOTCPACCEPTTO,
    NWIOTCPCONN, NWIOTCPLISTENQ, NWTC_LOCPORT_MASK, NWTC_LP_SEL, NWTC_LP_SET, NWTC_SET_RA,
    NWTC_SET_RP, NWTC_UNSET_RA, NWTC_UNSET_RP, NWUO_EN_LOC, NWUO_LP_SEL, NWUO_LP_SET, NWUO_RA_ANY,
    NWUO_RA_SET, NWUO_RP_ANY, NWUO_RP_SET, NWUO_RWDATALL, NwioTcpCl, NwioTcpConf, NwioUdpOpt,
    TcpCookie,
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
/// IP protocol number for TCP.
const IP_PROTO_TCP: u8 = 6;

/// UDP header length (src port, dst port, length, checksum).
const UDP_HDR_LEN: usize = 8;
/// TCP header length without options.
const TCP_HDR_LEN: usize = 20;

/// Static (non-cloned) minor for /dev/ip.
const IP_DEV_MINOR: i32 = 0;
/// Static minor for /dev/udp — each open clones to a fresh socket minor.
const UDP_DEV_MINOR: i32 = 1;
/// Static minor for /dev/tcp — each open clones to a fresh socket minor.
const TCP_DEV_MINOR: i32 = 2;

/// First clone minor handed out for UDP sockets.
const SOCKET_MINOR_BASE: i32 = 0x10;
/// First clone minor handed out for TCP sockets.
const TCP_SOCKET_MINOR_BASE: i32 = 0x20;
/// Number of concurrent UDP sockets.
const NR_SOCKETS: usize = 8;
/// Number of concurrent TCP sockets.
const NR_TCP_SOCKETS: usize = 8;
/// Start of the ephemeral local-port range for auto-bound sockets.
const EPHEMERAL_PORT_BASE: u16 = 32768;

// TCP header flag bits (RFC 793).
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

// Errnos used by the TCP paths (positive magnitudes, negative returns).
const ENOTCONN: i32 = -107;
const EISCONN: i32 = -106;
const EAGAIN: i32 = -11;
const ETIMEDOUT: i32 = -110;
const ECONNREFUSED: i32 = -111;
const EINPROGRESS: i32 = -115;

/// Max pending connections per listening socket (backlog ceiling).
const ACCEPT_QUEUE_MAX: usize = 4;
/// Poll-round groups before unacked data is retransmitted (~1 group per
/// `dl_read_frames` round; on the lossless QEMU path the ACK always lands
/// within one, so this never fires spuriously).
const RETX_AFTER_ROUNDS: u32 = 3;

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

// ---- TCP socket table ----

/// TCP connection state (a minimal subset of RFC 793).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum TcpState {
    /// No connection (fresh socket or after RST/timeout).
    Closed,
    /// SYN sent, waiting for the SYN-ACK.
    SynSent,
    /// Connection established, data flowing.
    Established,
    /// `listen()` was called; inbound SYNs join the accept queue.
    Listening,
}

/// One inbound connection waiting on a listening socket: the handshake ran
/// to completion (or is waiting for its final ACK) but `accept()` has not
/// yet moved it to a fresh socket. It carries its own sequence state so the
/// listener's backlog can buffer data sent right after connect.
#[repr(C)]
#[derive(Clone, Copy)]
struct PendingConn {
    in_use: bool,
    /// Handshake complete (`ACK == snd_nxt` seen) — ready for accept.
    established: bool,
    rem_addr: [u8; 4],
    rem_port: u16,
    rem_mac: [u8; 6],
    iss: u32,
    snd_nxt: u32,
    rcv_nxt: u32,
    rx_len: usize,
    rx_buf: [u8; RX_BUF_SIZE],
}

impl PendingConn {
    const fn init() -> Self {
        Self {
            in_use: false,
            established: false,
            rem_addr: [0; 4],
            rem_port: 0,
            rem_mac: [0; 6],
            iss: 0,
            snd_nxt: 0,
            rcv_nxt: 0,
            rx_len: 0,
            rx_buf: [0; RX_BUF_SIZE],
        }
    }
}

/// One TCP socket, keyed by its cloned minor number.
///
/// The connection is a byte stream: `write` stages one segment of user
/// data, keeps a copy for retransmission until it is ACKed, and `read`
/// returns whatever has been received and ACKed. A listening socket holds
/// its pending connections in [`TcpSock::accept_queue`].
#[repr(C)]
struct TcpSock {
    in_use: bool,
    minor: i32,
    /// Current `nwio_tcpconf_t` flags (NWTC_*).
    flags: u32,
    loc_port: u16,
    rem_port: u16,
    loc_addr: [u8; 4],
    rem_addr: [u8; 4],
    /// Peer MAC captured at connect time (for ACKs during demux).
    rem_mac: [u8; 6],
    state: TcpState,
    /// Initial send sequence number.
    iss: u32,
    /// Next sequence number to send.
    snd_nxt: u32,
    /// Oldest unacknowledged sequence number (first byte in `tx_buf`).
    snd_una: u32,
    /// Next sequence number expected from the peer.
    rcv_nxt: u32,
    /// Error recorded by demux (e.g. ECONNREFUSED on RST).
    err: i32,
    /// Received byte stream.
    rx_len: usize,
    rx_buf: [u8; RX_BUF_SIZE],
    /// Unacknowledged outgoing bytes (retransmission buffer).
    tx_len: usize,
    tx_buf: [u8; RX_BUF_SIZE],
    /// Poll rounds since the last send/ACK; drives retransmission.
    retx_rounds: u32,
    /// Accept cookie (fresh sockets only) and whether it was issued.
    cookie: TcpCookie,
    cookie_set: bool,
    /// `listen()` backlog and the pending connections themselves.
    backlog: usize,
    accept_queue: [PendingConn; ACCEPT_QUEUE_MAX],
}

impl TcpSock {
    const fn init(minor: i32, in_use: bool) -> Self {
        const EMPTY_PENDING: PendingConn = PendingConn::init();
        Self {
            in_use,
            minor,
            flags: 0,
            loc_port: 0,
            rem_port: 0,
            loc_addr: [0; 4],
            rem_addr: [0; 4],
            rem_mac: [0; 6],
            state: TcpState::Closed,
            iss: 0,
            snd_nxt: 0,
            snd_una: 0,
            rcv_nxt: 0,
            err: 0,
            rx_len: 0,
            rx_buf: [0; RX_BUF_SIZE],
            tx_len: 0,
            tx_buf: [0; RX_BUF_SIZE],
            retx_rounds: 0,
            cookie: TcpCookie {
                tc_ref: 0,
                tc_secret: [0; 12],
            },
            cookie_set: false,
            backlog: 0,
            accept_queue: [EMPTY_PENDING; ACCEPT_QUEUE_MAX],
        }
    }
}

struct TcpSocketTableCell(UnsafeCell<[TcpSock; NR_TCP_SOCKETS]>);
unsafe impl Sync for TcpSocketTableCell {}
impl TcpSocketTableCell {
    const fn new() -> Self {
        const EMPTY: TcpSock = TcpSock::init(0, false);
        Self(UnsafeCell::new([EMPTY; NR_TCP_SOCKETS]))
    }
    fn get(&self) -> *mut [TcpSock; NR_TCP_SOCKETS] {
        self.0.get()
    }
}
static TCP_SOCKETS: TcpSocketTableCell = TcpSocketTableCell::new();

/// Test hook: when non-zero, the next data-segment transmit is silently
/// dropped (counted as sent, nothing leaves the NIC). Poked from the QMP
/// monitor to force the retransmission path on the lossless QEMU link.
static TEST_DROP_TX: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Count of data-segment retransmissions (for the QMP verify probe).
static STAT_TX_RETRANS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Per-socket cookie secret: pseudo-random-looking, stable for the lifetime
/// of the socket (both NWIOGTCPCOOKIE and NWIOTCPACCEPTTO see the same
/// bytes). The reference draws from a kernel RNG; the counter mix is
/// sufficient for the clone-minor protocol where the cookie names a socket
/// slot the caller already holds open.
fn next_cookie_secret() -> [u8; 12] {
    static SEQ: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0x5EED_0001);
    let v = SEQ.fetch_add(0x9E37_79B9, core::sync::atomic::Ordering::Relaxed);
    let mut secret = [0u8; 12];
    for (i, b) in secret.iter_mut().enumerate() {
        *b = ((v >> ((i % 4) * 8)) as u8)
            .wrapping_mul(0x5D)
            .wrapping_add(i as u8);
    }
    secret
}

/// Find the live TCP socket for a cloned minor number.
fn tcp_socket_for_minor(minor: i32) -> Option<&'static mut TcpSock> {
    unsafe {
        let socks = &mut *TCP_SOCKETS.get();
        socks.iter_mut().find(|s| s.in_use && s.minor == minor)
    }
}

/// True if `minor` is a cloned TCP socket minor.
fn minor_is_tcp(minor: i32) -> bool {
    (TCP_SOCKET_MINOR_BASE..TCP_SOCKET_MINOR_BASE + NR_TCP_SOCKETS as i32).contains(&minor)
}

/// True if `minor` is a cloned UDP socket minor.
fn minor_is_udp(minor: i32) -> bool {
    (SOCKET_MINOR_BASE..SOCKET_MINOR_BASE + NR_SOCKETS as i32).contains(&minor)
}

/// Next TCP initial send sequence number.
fn next_iss() -> u32 {
    static SEQ: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0x2000);
    SEQ.fetch_add(0x1024, core::sync::atomic::Ordering::Relaxed)
}

// ---- DL protocol helpers ----

/// Build a DL request message and SENDREC it to virtio_net. Returns the
/// sender endpoint (>= 0) or a negative error.
fn dl_sendrec(msg: &mut Message, mtype: u32) -> i32 {
    #[cfg(not(target_os = "none"))]
    {
        // Host builds have no virtio_net peer; a raw `syscall` here would
        // invoke an arbitrary host syscall. Tests drive the TCP logic
        // directly, so a clean failure is correct.
        let _ = (msg, mtype);
        -1
    }
    #[cfg(target_os = "none")]
    {
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
fn read_be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
fn write_be32(b: &mut [u8], v: u32) {
    b[..4].copy_from_slice(&v.to_be_bytes());
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

/// True if `a` is strictly behind `b` in TCP sequence space (window ≤ 2³¹).
fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

/// Demux an inbound TCP segment: completes the SYN handshake, delivers
/// in-order data to the RX stream (ACKing it), records RST as a connection
/// failure (ECONNREFUSED) for a pending connect, and routes SYNs/data for
/// listening sockets into their accept queues. `src_mac` is the ethernet
/// source MAC, used to reply to a SYN from a peer we have not ARP-resolved.
fn tcp_demux(pkt: &[u8], ihl: usize, src_mac: &[u8; 6]) {
    let tcp = &pkt[ihl..];
    if tcp.len() < TCP_HDR_LEN {
        return;
    }
    let src_port = read_be16(&tcp[0..2]);
    let dst_port = read_be16(&tcp[2..4]);
    let seq = read_be32(&tcp[4..8]);
    let ack = read_be32(&tcp[8..12]);
    let data_off = ((tcp[12] >> 4) as usize) * 4;
    if data_off < TCP_HDR_LEN || data_off > tcp.len() {
        return;
    }
    let flags = tcp[13];
    let payload = &tcp[data_off..];
    let src_ip: [u8; 4] = pkt[12..16].try_into().unwrap_or([0; 4]);
    unsafe {
        let socks = &mut *TCP_SOCKETS.get();
        // Pass 1: connected/connecting sockets (exact 4-tuple match). A
        // listener and an accepted socket share the local port, so the
        // listener must not shadow the established connection.
        for s in socks.iter_mut() {
            if !s.in_use || s.loc_port != dst_port || s.rem_port != src_port {
                continue;
            }
            if s.rem_addr != [0; 4] && s.rem_addr != src_ip {
                continue;
            }
            if flags & TCP_RST != 0 {
                // Peer aborted — fail a pending connect, drop a live one.
                s.state = TcpState::Closed;
                s.err = ECONNREFUSED;
                return;
            }
            match s.state {
                TcpState::SynSent => {
                    // SYN-ACK validating our SYN (ack == snd_nxt = iss + 1).
                    if flags & (TCP_SYN | TCP_ACK) == TCP_SYN | TCP_ACK && ack == s.snd_nxt {
                        s.rcv_nxt = seq.wrapping_add(1);
                        s.state = TcpState::Established;
                        let _ = tcp_send_segment(s, s.snd_nxt, s.rcv_nxt, TCP_ACK, 0);
                    }
                }
                TcpState::Established => tcp_established_demux(s, ack, seq, payload),
                TcpState::Closed | TcpState::Listening => {}
            }
            return;
        }
        // Pass 2: listening sockets. A SYN opens a pending connection; any
        // other segment belongs to a pending connection already in the
        // queue (data buffered, final handshake ACK completing it).
        for s in socks.iter_mut() {
            if !s.in_use || s.loc_port != dst_port || s.state != TcpState::Listening {
                continue;
            }
            if flags & TCP_SYN != 0 {
                tcp_listen_syn(s, src_ip, src_port, seq, src_mac);
            } else {
                tcp_listen_data(s, src_ip, src_port, seq, ack, flags, payload);
            }
            return;
        }
    }
}

/// Established-socket segment handling: acknowledge sent data (drop acked
/// bytes from the retransmission buffer) and deliver in-order received
/// data, re-ACKing duplicates so a lost ACK does not wedge the peer.
fn tcp_established_demux(s: &mut TcpSock, ack: u32, seq: u32, payload: &[u8]) {
    // ACK progress on our unacked data (single-segment window, so at most
    // the one buffered segment is freed).
    let acked = ack.wrapping_sub(s.snd_una);
    let outstanding = s.snd_nxt.wrapping_sub(s.snd_una);
    if acked != 0 && acked <= outstanding {
        s.tx_buf.copy_within(acked as usize..s.tx_len, 0);
        s.tx_len -= acked as usize;
        s.snd_una = ack;
        s.retx_rounds = 0;
    }
    if payload.is_empty() {
        return; // pure ACK — nothing to deliver
    }
    if seq == s.rcv_nxt {
        let n = payload.len().min(s.rx_buf.len() - s.rx_len);
        s.rx_buf[s.rx_len..s.rx_len + n].copy_from_slice(&payload[..n]);
        s.rx_len += n;
        s.rcv_nxt = s.rcv_nxt.wrapping_add(n as u32);
        let _ = tcp_send_segment(s, s.snd_nxt, s.rcv_nxt, TCP_ACK, 0);
    } else if seq_lt(seq, s.rcv_nxt) {
        // Duplicate of already-acked data: re-ACK so the peer stops.
        let _ = tcp_send_segment(s, s.snd_nxt, s.rcv_nxt, TCP_ACK, 0);
    }
}

/// A SYN for a listening socket: create a pending connection (sending the
/// SYN-ACK) or re-send the SYN-ACK for a duplicate SYN. The accept queue
/// entry is marked established when the handshake's final ACK arrives.
fn tcp_listen_syn(s: &mut TcpSock, src_ip: [u8; 4], src_port: u16, seq: u32, src_mac: &[u8; 6]) {
    for p in s.accept_queue.iter_mut() {
        if p.in_use && p.rem_port == src_port && p.rem_addr == src_ip {
            if !p.established {
                let peer = TcpPeer {
                    loc_port: s.loc_port,
                    rem_port: p.rem_port,
                    loc_addr: s.loc_addr,
                    rem_addr: p.rem_addr,
                    rem_mac: p.rem_mac,
                };
                let mut ip_pkt = [0u8; RX_BUF_SIZE];
                let _ = tcp_send_raw(&mut ip_pkt, &peer, p.iss, p.rcv_nxt, TCP_SYN | TCP_ACK, 0);
            }
            return;
        }
    }
    let p = match s.accept_queue.iter_mut().find(|p| !p.in_use) {
        Some(p) => p,
        None => return, // backlog full — drop the SYN
    };
    // Learn the peer MAC from the frame itself (no ARP round-trip needed).
    unsafe {
        let st = &mut *STATE.get();
        cache_arp(st, src_ip, *src_mac);
    }
    p.in_use = true;
    p.established = false;
    p.rem_addr = src_ip;
    p.rem_port = src_port;
    p.rem_mac = *src_mac;
    p.iss = next_iss();
    p.snd_nxt = p.iss.wrapping_add(1);
    p.rcv_nxt = seq.wrapping_add(1);
    p.rx_len = 0;
    let peer = TcpPeer {
        loc_port: s.loc_port,
        rem_port: p.rem_port,
        loc_addr: s.loc_addr,
        rem_addr: p.rem_addr,
        rem_mac: p.rem_mac,
    };
    let mut ip_pkt = [0u8; RX_BUF_SIZE];
    let _ = tcp_send_raw(&mut ip_pkt, &peer, p.iss, p.rcv_nxt, TCP_SYN | TCP_ACK, 0);
}

/// Any non-SYN segment to a listening socket: it belongs to a pending
/// connection. The handshake's final ACK marks the entry established
/// (ready for `accept`); data is buffered into the entry's RX stream.
fn tcp_listen_data(
    s: &mut TcpSock,
    src_ip: [u8; 4],
    src_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    payload: &[u8],
) {
    let p = match s
        .accept_queue
        .iter_mut()
        .find(|p| p.in_use && p.rem_port == src_port && p.rem_addr == src_ip)
    {
        Some(p) => p,
        None => return, // no such pending connection
    };
    if flags & TCP_ACK != 0 && !p.established && ack == p.snd_nxt {
        p.established = true;
    }
    if payload.is_empty() {
        return;
    }
    if seq == p.rcv_nxt {
        let n = payload.len().min(p.rx_buf.len() - p.rx_len);
        p.rx_buf[p.rx_len..p.rx_len + n].copy_from_slice(&payload[..n]);
        p.rx_len += n;
        p.rcv_nxt = p.rcv_nxt.wrapping_add(n as u32);
        let peer = TcpPeer {
            loc_port: s.loc_port,
            rem_port: p.rem_port,
            loc_addr: s.loc_addr,
            rem_addr: p.rem_addr,
            rem_mac: p.rem_mac,
        };
        let mut ip_pkt = [0u8; RX_BUF_SIZE];
        let _ = tcp_send_raw(&mut ip_pkt, &peer, p.snd_nxt, p.rcv_nxt, TCP_ACK, 0);
    }
}

/// Retransmit unacked data when it has survived `RETX_AFTER_ROUNDS` poll
/// rounds (no timer exists in the net server — the poll loops drive it).
fn maybe_retransmit(s: &mut TcpSock) {
    if s.tx_len == 0 || s.state != TcpState::Established {
        s.retx_rounds = 0;
        return;
    }
    if s.retx_rounds < RETX_AFTER_ROUNDS {
        return;
    }
    s.retx_rounds = 0;
    STAT_TX_RETRANS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let mut ip_pkt = [0u8; RX_BUF_SIZE];
    ip_pkt[IP_HDR_LEN + TCP_HDR_LEN..IP_HDR_LEN + TCP_HDR_LEN + s.tx_len]
        .copy_from_slice(&s.tx_buf[..s.tx_len]);
    let _ = tcp_send_raw(
        &mut ip_pkt[..IP_HDR_LEN + TCP_HDR_LEN + s.tx_len],
        &TcpPeer::from_sock(s),
        s.snd_una,
        s.rcv_nxt,
        TCP_ACK | TCP_PSH,
        s.tx_len,
    );
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

/// Connection endpoints + peer MAC, the parameter bundle for the low-level
/// TCP builders (they are shared by socket sends and accept-queue sends,
/// which have no `TcpSock` of their own yet).
#[derive(Clone, Copy)]
struct TcpPeer {
    loc_port: u16,
    rem_port: u16,
    loc_addr: [u8; 4],
    rem_addr: [u8; 4],
    rem_mac: [u8; 6],
}

impl TcpPeer {
    fn from_sock(s: &TcpSock) -> Self {
        Self {
            loc_port: s.loc_port,
            rem_port: s.rem_port,
            loc_addr: s.loc_addr,
            rem_addr: s.rem_addr,
            rem_mac: s.rem_mac,
        }
    }
}

/// Build IP + TCP headers around a payload already present at
/// `out[IP_HDR_LEN + TCP_HDR_LEN..]` (for `payload_len` bytes). Returns the
/// total datagram length. The TCP checksum covers the pseudo-header + the
/// TCP header + payload (RFC 793).
fn build_tcp_datagram(
    out: &mut [u8],
    s: &TcpSock,
    seq: u32,
    ack: u32,
    flags: u8,
    payload_len: usize,
) -> usize {
    build_tcp_datagram_to(out, &TcpPeer::from_sock(s), seq, ack, flags, payload_len)
}

/// [`build_tcp_datagram`] with explicit connection parameters, so segments
/// for pending (not yet accepted) connections can be built from the
/// listener's local side and the pending conn's remote side.
fn build_tcp_datagram_to(
    out: &mut [u8],
    peer: &TcpPeer,
    seq: u32,
    ack: u32,
    flags: u8,
    payload_len: usize,
) -> usize {
    let total = IP_HDR_LEN + TCP_HDR_LEN + payload_len;
    let src_ip = if peer.loc_addr == [0; 4] {
        OUR_IP
    } else {
        peer.loc_addr
    };

    // IP header.
    out[0] = 0x45; // version 4, IHL 5
    out[1] = 0;
    write_be16(&mut out[2..4], total as u16);
    write_be16(&mut out[4..6], next_ip_id());
    write_be16(&mut out[6..8], 0); // frag
    out[8] = 64; // TTL
    out[9] = IP_PROTO_TCP;
    write_be16(&mut out[10..12], 0); // checksum (filled below)
    out[12..16].copy_from_slice(&src_ip);
    out[16..20].copy_from_slice(&peer.rem_addr);
    let ip_csum = checksum(&out[..IP_HDR_LEN]);
    write_be16(&mut out[10..12], ip_csum);

    // TCP header (checksum field starts zero, filled below).
    let t = IP_HDR_LEN;
    write_be16(&mut out[t..t + 2], peer.loc_port);
    write_be16(&mut out[t + 2..t + 4], peer.rem_port);
    write_be32(&mut out[t + 4..t + 8], seq);
    write_be32(&mut out[t + 8..t + 12], ack);
    out[t + 12] = 0x50; // data offset 5 (20 bytes), reserved 0
    out[t + 13] = flags;
    write_be16(&mut out[t + 14..t + 16], 0xFFFF); // window
    write_be16(&mut out[t + 16..t + 18], 0); // checksum
    write_be16(&mut out[t + 18..t + 20], 0); // urgent

    let mut sum = 0u32;
    csum_add(&mut sum, &src_ip);
    csum_add(&mut sum, &peer.rem_addr);
    csum_add(&mut sum, &[0, IP_PROTO_TCP]); // zero byte + protocol
    csum_add(
        &mut sum,
        &(TCP_HDR_LEN as u16 + payload_len as u16).to_be_bytes(),
    );
    csum_add(&mut sum, &out[t..t + TCP_HDR_LEN]);
    csum_add(&mut sum, &out[t + TCP_HDR_LEN..total]);
    let tcp_csum = csum_done(sum);
    write_be16(&mut out[t + 16..t + 18], tcp_csum);
    total
}

/// Frame and transmit a TCP segment built into `ip_pkt` (payload already at
/// `ip_pkt[IP_HDR_LEN + TCP_HDR_LEN..]`) to `peer.rem_mac`. Returns 0 on
/// success.
fn tcp_send_raw(
    ip_pkt: &mut [u8],
    peer: &TcpPeer,
    seq: u32,
    ack: u32,
    flags: u8,
    payload_len: usize,
) -> i32 {
    let total = build_tcp_datagram_to(ip_pkt, peer, seq, ack, flags, payload_len);
    let tx = unsafe { &mut *TX_BUF.get() };
    let n = build_eth_frame(tx, &peer.rem_mac, ETH_TYPE_IP, &ip_pkt[..total]);
    if dl_write_frame(&tx[..n]) != 0 {
        return -5;
    }
    0
}

/// Frame and transmit a TCP segment for `s` (SYN/ACK/data) via the peer
/// MAC stored on the socket. Returns 0 on success.
fn tcp_send_segment(s: &TcpSock, seq: u32, ack: u32, flags: u8, payload_len: usize) -> i32 {
    if s.rem_mac == [0; 6] {
        return -5;
    }
    let mut ip_pkt = [0u8; RX_BUF_SIZE];
    tcp_send_raw(
        &mut ip_pkt,
        &TcpPeer::from_sock(s),
        seq,
        ack,
        flags,
        payload_len,
    )
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
    // The frame's source MAC is the peer's — needed to answer a SYN for a
    // listening socket without an ARP round-trip.
    let src_mac: [u8; 6] = frame[6..12].try_into().unwrap_or([0; 6]);
    match ethertype {
        ETH_TYPE_ARP => handle_arp(st, payload),
        ETH_TYPE_IP => handle_ip(st, payload, &src_mac),
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
fn handle_ip(st: &mut NetState, pkt: &[u8], src_mac: &[u8; 6]) -> bool {
    if pkt.len() < IP_HDR_LEN {
        return false;
    }
    let ihl = (pkt[0] & 0x0F) as usize * 4;
    if pkt.len() < ihl + ICMP_HDR_LEN {
        return false;
    }
    // Trim Ethernet padding: virtio-net pads short frames to the 60-byte
    // minimum, but the IP total length (bytes 2..4) is the true datagram
    // size. Without this, a padded pure-ACK TCP frame would deliver its
    // padding as data (UDP is unaffected — udp_demux trusts the UDP
    // length field; TCP has none).
    let ip_total = read_be16(&pkt[2..4]) as usize;
    let pkt = if ip_total >= ihl && ip_total <= pkt.len() {
        &pkt[..ip_total]
    } else {
        pkt
    };
    if pkt[9] == IP_PROTO_TCP {
        tcp_demux(pkt, ihl, src_mac);
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

/// CDEV_OPEN: minor 0 (/dev/ip) is a plain open; minor 1 (/dev/udp) and
/// minor 2 (/dev/tcp) allocate a socket slot and reply with a cloned
/// minor, flagged as a vircopy-I/O channel.
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
        TCP_DEV_MINOR => unsafe {
            let socks = &mut *TCP_SOCKETS.get();
            for (i, slot) in socks.iter_mut().enumerate() {
                if !slot.in_use {
                    let clone_minor = TCP_SOCKET_MINOR_BASE + i as i32;
                    *slot = TcpSock::init(clone_minor, true);
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
    if minor_is_tcp(minor) {
        return match tcp_socket_for_minor(minor) {
            Some(s) => {
                s.in_use = false;
                0
            }
            None => -9, // EBADF
        };
    }
    match socket_for_minor(minor) {
        Some(s) => {
            s.in_use = false;
            0
        }
        None => -9, // EBADF
    }
}

/// CDEV_IOCTL: route to the TCP or UDP ioctl handler by minor range.
fn cdev_ioctl_net(msg: &mut Message) -> i32 {
    let minor = msg_i32(msg, 0);
    if minor_is_tcp(minor) {
        tcp_ioctl(msg)
    } else {
        udp_ioctl(msg)
    }
}

/// UDP ioctls: NWIOSUDPOPT / NWIOGUDPOPT. The `nwio_udpopt_t` struct
/// travels at payload bytes 16..32 (VFS's m2_l1 data area).
fn udp_ioctl(msg: &mut Message) -> i32 {
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

/// CDEV_WRITE (vircopy I/O): UDP writes a whole datagram; TCP writes a
/// byte-stream segment. Requires a bound, connected socket.
fn cdev_write_dgram(msg: &Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let user = msg_i32(msg, 8);
    let va = msg_u64(msg, 16);
    let len = msg_u64(msg, 24);
    if minor_is_tcp(minor) {
        return tcp_write(minor, user, va, len);
    }
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

/// CDEV_READ (vircopy I/O): UDP returns one datagram; TCP returns the
/// next chunk of the received byte stream. Polls the NIC for a bounded
/// time, then copies the result into the user's buffer.
fn cdev_read_dgram(msg: &mut Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let user = msg_i32(msg, 8);
    let va = msg_u64(msg, 16);
    let count = msg_u64(msg, 24);
    if count == 0 {
        return 0;
    }
    if minor_is_tcp(minor) {
        return tcp_read(minor, user, va, count);
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

// ---- TCP socket handlers ----

/// TCP ioctls: NWIOGTCPCONF / NWIOSTCPCONF / NWIOTCPCONN / NWIOTCPLISTENQ
/// / NWIOGTCPCOOKIE / NWIOTCPACCEPTTO. The conf struct travels at payload
/// bytes 16..32, the connect struct at 16..24, the listen backlog at
/// 16..20 and the accept cookie at 16..32.
fn tcp_ioctl(msg: &mut Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let request = msg_u32(msg, 4);
    match request {
        NWIOSTCPCONF => {
            let s = match tcp_socket_for_minor(minor) {
                Some(s) => s,
                None => return -9, // EBADF
            };
            let conf = NwioTcpConf::read_from(unsafe { &msg.m_payload.raw[16..32] });
            tcp_setconf(s, &conf)
        }
        NWIOGTCPCONF => {
            let s = match tcp_socket_for_minor(minor) {
                Some(s) => s,
                None => return -9, // EBADF
            };
            let conf = NwioTcpConf {
                nwtc_flags: s.flags,
                nwtc_locaddr: ip_to_u32(&s.loc_addr),
                nwtc_remaddr: ip_to_u32(&s.rem_addr),
                nwtc_locport: s.loc_port,
                nwtc_remport: s.rem_port,
            };
            conf.write_to(unsafe { &mut msg.m_payload.raw[16..32] });
            0
        }
        NWIOTCPCONN => {
            // The connect struct carries TCF_* flags; only the blocking
            // TCF_DEFAULT connect is implemented (the ioctl blocks until
            // the connection is established or fails).
            let _cl = NwioTcpCl::read_from(unsafe { &msg.m_payload.raw[16..24] });
            tcp_connect(minor)
        }
        NWIOTCPLISTENQ => {
            let s = match tcp_socket_for_minor(minor) {
                Some(s) => s,
                None => return -9, // EBADF
            };
            let backlog = i32::from_ne_bytes(
                unsafe { &msg.m_payload.raw[16..20] }
                    .try_into()
                    .unwrap_or([0; 4]),
            );
            tcp_listenq(s, backlog)
        }
        NWIOGTCPCOOKIE => {
            let s = match tcp_socket_for_minor(minor) {
                Some(s) => s,
                None => return -9, // EBADF
            };
            if !s.cookie_set {
                s.cookie.tc_ref = minor as u32;
                s.cookie.tc_secret = next_cookie_secret();
                s.cookie_set = true;
            }
            s.cookie.write_to(unsafe { &mut msg.m_payload.raw[16..32] });
            0
        }
        NWIOTCPACCEPTTO => {
            let cookie = TcpCookie::read_from(unsafe { &msg.m_payload.raw[16..32] });
            tcp_acceptto(minor, cookie)
        }
        _ => -25, // ENOTTY
    }
}

/// NWIOTCPLISTENQ: put the socket into the listening state with a bounded
/// backlog. The local port must already be bound (NWIOSTCPCONF with
/// NWTC_LP_SET), mirroring the reference's requirement that the config be
/// set before listen.
fn tcp_listenq(s: &mut TcpSock, backlog: i32) -> i32 {
    if s.state != TcpState::Closed {
        return EISCONN;
    }
    if s.loc_port == 0 {
        return -22; // EINVAL: no local port bound
    }
    s.backlog = (backlog as usize).clamp(1, ACCEPT_QUEUE_MAX);
    s.state = TcpState::Listening;
    0
}

/// Index of the first established pending connection on the listening
/// socket `minor`, if any.
fn ready_pending(minor: i32) -> Option<usize> {
    let s = tcp_socket_for_minor(minor)?;
    s.accept_queue
        .iter()
        .position(|p| p.in_use && p.established)
}

/// NWIOTCPACCEPTTO: block (bounded poll) until the listener has an
/// established pending connection, then move it to the fresh socket named
/// by `cookie` (its clone minor plus the per-socket secret). Returns 0, or
/// EAGAIN when no connection completed within the poll window (the libc
/// `accept` retries).
fn tcp_acceptto(minor: i32, cookie: TcpCookie) -> i32 {
    {
        let s = match tcp_socket_for_minor(minor) {
            Some(s) => s,
            None => return -9, // EBADF
        };
        if s.state != TcpState::Listening {
            return -22; // EINVAL: not listening
        }
    }
    for _ in 0..READ_POLL_ROUNDS {
        if let Some(i) = ready_pending(minor) {
            return move_pending(minor, i, cookie);
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
    // Final check after the poll window (a SYN may have completed in the
    // last round without a re-check).
    if let Some(i) = ready_pending(minor) {
        return move_pending(minor, i, cookie);
    }
    EAGAIN
}

/// Move the pending connection at queue index `qi` of the listening socket
/// `minor` onto the fresh socket named by `cookie`, transferring the
/// connection state (peer, sequence numbers, buffered RX data). The fresh
/// socket becomes `Established`.
fn move_pending(minor: i32, qi: usize, cookie: TcpCookie) -> i32 {
    unsafe {
        let socks = &mut *TCP_SOCKETS.get();
        let dst_pos = match socks
            .iter()
            .position(|s| s.in_use && s.minor == cookie.tc_ref as i32)
        {
            Some(p) => p,
            None => return -22, // EINVAL: no such fresh socket
        };
        let lst_pos = match socks.iter().position(|s| s.in_use && s.minor == minor) {
            Some(p) => p,
            None => return -9, // EBADF
        };
        // Validate before mutating anything.
        if !socks[dst_pos].cookie_set
            || socks[dst_pos].cookie != cookie
            || socks[dst_pos].state != TcpState::Closed
        {
            return -22; // EINVAL: bad cookie or busy fresh socket
        }
        if socks[lst_pos].state != TcpState::Listening || qi >= socks[lst_pos].accept_queue.len() {
            return -22; // EINVAL
        }
        if !socks[lst_pos].accept_queue[qi].in_use || !socks[lst_pos].accept_queue[qi].established {
            return EAGAIN;
        }
        // Snapshot the pending connection (plus the listener's local side,
        // which the accepted socket inherits), then free the queue slot.
        let (loc_port, loc_addr) = (socks[lst_pos].loc_port, socks[lst_pos].loc_addr);
        let (rem_addr, rem_port, rem_mac, iss, snd_nxt, rcv_nxt, rx_len, rx_buf) = {
            let p = &mut socks[lst_pos].accept_queue[qi];
            let r = (
                p.rem_addr, p.rem_port, p.rem_mac, p.iss, p.snd_nxt, p.rcv_nxt, p.rx_len, p.rx_buf,
            );
            p.in_use = false;
            p.established = false;
            r
        };
        let dst = &mut socks[dst_pos];
        dst.loc_port = loc_port;
        dst.loc_addr = loc_addr;
        dst.rem_addr = rem_addr;
        dst.rem_port = rem_port;
        dst.rem_mac = rem_mac;
        dst.iss = iss;
        dst.snd_nxt = snd_nxt;
        dst.snd_una = snd_nxt; // our SYN was consumed by the handshake
        dst.rcv_nxt = rcv_nxt;
        dst.rx_len = rx_len;
        dst.rx_buf[..rx_len].copy_from_slice(&rx_buf[..rx_len]);
        dst.tx_len = 0;
        dst.err = 0;
        dst.retx_rounds = 0;
        dst.state = TcpState::Established;
    }
    0
}

/// Apply a NWIOSTCPCONF struct to a socket. Flag groups update only the
/// fields whose mask bits are set, so connect() after socket() only
/// touches the remote address/port and the auto local port.
fn tcp_setconf(s: &mut TcpSock, conf: &NwioTcpConf) -> i32 {
    let f = conf.nwtc_flags;
    // NWTC_LP_SEL (0x30) includes the LP_SET bit (0x20), so discriminate
    // the local-port mode via the mask, not a single bit test.
    match f & NWTC_LOCPORT_MASK {
        NWTC_LP_SET => s.loc_port = conf.nwtc_locport,
        NWTC_LP_SEL => {
            // Auto local port: unique per socket slot.
            s.loc_port = EPHEMERAL_PORT_BASE + (s.minor - TCP_SOCKET_MINOR_BASE) as u16;
        }
        _ => {}
    }
    if f & NWTC_SET_RA != 0 {
        s.rem_addr = u32_to_ip(conf.nwtc_remaddr);
    } else if f & NWTC_UNSET_RA != 0 {
        s.rem_addr = [0; 4];
    }
    if f & NWTC_SET_RP != 0 {
        s.rem_port = conf.nwtc_remport;
    } else if f & NWTC_UNSET_RP != 0 {
        s.rem_port = 0;
    }
    s.flags = f;
    0
}

/// NWIOTCPCONN: run the three-way handshake (SYN → SYN-ACK → ACK) and
/// block until the connection is established. Returns 0, or a negative
/// errno (ECONNREFUSED on RST, ETIMEDOUT when no SYN-ACK arrives).
fn tcp_connect(minor: i32) -> i32 {
    // Initialize the connection state.
    {
        let s = match tcp_socket_for_minor(minor) {
            Some(s) => s,
            None => return -9, // EBADF
        };
        if s.state != TcpState::Closed {
            return EISCONN;
        }
        if s.rem_addr == [0; 4] || s.rem_port == 0 || s.loc_port == 0 {
            return ENOTCONN;
        }
        s.iss = next_iss();
        s.snd_nxt = s.iss.wrapping_add(1);
        s.snd_una = s.snd_nxt;
        s.rcv_nxt = 0;
        s.err = 0;
        s.state = TcpState::SynSent;
    }

    // Resolve the peer and send the SYN.
    let send_r = {
        let s = match tcp_socket_for_minor(minor) {
            Some(s) => s,
            None => return -9,
        };
        let mac = match arp_resolve(&s.rem_addr) {
            Some(m) => m,
            None => {
                s.state = TcpState::Closed;
                return -5; // EIO: ARP resolution failed / no NIC
            }
        };
        s.rem_mac = mac;
        let mut ip_pkt = [0u8; RX_BUF_SIZE];
        let total = build_tcp_datagram(&mut ip_pkt, s, s.iss, 0, TCP_SYN, 0);
        let tx = unsafe { &mut *TX_BUF.get() };
        let n = build_eth_frame(tx, &mac, ETH_TYPE_IP, &ip_pkt[..total]);
        dl_write_frame(&tx[..n])
    };
    if send_r != 0 {
        if let Some(s) = tcp_socket_for_minor(minor) {
            s.state = TcpState::Closed;
        }
        return -5;
    }

    // Poll for the SYN-ACK (tcp_demux completes the handshake).
    for _ in 0..READ_POLL_ROUNDS {
        {
            let s = match tcp_socket_for_minor(minor) {
                Some(s) => s,
                None => return -9,
            };
            match s.state {
                TcpState::Established => return 0,
                TcpState::Closed => return if s.err != 0 { s.err } else { ETIMEDOUT },
                TcpState::SynSent | TcpState::Listening => {}
            }
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
    let s = match tcp_socket_for_minor(minor) {
        Some(s) => s,
        None => return -9,
    };
    match s.state {
        TcpState::Established => 0,
        _ => {
            let e = if s.err != 0 { s.err } else { ETIMEDOUT };
            s.state = TcpState::Closed;
            e
        }
    }
}

/// TCP stream write: copy up to one segment of user data, buffer it for
/// retransmission, and transmit it (seq = snd_nxt). A single-segment send
/// window: while a previous segment is unacked, a short drain reclaims the
/// window and the write returns 0 (a stream short write — the caller
/// retries). Returns the bytes sent.
fn tcp_write(minor: i32, user: i32, va: u64, count: u64) -> i32 {
    // Reclaim the window / drive retransmission before accepting new data.
    for _ in 0..2 {
        let has_unacked = {
            let s = match tcp_socket_for_minor(minor) {
                Some(s) => s,
                None => return -9, // EBADF
            };
            maybe_retransmit(s);
            s.tx_len > 0
        };
        if !has_unacked {
            break;
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
    let s = match tcp_socket_for_minor(minor) {
        Some(s) => s,
        None => return -9, // EBADF
    };
    if s.state != TcpState::Established {
        return ENOTCONN;
    }
    if s.rem_mac == [0; 6] {
        return ENOTCONN;
    }
    if count == 0 {
        return 0;
    }
    if s.tx_len > 0 {
        // Unacked data still in flight after the drain — no window.
        return 0;
    }
    let max_payload = (RX_BUF_SIZE - IP_HDR_LEN - TCP_HDR_LEN) as u64;
    let n = count.min(max_payload) as usize;
    let mut ip_pkt = [0u8; RX_BUF_SIZE];
    let copy_r = minix_rt::sys_vircopy(
        user,
        va,
        minix_rt::SELF,
        ip_pkt.as_mut_ptr() as u64 + (IP_HDR_LEN + TCP_HDR_LEN) as u64,
        n,
    );
    if copy_r != 0 {
        return copy_r;
    }
    let seq = s.snd_nxt;
    let ack = s.rcv_nxt;
    let peer = TcpPeer::from_sock(s);
    s.tx_buf[..n].copy_from_slice(&ip_pkt[IP_HDR_LEN + TCP_HDR_LEN..IP_HDR_LEN + TCP_HDR_LEN + n]);
    s.tx_len = n;
    s.snd_nxt = s.snd_nxt.wrapping_add(n as u32);
    s.retx_rounds = 0;
    // TEST_DROP_TX: count the segment as sent without transmitting it — no
    // ACK will come, so the poll loops retransmit from tx_buf.
    if TEST_DROP_TX.fetch_sub(1, core::sync::atomic::Ordering::Relaxed) > 0 {
        return n as i32;
    }
    let total = build_tcp_datagram_to(
        &mut ip_pkt[..IP_HDR_LEN + TCP_HDR_LEN + n],
        &peer,
        seq,
        ack,
        TCP_ACK | TCP_PSH,
        n,
    );
    let tx = unsafe { &mut *TX_BUF.get() };
    let flen = build_eth_frame(tx, &peer.rem_mac, ETH_TYPE_IP, &ip_pkt[..total]);
    if dl_write_frame(&tx[..flen]) != 0 {
        return -5;
    }
    n as i32
}

/// TCP stream read: return the next chunk of the received byte stream
/// (acknowledged data), polling the NIC for a bounded time first. The poll
/// also drives retransmission of unacked data and reclaims the send window.
fn tcp_read(minor: i32, user: i32, va: u64, count: u64) -> i32 {
    if count == 0 {
        return 0;
    }
    for _ in 0..READ_POLL_ROUNDS {
        let (ready, n) = {
            let s = match tcp_socket_for_minor(minor) {
                Some(s) => s,
                None => return -9, // EBADF
            };
            maybe_retransmit(s);
            (s.rx_len > 0, s.rx_len.min(count as usize))
        };
        if ready {
            let s = match tcp_socket_for_minor(minor) {
                Some(s) => s,
                None => return -9,
            };
            let r = minix_rt::sys_vircopy(minix_rt::SELF, s.rx_buf.as_ptr() as u64, user, va, n);
            if r != 0 {
                return r;
            }
            s.rx_buf.copy_within(n..s.rx_len, 0);
            s.rx_len -= n;
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
    // Final check after the poll window.
    let s = match tcp_socket_for_minor(minor) {
        Some(s) => s,
        None => return -9,
    };
    if s.rx_len > 0 {
        let n = s.rx_len.min(count as usize);
        let r = minix_rt::sys_vircopy(minix_rt::SELF, s.rx_buf.as_ptr() as u64, user, va, n);
        if r != 0 {
            return r;
        }
        s.rx_buf.copy_within(n..s.rx_len, 0);
        s.rx_len -= n;
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

        assert_eq!(core::mem::offset_of!(TcpSock, in_use), 0);
        assert_eq!(core::mem::offset_of!(TcpSock, minor), 4);
        assert_eq!(core::mem::offset_of!(TcpSock, state), 30);
        assert_eq!(core::mem::offset_of!(TcpSock, iss), 32);
        assert_eq!(core::mem::offset_of!(TcpSock, snd_nxt), 36);
        assert_eq!(core::mem::offset_of!(TcpSock, snd_una), 40);
        assert_eq!(core::mem::offset_of!(TcpSock, rcv_nxt), 44);
        assert_eq!(core::mem::offset_of!(TcpSock, err), 48);
        assert_eq!(core::mem::offset_of!(TcpSock, rx_len), 56);
        assert_eq!(core::mem::offset_of!(TcpSock, rx_buf), 64);
        assert_eq!(core::mem::offset_of!(TcpSock, tx_len), 2112);
        assert_eq!(core::mem::offset_of!(TcpSock, tx_buf), 2120);
        assert_eq!(core::mem::offset_of!(TcpSock, retx_rounds), 4168);
        assert_eq!(core::mem::offset_of!(TcpSock, cookie), 4172);
        assert_eq!(core::mem::offset_of!(TcpSock, cookie_set), 4188);
        assert_eq!(core::mem::offset_of!(TcpSock, backlog), 4192);
        assert_eq!(core::mem::offset_of!(TcpSock, accept_queue), 4200);
        assert_eq!(core::mem::offset_of!(PendingConn, in_use), 0);
        assert_eq!(core::mem::offset_of!(PendingConn, established), 1);
        assert_eq!(core::mem::offset_of!(PendingConn, rem_port), 6);
        assert_eq!(core::mem::offset_of!(PendingConn, rcv_nxt), 24);
        assert_eq!(core::mem::offset_of!(PendingConn, rx_len), 32);
        assert_eq!(core::mem::offset_of!(PendingConn, rx_buf), 40);
    }

    /// Sequence-space ordering helper (RFC 1982 semantics for a 2³¹ window).
    #[test]
    fn seq_lt_orders_wrapping_sequences() {
        assert!(seq_lt(5, 10));
        assert!(!seq_lt(10, 5));
        assert!(!seq_lt(5, 5));
        // Wraparound: a small seq is ahead of a near-u32::MAX one.
        assert!(seq_lt(u32::MAX - 3, 5));
        assert!(!seq_lt(5, u32::MAX - 3));
    }

    /// Listen requires a bound local port; a bound socket enters Listening.
    /// Uses a local socket, so no shared state is touched.
    #[test]
    fn listen_requires_bound_port() {
        let mut s = TcpSock::init(0, true);
        assert_eq!(tcp_listenq(&mut s, 4), -22); // EINVAL: not bound
        s.loc_port = 20000;
        assert_eq!(tcp_listenq(&mut s, 4), 0);
        assert_eq!(s.state, TcpState::Listening);
        assert_eq!(s.backlog, 4);
        // Listening again is a busy-socket error, like connect.
        assert_eq!(tcp_listenq(&mut s, 2), EISCONN);
    }

    /// The retransmission and accept-queue machinery, exercised on one
    /// thread (they share the net server's statics, so they must not run
    /// in parallel with each other): ACKs free the unacked buffer, a stale
    /// segment is retransmitted, a SYN seeds the accept queue, the final
    /// handshake ACK establishes it, and accept moves it to a fresh socket.
    #[test]
    fn tcp_retransmit_and_accept_queue_logic() {
        // --- ACK processing frees the unacked buffer ---
        let mut s = TcpSock::init(0, true);
        s.state = TcpState::Established;
        s.rem_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        s.iss = 0x2000;
        s.snd_nxt = s.iss.wrapping_add(1);
        s.snd_una = s.snd_nxt;
        s.rcv_nxt = 0x1234;
        s.tx_buf[..5].copy_from_slice(b"hello");
        s.tx_len = 5;
        s.snd_nxt = s.snd_una.wrapping_add(5);
        s.retx_rounds = 0;
        // Pure ACK covering all 5 bytes: buffer freed, window reclaimed.
        let ack = s.snd_una.wrapping_add(5);
        tcp_established_demux(&mut s, ack, 0, &[]);
        assert_eq!(s.tx_len, 0);
        assert_eq!(s.snd_una, s.iss.wrapping_add(6));
        assert_eq!(s.retx_rounds, 0);
        // Partial ACK leaves the remainder for retransmission.
        s.tx_buf[..5].copy_from_slice(b"hello");
        s.tx_len = 5;
        s.snd_una = s.iss.wrapping_add(6);
        s.snd_nxt = s.snd_una.wrapping_add(5);
        let ack = s.snd_una.wrapping_add(2);
        tcp_established_demux(&mut s, ack, 0, &[]);
        assert_eq!(s.tx_len, 3);
        assert_eq!(s.snd_una, s.iss.wrapping_add(8));

        // --- Stale unacked data is retransmitted ---
        let before = STAT_TX_RETRANS.load(core::sync::atomic::Ordering::Relaxed);
        s.retx_rounds = RETX_AFTER_ROUNDS - 1;
        maybe_retransmit(&mut s);
        assert_eq!(
            STAT_TX_RETRANS.load(core::sync::atomic::Ordering::Relaxed),
            before,
            "below the threshold nothing is resent"
        );
        s.retx_rounds = RETX_AFTER_ROUNDS;
        maybe_retransmit(&mut s);
        assert_eq!(
            STAT_TX_RETRANS.load(core::sync::atomic::Ordering::Relaxed),
            before + 1,
            "a stale segment is retransmitted"
        );
        assert_eq!(s.retx_rounds, 0);

        // --- The drop hook leaves data unacked, forcing the retransmit ---
        let drop_before = TEST_DROP_TX.load(core::sync::atomic::Ordering::Relaxed);
        TEST_DROP_TX.store(drop_before + 1, core::sync::atomic::Ordering::Relaxed);
        s.tx_len = 0;
        s.snd_una = s.iss.wrapping_add(8);
        s.snd_nxt = s.snd_una;
        let retrans_before = STAT_TX_RETRANS.load(core::sync::atomic::Ordering::Relaxed);
        // Simulate the write path's drop: buffer + advance snd_nxt without
        // transmitting (as tcp_write does when TEST_DROP_TX fires).
        s.tx_buf[..3].copy_from_slice(b"abc");
        s.tx_len = 3;
        s.snd_nxt = s.snd_nxt.wrapping_add(3);
        s.retx_rounds = RETX_AFTER_ROUNDS;
        maybe_retransmit(&mut s);
        assert_eq!(
            STAT_TX_RETRANS.load(core::sync::atomic::Ordering::Relaxed),
            retrans_before + 1,
            "dropped data is retransmitted from the buffer"
        );
        TEST_DROP_TX.store(drop_before, core::sync::atomic::Ordering::Relaxed);

        // --- A SYN seeds the listener's accept queue ---
        let mut listener = TcpSock::init(0, true);
        listener.loc_port = 20000;
        listener.state = TcpState::Listening;
        let mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        tcp_listen_syn(&mut listener, [10, 0, 2, 2], 40000, 0x1000, &mac);
        let pending_snd = listener.accept_queue[0].snd_nxt;
        let pending_rcv = listener.accept_queue[0].rcv_nxt;
        assert!(listener.accept_queue[0].in_use);
        assert!(!listener.accept_queue[0].established);
        assert_eq!(listener.accept_queue[0].rem_addr, [10, 0, 2, 2]);
        assert_eq!(listener.accept_queue[0].rem_port, 40000);
        assert_eq!(listener.accept_queue[0].rem_mac, mac);
        assert_eq!(pending_rcv, 0x1001);
        assert_eq!(pending_snd, listener.accept_queue[0].iss.wrapping_add(1));
        // The final handshake ACK establishes the pending connection.
        tcp_listen_data(
            &mut listener,
            [10, 0, 2, 2],
            40000,
            pending_rcv,
            pending_snd,
            TCP_ACK,
            &[],
        );
        assert!(listener.accept_queue[0].established);
        // Data arriving before accept is buffered in the pending conn.
        let data_seq = listener.accept_queue[0].rcv_nxt;
        tcp_listen_data(
            &mut listener,
            [10, 0, 2, 2],
            40000,
            data_seq,
            pending_snd,
            TCP_ACK | TCP_PSH,
            b"hi",
        );
        assert_eq!(listener.accept_queue[0].rx_len, 2);
        assert_eq!(&listener.accept_queue[0].rx_buf[..2], b"hi");
        assert_eq!(listener.accept_queue[0].rcv_nxt, 0x1001 + 2);

        // --- accept moves the pending connection to the fresh socket ---
        unsafe {
            let socks = &mut *TCP_SOCKETS.get();
            for slot in socks.iter_mut() {
                *slot = TcpSock::init(0, false);
            }
            socks[0] = TcpSock::init(100, true);
            socks[0].loc_port = 20000;
            socks[0].state = TcpState::Listening;
            socks[0].accept_queue[0] = listener.accept_queue[0];
            socks[1] = TcpSock::init(101, true);
            socks[1].cookie.tc_ref = 101;
            socks[1].cookie.tc_secret = [7; 12];
            socks[1].cookie_set = true;
        }
        let cookie = TcpCookie {
            tc_ref: 101,
            tc_secret: [7; 12],
        };
        assert_eq!(move_pending(100, 0, cookie), 0);
        unsafe {
            let socks = &mut *TCP_SOCKETS.get();
            let dst = &socks[1];
            assert_eq!(dst.state, TcpState::Established);
            assert_eq!(dst.rem_addr, [10, 0, 2, 2]);
            assert_eq!(dst.rem_port, 40000);
            assert_eq!(dst.rcv_nxt, 0x1003);
            assert_eq!(dst.rx_len, 2);
            assert_eq!(&dst.rx_buf[..2], b"hi");
            assert_eq!(dst.snd_una, dst.snd_nxt);
            assert!(!socks[0].accept_queue[0].in_use, "queue slot freed");
        }
        // A mismatched secret is rejected without mutating anything.
        let bad = TcpCookie {
            tc_ref: 101,
            tc_secret: [8; 12],
        };
        assert_eq!(move_pending(100, 0, bad), -22);
    }
}
