//! UNIX-domain socket server (`/dev/uds`, major 18) — `WAYLAND.md` Phase 0.
//!
//! # Scope
//!
//! The device, the socket table, `socketpair(2)`, and the named-socket path:
//! `bind`, `listen`, `connect`, `accept` and `getsockname`. fd passing
//! (`sendmsg`/`recvmsg`/`SCM_RIGHTS`) and readiness (`select`/`poll`) are the
//! increments after this one (`WAYLAND.md` §6.1–6.2).
//!
//! # Why I/O never blocks
//!
//! VFS performs CDEV I/O with a synchronous `sendrec` to the driver
//! (`vfs/device.rs::cdev_io`), and a *deferred* reply would freeze it — the pty
//! documents the same constraint (`tty.rs`: "a deferred request would freeze
//! VFS"). So a UDS read or write moves what it can and returns `EAGAIN` when it
//! cannot move any, as the pty and `net`'s bounded-poll paths do. That is also
//! Wayland's own model — `wl_display_connect` puts the socket in non-blocking
//! mode and the client polls — so the deviation costs the compositor nothing.
//! The reference suspends instead (`uds.h`'s `UDS_SUSPENDED_*` with
//! `susp_endpt`/`susp_grant`), which its VFS can afford and this one cannot.
//!
//! `accept` follows from the same constraint: with nothing to accept it answers
//! `EAGAIN` rather than parking, and the caller retries.
//!
//! # Connect authorisation
//!
//! The reference authorises `bind`/`connect` with
//! `checkperms(owner, addr.sun_path, …)` (`ioc_uds.c`), i.e. against the
//! *filesystem* permissions of the socket node. This port cannot ask VFS for a
//! path's mode from inside a device driver, so it applies the narrower rule
//! that covers the case Wayland needs: **a socket may be connected to by its
//! binder or by root.** The binder's effective uid is recorded at `bind`; the
//! caller's is read from PM (`PM_GETEPINFO`). A uid that PM cannot resolve
//! denies rather than allows, so an unavailable PM is fail-closed.
//!
//! # Not yet
//!
//! * No `sendmsg`/`recvmsg` or `SCM_RIGHTS` fd passing.
//! * `CDEV_SELECT` answers the ready ops and holds a late watch that a later
//!   readiness change wakes through a *notification* (VFS is told to re-ask,
//!   `vfs::select::rescan_suspended`), but only **one** watcher per socket and
//!   no `CDEV_CANCEL` (a satisfied watch is dropped, not cancelled). `net`
//!   still answers 0.
//! * No socket types (`SOCK_STREAM` is assumed) and no `shutdown`.
//!
//! The pure core below — the table, `open`, `pair`, `bind`, `listen`,
//! `connect`, `accept`, `close` and the read/write plans — takes the table as an
//! argument, so the host tests drive the state machine without the IPC.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use arch_common::com::{
    CDEV_CLONED, CDEV_CLOSE, CDEV_DGRAM, CDEV_DGRAM_OPEN, CDEV_IOCTL, CDEV_NOTIFY, CDEV_OP_ERR,
    CDEV_OP_RD, CDEV_OP_WR, CDEV_OPEN, CDEV_READ, CDEV_SELECT, CDEV_WRITE,
};
use arch_common::ipc::Message;
use net::{
    AF_UNIX, NWIOGUDSADDR, NWIOGUDSMINOR, NWIOGUDSPEERCRED, NWIOSUDSACCEPT, NWIOSUDSADDR,
    NWIOSUDSBLOG, NWIOSUDSCONN, NWIOSUDSCTRL, NWIOSUDSPAIR, SOCKADDR_UN_SIZE, UDS_PATH_MAX,
    UUCRED_SIZE,
};

/// The minor every `open("/dev/uds")` arrives as. As with `/dev/udp`'s minor 1,
/// an open here is answered with a *cloned* socket minor.
pub const UDS_DEV_MINOR: i32 = 0;

/// Clone minors handed to sockets. Disjoint from `net`'s ranges (0x10 UDP,
/// 0x20 TCP) so a `dev` value names exactly one server's socket.
const UDS_SOCKET_MINOR_BASE: i32 = 0x30;

/// Sockets open at once. Also the ceiling on a listening socket's backlog, so
/// a listener can never queue more connections than the table could hold.
pub const NR_UDS_SOCKETS: usize = 8;

/// Bytes one socket can queue for its peer to read.
///
/// The reference's rings are `PIPE_BUF` (32 KiB). A ring is a *buffer*, not a
/// message limit — a short write is legal and the caller retries — and
/// 8 × 4 KiB keeps the table small.
pub const UDS_BUF: usize = 4096;

const ENOENT: i32 = -2;
const ESRCH: i32 = -3;
const EAGAIN: i32 = -11;
const EBADF: i32 = -9;
const EACCES: i32 = -13;
const EINVAL: i32 = -22;
const EMFILE: i32 = -24;
const ENOTTY: i32 = -25;
const EPIPE: i32 = -32;
const EAFNOSUPPORT: i32 = -97;
const ENOTCONN: i32 = -107;
const EISCONN: i32 = -106;
const ECONNREFUSED: i32 = -111;
const EADDRINUSE: i32 = -98;

/// One end of a socket.
#[derive(Clone, Copy)]
struct UdsSock {
    in_use: bool,
    /// The clone minor VFS knows this end by.
    minor: i32,
    /// The other end's table index, or -1 when unpaired *or* the peer has
    /// closed. -1 is what makes a drained read report end-of-input.
    peer: i32,
    /// Bytes queued for this end, always at the front of `rx_buf`.
    rx_len: usize,
    rx_buf: [u8; UDS_BUF],
    /// A named socket: the path it was bound to, its binder's uid, and how
    /// many connections may wait unaccepted.
    bound: bool,
    listening: bool,
    owner_uid: u32,
    /// The endpoint of the process that last issued an ioctl on this socket, or
    /// -1 before any. The reference records this in `uds_ioctl` and later names
    /// the *peer's* owner to PM for `SO_PEERCRED`.
    owner_ep: i32,
    backlog: usize,
    path: [u8; UDS_PATH_MAX],
    path_len: usize,
    /// Established-but-unaccepted ends, oldest first.
    acceptq: [i32; NR_UDS_SOCKETS],
    acceptq_len: usize,
    /// A `select`/`poll`/`epoll` watcher: the ops it asked for that were not
    /// ready (`0` = none) and the endpoint to wake (`CDEV_SELECT`'s sender,
    /// VFS).
    sel_ops: u32,
    sel_ep: i32,
}

impl UdsSock {
    const fn empty() -> Self {
        Self {
            in_use: false,
            minor: 0,
            peer: -1,
            rx_len: 0,
            rx_buf: [0u8; UDS_BUF],
            bound: false,
            listening: false,
            owner_uid: 0,
            owner_ep: -1,
            backlog: 0,
            path: [0u8; UDS_PATH_MAX],
            path_len: 0,
            acceptq: [-1; NR_UDS_SOCKETS],
            acceptq_len: 0,
            sel_ops: 0,
            sel_ep: -1,
        }
    }

    const fn new(minor: i32) -> Self {
        let mut s = Self::empty();
        s.in_use = true;
        s.minor = minor;
        s
    }

    /// The path this socket is bound to, empty when unbound.
    fn path(&self) -> &[u8] {
        &self.path[..self.path_len]
    }
}

struct TableCell(UnsafeCell<[UdsSock; NR_UDS_SOCKETS]>);
// Safety: the server is single-threaded; the table is reached only from its
// receive loop. The host tests drive the pure functions below with their own
// arrays and never touch this static.
unsafe impl Sync for TableCell {}

static SOCKETS: TableCell = TableCell(UnsafeCell::new([UdsSock::empty(); NR_UDS_SOCKETS]));

unsafe fn table() -> &'static mut [UdsSock; NR_UDS_SOCKETS] {
    unsafe { &mut *SOCKETS.0.get() }
}

// ---- the pure core ----

fn index_of(socks: &[UdsSock], minor: i32) -> Option<usize> {
    socks.iter().position(|s| s.in_use && s.minor == minor)
}

/// Take a free slot and give it the next clone minor.
fn alloc_slot(socks: &mut [UdsSock]) -> Option<usize> {
    for (i, s) in socks.iter_mut().enumerate() {
        if !s.in_use {
            *s = UdsSock::new(UDS_SOCKET_MINOR_BASE + i as i32);
            return Some(i);
        }
    }
    None
}

/// `CDEV_OPEN`: allocate a socket and answer with the clone minor. Returns the
/// reply — the clone bit, the datagram bit and the minor — or `EMFILE`.
fn open_inner(socks: &mut [UdsSock]) -> i32 {
    match alloc_slot(socks) {
        Some(i) => (CDEV_CLONED | CDEV_DGRAM_OPEN | socks[i].minor as u32) as i32,
        None => EMFILE,
    }
}

/// Pair two slots. Returns 0, or `EINVAL` if they are the same socket.
fn pair_inner(socks: &mut [UdsSock], a: usize, b: usize) -> i32 {
    if a == b {
        return EINVAL;
    }
    socks[a].peer = b as i32;
    socks[b].peer = a as i32;
    0
}

fn pair_by_minor_inner(socks: &mut [UdsSock], minor: i32, peer_minor: i32) -> i32 {
    let (Some(a), Some(b)) = (index_of(socks, minor), index_of(socks, peer_minor)) else {
        return EBADF;
    };
    let r = pair_inner(socks, a, b);
    if r == 0 {
        notify_select(socks, a);
        notify_select(socks, b);
    }
    r
}

/// Free the socket and detach its peer, so the peer's next drained read reports
/// end-of-input and its next write fails. A listening socket also resets the
/// connections queued on it but never accepted: their clients see EOF.
fn close_inner(socks: &mut [UdsSock], minor: i32) -> i32 {
    let Some(i) = index_of(socks, minor) else {
        return EBADF;
    };
    for k in 0..socks[i].acceptq_len {
        let q = socks[i].acceptq[k] as usize;
        if socks[q].in_use {
            let c = socks[q].peer;
            if c >= 0 && socks[c as usize].in_use {
                socks[c as usize].peer = -1;
                notify_select(socks, c as usize);
            }
            socks[q] = UdsSock::empty();
        }
    }
    let p = socks[i].peer;
    if p >= 0 && socks[p as usize].in_use {
        socks[p as usize].peer = -1;
        // The surviving end is now readable/writable without blocking.
        notify_select(socks, p as usize);
    }
    socks[i] = UdsSock::empty();
    0
}

/// `bind`: name the socket `path`, recording the binder as its owner.
fn bind_inner(socks: &mut [UdsSock], minor: i32, uid: u32, path: &[u8]) -> i32 {
    let Some(i) = index_of(socks, minor) else {
        return EBADF;
    };
    if path.is_empty() {
        return ENOENT; // the reference rejects an empty sun_path the same way
    }
    if path.len() > UDS_PATH_MAX {
        return EINVAL;
    }
    // Re-binding a socket that already has a name is an error; a second socket
    // claiming the name is EADDRINUSE.
    if socks[i].bound {
        return EINVAL;
    }
    if socks
        .iter()
        .any(|s| s.in_use && s.bound && s.path() == path)
    {
        return EADDRINUSE;
    }
    socks[i].path[..path.len()].copy_from_slice(path);
    socks[i].path_len = path.len();
    socks[i].bound = true;
    socks[i].owner_uid = uid;
    0
}

/// `listen`: start accepting connections on a bound socket.
fn listen_inner(socks: &mut [UdsSock], minor: i32, backlog: i32) -> i32 {
    let Some(i) = index_of(socks, minor) else {
        return EBADF;
    };
    if !socks[i].bound || socks[i].listening {
        return EINVAL;
    }
    let want = if backlog <= 0 { 1 } else { backlog as usize };
    socks[i].backlog = want.clamp(1, NR_UDS_SOCKETS);
    socks[i].listening = true;
    0
}

/// The listening socket bound to `path`, if any.
fn find_listener(socks: &[UdsSock], path: &[u8]) -> Option<usize> {
    socks
        .iter()
        .position(|s| s.in_use && s.bound && s.listening && s.path() == path)
}

/// `connect`: match a listener by path and queue a connection on it.
///
/// The connection is established at once — the client end is paired with a
/// fresh server end that `accept` will hand out — because this server never
/// suspends. A listener with a full queue answers `ECONNREFUSED`, so a caller
/// retries rather than waits.
fn connect_inner(socks: &mut [UdsSock], minor: i32, uid: u32, path: &[u8]) -> i32 {
    let Some(i) = index_of(socks, minor) else {
        return EBADF;
    };
    if path.is_empty() {
        return ENOENT;
    }
    if socks[i].listening {
        return EINVAL;
    }
    if socks[i].peer >= 0 {
        return EISCONN;
    }
    let Some(l) = find_listener(socks, path) else {
        return ECONNREFUSED;
    };
    if uid != 0 && uid != socks[l].owner_uid {
        return EACCES;
    }
    if socks[l].acceptq_len >= socks[l].backlog {
        return ECONNREFUSED;
    }
    let Some(srv) = alloc_slot(socks) else {
        return EMFILE;
    };
    socks[i].peer = srv as i32;
    socks[srv].peer = i as i32;
    let q = socks[l].acceptq_len;
    socks[l].acceptq[q] = srv as i32;
    socks[l].acceptq_len = q + 1;
    // A connection now waits: wake a listener watching for readability.
    notify_select(socks, l);
    0
}

/// `accept`: hand the caller the oldest queued connection.
///
/// `minor` is a *fresh* socket the caller opened for the purpose (the
/// reference's `_uds_accept` does the same), and `path` names the listener that
/// the queue belongs to. The queued end is re-homed onto the caller's socket so
/// it keeps the caller's own minor.
fn accept_inner(socks: &mut [UdsSock], minor: i32, path: &[u8]) -> i32 {
    let Some(l) = find_listener(socks, path) else {
        return EBADF;
    };
    let Some(dst) = index_of(socks, minor) else {
        return EBADF;
    };
    if dst == l {
        return EINVAL;
    }
    if socks[l].acceptq_len == 0 {
        return EAGAIN; // nothing to accept; the caller retries
    }
    let srv = socks[l].acceptq[0] as usize;
    for k in 1..socks[l].acceptq_len {
        socks[l].acceptq[k - 1] = socks[l].acceptq[k];
    }
    socks[l].acceptq_len -= 1;
    if !socks[srv].in_use {
        return EBADF;
    }
    let client = socks[srv].peer;
    // The acceptor's socket is being replaced by the queued end, but the
    // acceptor is the one that owns it now, so its endpoint and any select
    // watch are carried over.
    let acceptor_ep = socks[dst].owner_ep;
    let (sel_ops, sel_ep) = (socks[dst].sel_ops, socks[dst].sel_ep);
    let mut taken = socks[srv];
    taken.minor = socks[dst].minor;
    taken.owner_ep = acceptor_ep;
    taken.sel_ops = sel_ops;
    taken.sel_ep = sel_ep;
    socks[dst] = taken;
    socks[srv] = UdsSock::empty();
    if client >= 0 && socks[client as usize].in_use {
        socks[client as usize].peer = dst as i32;
        // The client's connection is live on the server side now.
        notify_select(socks, client as usize);
    }
    // The queue shrank; a second queued connection would wake a watcher here.
    notify_select(socks, l);
    0
}

/// How much of a `len`-byte write can move: `Ok((peer, n))`, or the errno when
/// nothing can.
fn write_plan(socks: &[UdsSock], from: usize, len: usize) -> Result<(usize, usize), i32> {
    if !socks[from].in_use {
        return Err(EBADF);
    }
    let p = socks[from].peer;
    if p < 0 {
        return Err(EPIPE);
    }
    let peer = p as usize;
    if !socks[peer].in_use {
        return Err(EPIPE);
    }
    let space = UDS_BUF - socks[peer].rx_len;
    if space == 0 {
        return Err(EAGAIN);
    }
    Ok((peer, space.min(len)))
}

/// How much a read can take. `Ok(0)` is end-of-input (peer closed, nothing
/// queued); `Err(EAGAIN)` is an empty queue with the peer still open.
fn read_plan(socks: &[UdsSock], at: usize, count: usize) -> Result<usize, i32> {
    if !socks[at].in_use {
        return Err(EBADF);
    }
    if socks[at].rx_len == 0 {
        return if socks[at].peer < 0 {
            Ok(0)
        } else {
            Err(EAGAIN)
        };
    }
    Ok(socks[at].rx_len.min(count))
}

/// Append `bytes` to the peer's queue. Caller has sized it with [`write_plan`].
fn commit_write(socks: &mut [UdsSock], peer: usize, bytes: &[u8]) -> usize {
    let at = socks[peer].rx_len;
    socks[peer].rx_buf[at..at + bytes.len()].copy_from_slice(bytes);
    socks[peer].rx_len = at + bytes.len();
    bytes.len()
}

/// Take from the front of a socket's queue into `out`; returns how many bytes.
/// Caller has sized it with [`read_plan`].
fn commit_read(socks: &mut [UdsSock], at: usize, out: &mut [u8]) -> usize {
    let n = out.len().min(socks[at].rx_len);
    out[..n].copy_from_slice(&socks[at].rx_buf[..n]);
    socks[at].rx_buf.copy_within(n..socks[at].rx_len, 0);
    socks[at].rx_len -= n;
    n
}

/// The ops ready on `socks[at]`: what `select`/`poll`/`epoll` ask and
/// `CDEV_SELECT` answers.
///
/// A listening socket is readable when a connection waits. A connected one is
/// readable with data queued, and either readable or writable when its peer is
/// gone (a read then returns EOF and a write `EPIPE` — neither blocks). A
/// listening socket is never writable.
fn ready_ops(socks: &[UdsSock], at: usize) -> u32 {
    let s = &socks[at];
    if !s.in_use {
        return 0;
    }
    if s.listening {
        return if s.acceptq_len > 0 { CDEV_OP_RD } else { 0 };
    }
    let mut r = 0u32;
    if s.rx_len > 0 || s.peer < 0 {
        r |= CDEV_OP_RD;
    }
    let writable = if s.peer < 0 {
        true
    } else {
        let p = s.peer as usize;
        p < NR_UDS_SOCKETS && socks[p].in_use && socks[p].rx_len < UDS_BUF
    };
    if writable {
        r |= CDEV_OP_WR;
    }
    r
}

/// `CDEV_SELECT`: answer the ready ops, and remember a watcher for the ops that
/// are not ready so a later change can wake it ([`notify_select`]). `ep` is the
/// sender of the request — VFS, which the late reply goes back to.
fn select_inner(socks: &mut [UdsSock], minor: i32, ops: u32, ep: i32) -> i32 {
    let Some(i) = index_of(socks, minor) else {
        return EBADF;
    };
    let want = ops & (CDEV_OP_RD | CDEV_OP_WR | CDEV_OP_ERR);
    let watch = ops & CDEV_NOTIFY;
    let ready = ready_ops(socks, i) & want;
    let remaining = want & !ready;
    if remaining != 0 && watch != 0 {
        socks[i].sel_ops |= remaining;
        socks[i].sel_ep = ep;
    }
    ready as i32
}

/// Wake a watcher on `socks[at]` for whatever it asked for and is now ready.
/// Called wherever readiness can change: data queued, a read draining the ring
/// a peer writes into, a connection queued or accepted, and a peer closing.
fn notify_select(socks: &mut [UdsSock], at: usize) {
    if at >= NR_UDS_SOCKETS || !socks[at].in_use || socks[at].sel_ops == 0 {
        return;
    }
    let ready = ready_ops(socks, at) & socks[at].sel_ops;
    if ready == 0 {
        return;
    }
    let ep = socks[at].sel_ep;
    socks[at].sel_ops &= !ready;
    if ep >= 0 {
        notify_ready(ep);
    }
}

/// Tell `ep` (VFS) that this socket's readiness changed, so it re-asks.
///
/// A *notification*, not a message: the kernel remembers one for a destination
/// that is not receiving and hands it over on that destination's next
/// `RECEIVE`, whereas a non-blocking send answers `ENOTREADY` to a busy
/// destination and the wake is gone. That is not theoretical here — a reply
/// retried on a short alarm phase-locks with VFS's own deadline tick, so VFS is
/// awake at every retry and the send is refused indefinitely (`KNOWN_ISSUES`
/// 36). The input server was fixed the same way, for the same reason.
///
/// The notification carries no payload, which is why VFS re-asks the driver
/// (`vfs::select::rescan_suspended`) rather than reading a report out of it.
fn notify_ready(ep: i32) {
    #[cfg(target_os = "minix")]
    {
        let mut buf = [0u8; 8];
        unsafe {
            minix_rt::syscall2(minix_rt::NOTIFY_CALL, ep as u64, buf.as_mut_ptr() as u64);
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = ep;
    }
}

/// Split a `sockaddr_un`'s bytes into `(family, path)`. The path runs to the
/// first NUL, or to the end of the field when it is not terminated.
fn parse_addr(bytes: &[u8]) -> Option<(u16, &[u8])> {
    if bytes.len() < 2 {
        return None;
    }
    let family = u16::from_ne_bytes([bytes[0], bytes[1]]);
    let field = &bytes[2..];
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    Some((family, &field[..end]))
}

// ---- CDEV handlers ----

/// Handle a CDEV request and return the status VFS will read as the reply.
///
/// # Safety
///
/// Touches the shared socket table; only the server's own receive loop (or the
/// host tests, which do not call this) may invoke it.
unsafe fn handle_cdev_request(msg: &mut Message, call_type: u32, src_ep: i32) -> i32 {
    let minor = msg_i32(msg, 0);
    match call_type {
        CDEV_OPEN => {
            let socks = unsafe { table() };
            open_inner(socks)
        }
        CDEV_CLOSE => {
            let socks = unsafe { table() };
            close_inner(socks, minor)
        }
        // Reads and writes travel the datagram layout only because the open
        // reply set `CDEV_DGRAM_OPEN`; a request without the flag would use the
        // inline layout this server does not implement.
        CDEV_READ => {
            if msg_u32(msg, 4) & CDEV_DGRAM == 0 {
                EINVAL
            } else {
                unsafe { do_read(msg) }
            }
        }
        CDEV_WRITE => {
            if msg_u32(msg, 4) & CDEV_DGRAM == 0 {
                EINVAL
            } else {
                unsafe { do_write(msg) }
            }
        }
        CDEV_IOCTL => unsafe { do_ioctl(msg) },
        // Readiness: answer the ready ops and hold a late watch for the rest
        // (`src_ep` is VFS, which a notification tells to re-ask).
        CDEV_SELECT => {
            let ops = msg_u32(msg, 4);
            let socks = unsafe { table() };
            select_inner(socks, minor, ops, src_ep)
        }
        _ => EINVAL,
    }
}

/// `CDEV_READ` in the datagram layout: the caller's VA is in `m2_l1`, the
/// maximum byte count in `m2_l2`, the caller's endpoint in `m2_i3`.
unsafe fn do_read(msg: &Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let user = msg_i32(msg, 8);
    let va = msg_u64(msg, 16);
    let count = msg_u64(msg, 24) as usize;
    if count == 0 {
        return 0;
    }
    let socks = unsafe { table() };
    let Some(i) = index_of(socks, minor) else {
        return EBADF;
    };
    let n = match read_plan(socks, i, count) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if n == 0 {
        return 0; // end-of-input
    }
    // Move through a scratch buffer: the queue lives in a static, and the
    // vircopy names this server as the source.
    let mut scratch = [0u8; UDS_BUF];
    let moved = commit_read(socks, i, &mut scratch[..n]);
    // Draining this end's queue frees space in it for whoever writes to it.
    let writer = socks[i].peer;
    if writer >= 0 {
        notify_select(socks, writer as usize);
    }
    let r = minix_rt::sys_vircopy(minix_rt::SELF, scratch.as_ptr() as u64, user, va, moved);
    if r != 0 {
        return r;
    }
    moved as i32
}

/// `CDEV_WRITE` in the datagram layout.
unsafe fn do_write(msg: &Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let user = msg_i32(msg, 8);
    let va = msg_u64(msg, 16);
    let len = msg_u64(msg, 24) as usize;
    if len == 0 {
        return 0;
    }
    let socks = unsafe { table() };
    let Some(i) = index_of(socks, minor) else {
        return EBADF;
    };
    let (peer, n) = match write_plan(socks, i, len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut scratch = [0u8; UDS_BUF];
    let r = minix_rt::sys_vircopy(user, va, minix_rt::SELF, scratch.as_mut_ptr() as u64, n);
    if r != 0 {
        return r;
    }
    commit_write(socks, peer, &scratch[..n]);
    // The peer's queue is no longer empty: wake a reader watching it.
    notify_select(socks, peer);
    n as i32
}

/// The calling process's effective uid, or `None` when PM cannot name it.
///
/// A `None` must deny, not allow: an unreachable PM is fail-closed.
fn caller_uid(endpt: i32) -> Option<u32> {
    caller_ids(endpt).map(|(_, uid, _)| uid)
}

/// The `(pid, uid, gid)` PM reports for `endpt`, or `None` when it cannot name
/// it. The same `PM_GETEPINFO` the reference's `getepinfo` wraps.
fn caller_ids(endpt: i32) -> Option<(i32, u32, u32)> {
    #[cfg(target_os = "minix")]
    {
        unsafe { minix_std::process::getepinfo(endpt) }
            .ok()
            .map(|(pid, uid, gid)| (pid, uid as u32, gid as u32))
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = endpt;
        None
    }
}

/// The peer's credentials as the `struct uucred` `SO_PEERCRED` returns.
///
/// C: `getnucred` fills `cr_uid`/`cr_gid` and zeroes the rest
/// (`lib/libsys/getepinfo.c`), so `cr_groups` and `cr_ngroups` travel empty.
fn peer_cred(socks: &[UdsSock], minor: i32) -> Result<[u8; UUCRED_SIZE], i32> {
    let Some(i) = index_of(socks, minor) else {
        return Err(EBADF);
    };
    let peer = socks[i].peer;
    if peer < 0 || !socks[peer as usize].in_use {
        return Err(ENOTCONN);
    }
    let owner = socks[peer as usize].owner_ep;
    if owner < 0 {
        return Err(ENOTCONN);
    }
    let (_, uid, gid) = caller_ids(owner).ok_or(ESRCH)?;
    Ok(net::uucred(uid, gid))
}

/// `CDEV_IOCTL`: the request is in `m2_i2`, the caller's endpoint in `m2_l1`,
/// and the argument struct travels through the magic grant VFS created in
/// `m2_i3` over the caller's buffer.
unsafe fn do_ioctl(msg: &mut Message) -> i32 {
    let minor = msg_i32(msg, 0);
    let request = msg_u32(msg, 4);
    let grant = msg_u32(msg, 8);
    let user_ep = msg_i32(msg, 16);
    let granter = msg.m_source;

    // The reference records the caller as the socket's owner on every ioctl
    // (`uds_ioctl`), which is what `SO_PEERCRED` later reports for the peer.
    {
        let socks = unsafe { table() };
        if let Some(i) = index_of(socks, minor) {
            socks[i].owner_ep = user_ep;
        }
    }

    match request {
        NWIOSUDSPAIR => {
            let mut dev = [0u8; 4];
            let r = safecopy_from_grant(granter, grant, &mut dev);
            if r != 0 {
                return r;
            }
            let peer_minor = (u32::from_ne_bytes(dev) & 0xFFFF) as i32;
            let socks = unsafe { table() };
            let r = pair_by_minor_inner(socks, minor, peer_minor);
            // Both ends of a pair belong to the pairing process (the reference
            // rejects a pair whose owners differ).
            if r == 0
                && let Some(j) = index_of(socks, peer_minor)
            {
                socks[j].owner_ep = user_ep;
            }
            r
        }
        NWIOSUDSADDR | NWIOSUDSCONN | NWIOSUDSACCEPT => {
            let mut raw = [0u8; SOCKADDR_UN_SIZE];
            let r = safecopy_from_grant(granter, grant, &mut raw);
            if r != 0 {
                return r;
            }
            let Some((family, path)) = parse_addr(&raw) else {
                return EINVAL;
            };
            if family != AF_UNIX {
                return EAFNOSUPPORT;
            }
            let socks = unsafe { table() };
            match request {
                NWIOSUDSADDR => {
                    // Fail-closed: a binder PM cannot name owns nothing.
                    let uid = caller_uid(user_ep).unwrap_or(u32::MAX);
                    bind_inner(socks, minor, uid, path)
                }
                NWIOSUDSCONN => match caller_uid(user_ep) {
                    Some(uid) => connect_inner(socks, minor, uid, path),
                    None => EACCES,
                },
                _ => accept_inner(socks, minor, path),
            }
        }
        NWIOSUDSBLOG => {
            let mut raw = [0u8; 4];
            let r = safecopy_from_grant(granter, grant, &mut raw);
            if r != 0 {
                return r;
            }
            let socks = unsafe { table() };
            listen_inner(socks, minor, i32::from_ne_bytes(raw))
        }
        NWIOGUDSADDR => {
            let socks = unsafe { table() };
            let Some(i) = index_of(socks, minor) else {
                return EBADF;
            };
            if !socks[i].bound {
                return EINVAL;
            }
            let mut raw = [0u8; SOCKADDR_UN_SIZE];
            raw[..2].copy_from_slice(&AF_UNIX.to_ne_bytes());
            let n = socks[i].path_len;
            raw[2..2 + n].copy_from_slice(&socks[i].path[..n]);
            safecopy_to_grant(granter, grant, &raw)
        }
        NWIOGUDSMINOR => {
            let socks = unsafe { table() };
            let Some(i) = index_of(socks, minor) else {
                return EBADF;
            };
            let raw = socks[i].minor.to_ne_bytes();
            safecopy_to_grant(granter, grant, &raw)
        }
        // The `sendmsg` control ioctl never reaches the driver carrying data:
        // VFS keeps the descriptors itself (`vfs::scm`) and forwards this
        // request only to learn the peer and the sender's credentials. Those
        // come back in the reply payload — the peer's clone minor, then the
        // caller's uid and gid — because there is no user buffer to carry them
        // and no second ioctl worth adding. An unconnected sender is
        // `ENOTCONN`, and a caller PM cannot name is refused, as the
        // reference's `send_fds` -> `getnucred` refuses it.
        NWIOSUDSCTRL => {
            let socks = unsafe { table() };
            let Some(i) = index_of(socks, minor) else {
                return EBADF;
            };
            let peer = socks[i].peer;
            if peer < 0 || !socks[peer as usize].in_use {
                return ENOTCONN;
            }
            let Some((_, uid, gid)) = caller_ids(socks[i].owner_ep) else {
                return ESRCH;
            };
            msg_set_i32(msg, 0, socks[peer as usize].minor);
            msg_set_i32(msg, 4, uid as i32);
            msg_set_i32(msg, 8, gid as i32);
            0
        }
        // `getsockopt(SO_PEERCRED)`: the peer's credentials, as a `uucred`.
        // C: `do_getsockopt_peercred` -> `getnucred(peer.owner, &cred)`.
        NWIOGUDSPEERCRED => {
            let socks = unsafe { table() };
            match peer_cred(socks, minor) {
                Ok(cred) => safecopy_to_grant(granter, grant, &cred),
                Err(e) => e,
            }
        }
        // The reference's libc probes a device with an ioctl and reads `ENOTTY`
        // as "not this device family" (`lib/libc/sys/bind.c`), so an unknown
        // request must answer `ENOTTY`, not `EINVAL`.
        _ => ENOTTY,
    }
}

/// Copy `dst.len()` bytes from the caller's granted buffer
/// (`SYS_SAFECOPYFROM`).
///
/// `granter` is the message sender (VFS), whose grant table holds the magic
/// grant over the user's ioctl argument; the kernel resolves the effective
/// granter (the user) from the grant entry.
fn safecopy_from_grant(granter: i32, grant: u32, dst: &mut [u8]) -> i32 {
    safe_copy(granter, grant, dst.as_mut_ptr() as u64, dst.len(), 0)
}

/// Copy `src` into the caller's granted buffer (`SYS_SAFECOPYTO`).
fn safecopy_to_grant(granter: i32, grant: u32, src: &[u8]) -> i32 {
    safe_copy(granter, grant, src.as_ptr() as u64, src.len(), 1)
}

/// A `SYS_SAFECOPYFROM` (direction 0) or `SYS_SAFECOPYTO` (direction 1) with
/// the server's address as the local side.
fn safe_copy(granter: i32, grant: u32, addr: u64, len: usize, direction: u8) -> i32 {
    #[cfg(target_os = "minix")]
    {
        // SYS_SAFECOPY message offsets (payload starts at byte 8).
        const SAFE_GRANTER_OFF: usize = 8;
        const SAFE_GRANT_ID_OFF: usize = 12;
        const SAFE_OFFSET_OFF: usize = 16;
        const SAFE_ADDR_OFF: usize = 24;
        const SAFE_BYTES_OFF: usize = 32;

        let mut kmsg = [0u8; 64];
        kmsg[SAFE_GRANTER_OFF..SAFE_GRANTER_OFF + 4].copy_from_slice(&granter.to_ne_bytes());
        kmsg[SAFE_GRANT_ID_OFF..SAFE_GRANT_ID_OFF + 4].copy_from_slice(&grant.to_ne_bytes());
        kmsg[SAFE_OFFSET_OFF..SAFE_OFFSET_OFF + 8].copy_from_slice(&0u64.to_ne_bytes());
        kmsg[SAFE_ADDR_OFF..SAFE_ADDR_OFF + 8].copy_from_slice(&addr.to_ne_bytes());
        kmsg[SAFE_BYTES_OFF..SAFE_BYTES_OFF + 8].copy_from_slice(&(len as u64).to_ne_bytes());
        // SYS_SAFECOPYFROM = 31, SYS_SAFECOPYTO = 32.
        minix_rt::kernel_call(31 + direction as i32, &mut kmsg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (granter, grant, addr, len, direction);
        0
    }
}

// ---- message accessors (payload starts at byte 8 of the raw buffer) ----

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

/// Write an `i32` into the message payload (offset 0 is its first word).
fn msg_set_i32(msg: &mut Message, off: usize, value: i32) {
    unsafe {
        msg.m_payload.raw[off..off + 4].copy_from_slice(&value.to_ne_bytes());
    }
}

/// The server main loop.
pub fn uds_server_main() {
    #[cfg(target_os = "minix")]
    {
        const ANY: i32 = 0x0000ffff;

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
            let call_type = msg.m_type as u32;

            // A notification (a peer's wake, or something the kernel relayed):
            // nothing to do. The socket path raises no alarms of its own.
            if call_type.wrapping_sub(arch_common::com::NOTIFY_MESSAGE) < 0x100 {
                continue;
            }

            if arch_common::com::is_cdev_rq(call_type) {
                let result = unsafe { handle_cdev_request(&mut msg, call_type, src as i32) };
                msg.m_type = result;
                unsafe {
                    minix_rt::syscall2(
                        minix_rt::SEND_CALL,
                        src as u64,
                        &mut msg as *mut Message as u64,
                    );
                }
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // No-op on host builds — the tests cover the pure core.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &[u8] = b"/run/user/1000/wayland-0";

    fn sockets() -> [UdsSock; NR_UDS_SOCKETS] {
        [UdsSock::empty(); NR_UDS_SOCKETS]
    }

    /// The clone minor out of an `open_inner` reply.
    fn minor_of(reply: i32) -> i32 {
        reply & 0xFFFF
    }

    fn open(s: &mut [UdsSock]) -> i32 {
        minor_of(open_inner(s))
    }

    /// A bound, listening socket owned by uid 1000.
    fn listener() -> ([UdsSock; NR_UDS_SOCKETS], i32) {
        let mut s = sockets();
        let l = open(&mut s);
        assert_eq!(bind_inner(&mut s, l, 1000, PATH), 0);
        assert_eq!(listen_inner(&mut s, l, 4), 0);
        (s, l)
    }

    /// Two opens, paired, as `socketpair(2)` leaves them.
    fn paired() -> ([UdsSock; NR_UDS_SOCKETS], usize, usize) {
        let mut s = sockets();
        let a = open(&mut s);
        let b = open(&mut s);
        assert_eq!(pair_by_minor_inner(&mut s, a, b), 0, "pairing must succeed");
        (s, index_of(&s, a).unwrap(), index_of(&s, b).unwrap())
    }

    // ---- readiness ----

    #[test]
    fn select_answers_readiness_and_holds_a_watch() {
        let (mut s, a, _b) = paired();
        let minor = s[a].minor;
        // Nothing queued and the peer open: read blocks, write does not.
        let r = select_inner(&mut s, minor, CDEV_OP_RD | CDEV_OP_WR | CDEV_NOTIFY, 7);
        assert_eq!(r, CDEV_OP_WR as i32);
        // The unready op is remembered for a late wake.
        assert_eq!(s[a].sel_ops, CDEV_OP_RD);
        assert_eq!(s[a].sel_ep, 7);
    }

    #[test]
    fn a_queued_write_wakes_a_reader_watch() {
        let (mut s, a, b) = paired();
        let minor = s[a].minor;
        assert_eq!(select_inner(&mut s, minor, CDEV_OP_RD | CDEV_NOTIFY, 9), 0);
        // b writes into a's queue; the watch is satisfied and dropped.
        let (peer, n) = write_plan(&s, b, 3).unwrap();
        commit_write(&mut s, peer, &[1, 2, 3][..n]);
        notify_select(&mut s, peer);
        assert_eq!(s[a].sel_ops, 0);
        assert_eq!(ready_ops(&s, a) & CDEV_OP_RD, CDEV_OP_RD);
    }

    #[test]
    fn a_queued_connection_wakes_a_listener_watch() {
        let (mut s, l) = listener();
        assert_eq!(select_inner(&mut s, l, CDEV_OP_RD | CDEV_NOTIFY, 11), 0);
        let client = open(&mut s);
        assert_eq!(connect_inner(&mut s, client, 0, PATH), 0);
        let li = index_of(&s, l).unwrap();
        assert_eq!(s[li].sel_ops, 0, "the listener watch is satisfied");
        assert_eq!(ready_ops(&s, li) & CDEV_OP_RD, CDEV_OP_RD);
    }

    #[test]
    fn a_peer_closing_wakes_a_reader_watch() {
        let (mut s, a, b) = paired();
        let minor = s[a].minor;
        assert_eq!(select_inner(&mut s, minor, CDEV_OP_RD | CDEV_NOTIFY, 5), 0);
        let bm = s[b].minor;
        assert_eq!(close_inner(&mut s, bm), 0);
        // a's peer is gone: readable (EOF), and the watch cleared.
        assert_eq!(s[a].sel_ops, 0);
        assert_ne!(ready_ops(&s, a) & CDEV_OP_RD, 0);
    }

    #[test]
    fn draining_a_queue_wakes_the_writer() {
        let (mut s, a, b) = paired();
        // Fill a's queue from b, so b's next write would block.
        let (peer, _) = write_plan(&s, b, UDS_BUF).unwrap();
        commit_write(&mut s, peer, &[0u8; UDS_BUF]);
        assert_eq!(ready_ops(&s, b) & CDEV_OP_WR, 0, "b's write would block");
        let bm = s[b].minor;
        assert_eq!(select_inner(&mut s, bm, CDEV_OP_WR | CDEV_NOTIFY, 3), 0);
        // a drains a byte, freeing space; the watch is satisfied.
        commit_read(&mut s, a, &mut [0u8; 1]);
        notify_select(&mut s, b);
        assert_eq!(s[b].sel_ops, 0);
        assert_ne!(ready_ops(&s, b) & CDEV_OP_WR, 0);
    }

    // ---- open / pair / close ----

    #[test]
    fn open_clones_a_fresh_minor_each_time() {
        let mut s = sockets();
        let first = open_inner(&mut s);
        let second = open_inner(&mut s);
        assert_eq!(first & 0xFFFF, UDS_SOCKET_MINOR_BASE);
        assert_eq!(second & 0xFFFF, UDS_SOCKET_MINOR_BASE + 1);
        assert_ne!(first as u32 & CDEV_CLONED, 0);
        assert_ne!(first as u32 & CDEV_DGRAM_OPEN, 0);
    }

    #[test]
    fn open_reports_emfile_when_the_table_is_full() {
        let mut s = sockets();
        for _ in 0..NR_UDS_SOCKETS {
            assert!(open_inner(&mut s) > 0);
        }
        assert_eq!(open_inner(&mut s), EMFILE);
    }

    #[test]
    fn pairing_a_socket_with_itself_is_einval() {
        let mut s = sockets();
        let a = open(&mut s);
        assert_eq!(pair_by_minor_inner(&mut s, a, a), EINVAL);
    }

    #[test]
    fn pairing_an_unknown_minor_is_ebadf() {
        let mut s = sockets();
        let a = open(&mut s);
        assert_eq!(pair_by_minor_inner(&mut s, a, 0x7FFF), EBADF);
    }

    // ---- bind / listen ----

    #[test]
    fn bind_records_the_path_and_owner() {
        let mut s = sockets();
        let m = open(&mut s);
        assert_eq!(bind_inner(&mut s, m, 1000, PATH), 0);
        let i = index_of(&s, m).unwrap();
        assert!(s[i].bound);
        assert_eq!(s[i].path(), PATH);
        assert_eq!(s[i].owner_uid, 1000);
    }

    #[test]
    fn an_empty_path_is_enoent_and_a_second_claim_is_eaddrinuse() {
        let mut s = sockets();
        let a = open(&mut s);
        let b = open(&mut s);
        assert_eq!(bind_inner(&mut s, a, 1000, b""), ENOENT);
        assert_eq!(bind_inner(&mut s, a, 1000, PATH), 0);
        assert_eq!(bind_inner(&mut s, b, 1000, PATH), EADDRINUSE);
        // Re-binding the same socket is an error even for its own path.
        assert_eq!(bind_inner(&mut s, a, 1000, PATH), EINVAL);
    }

    #[test]
    fn listen_requires_a_bound_socket_and_clamps_the_backlog() {
        let mut s = sockets();
        let m = open(&mut s);
        assert_eq!(listen_inner(&mut s, m, 4), EINVAL, "unbound");
        assert_eq!(bind_inner(&mut s, m, 1000, PATH), 0);
        assert_eq!(listen_inner(&mut s, m, 999), 0);
        let i = index_of(&s, m).unwrap();
        assert!(s[i].listening);
        assert_eq!(s[i].backlog, NR_UDS_SOCKETS);
        assert_eq!(listen_inner(&mut s, m, 1), EINVAL, "already listening");
    }

    // ---- connect / accept ----

    #[test]
    fn connect_queues_a_server_end_on_the_listener() {
        let (mut s, l) = listener();
        let c = open(&mut s);
        assert_eq!(connect_inner(&mut s, c, 1000, PATH), 0);
        let ci = index_of(&s, c).unwrap();
        let srv = s[ci].peer as usize;
        assert_eq!(s[srv].peer, ci as i32, "the pair is symmetric");
        let li = index_of(&s, l).unwrap();
        assert_eq!(s[li].acceptq_len, 1);
        assert_eq!(s[li].acceptq[0] as usize, srv);
    }

    #[test]
    fn connect_to_an_unbound_path_is_econnrefused() {
        let mut s = sockets();
        let c = open(&mut s);
        assert_eq!(connect_inner(&mut s, c, 1000, PATH), ECONNREFUSED);
    }

    #[test]
    fn connect_is_authorised_for_the_owner_and_root_only() {
        let (mut s, _l) = listener();
        let owner = open(&mut s);
        assert_eq!(connect_inner(&mut s, owner, 1000, PATH), 0);
        let root = open(&mut s);
        assert_eq!(connect_inner(&mut s, root, 0, PATH), 0);
        let other = open(&mut s);
        assert_eq!(connect_inner(&mut s, other, 2000, PATH), EACCES);
    }

    #[test]
    fn a_full_backlog_refuses_a_connection() {
        let mut s = sockets();
        let l = open(&mut s);
        assert_eq!(bind_inner(&mut s, l, 1000, PATH), 0);
        assert_eq!(listen_inner(&mut s, l, 1), 0);
        let first = open(&mut s);
        assert_eq!(connect_inner(&mut s, first, 1000, PATH), 0);
        let second = open(&mut s);
        assert_eq!(connect_inner(&mut s, second, 1000, PATH), ECONNREFUSED);
    }

    #[test]
    fn connect_twice_on_one_socket_is_eisconn() {
        let (mut s, _l) = listener();
        let c = open(&mut s);
        assert_eq!(connect_inner(&mut s, c, 1000, PATH), 0);
        assert_eq!(connect_inner(&mut s, c, 1000, PATH), EISCONN);
    }

    #[test]
    fn accept_with_nothing_queued_is_eagain() {
        let (mut s, _l) = listener();
        let fresh = open(&mut s);
        assert_eq!(accept_inner(&mut s, fresh, PATH), EAGAIN);
    }

    #[test]
    fn accept_rehomes_the_queued_end_and_data_flows() {
        let (mut s, l) = listener();
        let c = open(&mut s);
        assert_eq!(connect_inner(&mut s, c, 1000, PATH), 0);
        let accepted = open(&mut s);
        assert_eq!(accept_inner(&mut s, accepted, PATH), 0);

        let ai = index_of(&s, accepted).unwrap();
        let ci = index_of(&s, c).unwrap();
        assert_eq!(s[ai].peer, ci as i32, "accepted end sees the client");
        assert_eq!(s[ci].peer, ai as i32, "client sees the accepted end");
        // The queue drained, and the accepted socket kept its own minor.
        let li = index_of(&s, l).unwrap();
        assert_eq!(s[li].acceptq_len, 0);
        assert_eq!(s[ai].minor, accepted);

        // Bytes written by the client are read by the accepted end.
        let (peer, _) = write_plan(&s, ci, 5).unwrap();
        assert_eq!(peer, ai);
        commit_write(&mut s, peer, b"hello");
        let mut out = [0u8; 5];
        let n = read_plan(&s, ai, out.len()).unwrap();
        assert_eq!(commit_read(&mut s, ai, &mut out[..n]), 5);
        assert_eq!(&out, b"hello");
    }

    #[test]
    fn connect_after_accept_reuses_the_table_cleanly() {
        // The queued-end slot is freed by accept, so a second connection can
        // take it — with no stale peer pointing anywhere.
        let (mut s, _l) = listener();
        let c1 = open(&mut s);
        assert_eq!(connect_inner(&mut s, c1, 1000, PATH), 0);
        let a1 = open(&mut s);
        assert_eq!(accept_inner(&mut s, a1, PATH), 0);
        let c2 = open(&mut s);
        assert_eq!(connect_inner(&mut s, c2, 1000, PATH), 0);
        let a2 = open(&mut s);
        assert_eq!(accept_inner(&mut s, a2, PATH), 0);
        assert_ne!(a1, a2);
        assert_eq!(
            s[index_of(&s, a2).unwrap()].peer,
            index_of(&s, c2).unwrap() as i32
        );
    }

    // ---- read / write / close ----

    #[test]
    fn a_write_reaches_the_peers_read_queue() {
        let (mut s, a, b) = paired();
        let (peer, n) = write_plan(&s, a, 5).expect("a paired write is writable");
        assert_eq!(peer, b);
        assert_eq!(n, 5);
        commit_write(&mut s, peer, b"hello");

        let mut out = [0u8; 5];
        let taken = read_plan(&s, b, out.len()).expect("data is queued");
        assert_eq!(taken, 5);
        assert_eq!(commit_read(&mut s, b, &mut out), 5);
        assert_eq!(&out, b"hello");
        assert_eq!(read_plan(&s, b, 5), Err(EAGAIN), "drained");
    }

    #[test]
    fn a_short_write_moves_what_fits_and_the_rest_is_eagain() {
        let (mut s, a, b) = paired();
        let chunk = [7u8; UDS_BUF];
        let (peer, n) = write_plan(&s, a, UDS_BUF + 100).expect("space at first");
        assert_eq!(n, UDS_BUF);
        commit_write(&mut s, peer, &chunk[..n]);
        assert_eq!(write_plan(&s, a, 1), Err(EAGAIN));
        assert_eq!(s[b].rx_len, UDS_BUF);
        assert_eq!(s[a].rx_len, 0);
    }

    #[test]
    fn a_partial_read_leaves_the_tail_queued() {
        let (mut s, a, b) = paired();
        let (peer, _) = write_plan(&s, a, 10).unwrap();
        commit_write(&mut s, peer, b"0123456789");
        let mut out = [0u8; 4];
        assert_eq!(commit_read(&mut s, b, &mut out), 4);
        assert_eq!(&out, b"0123");
        let mut rest = [0u8; 6];
        let n = read_plan(&s, b, rest.len()).unwrap();
        assert_eq!(commit_read(&mut s, b, &mut rest[..n]), 6);
        assert_eq!(&rest, b"456789");
    }

    #[test]
    fn an_unpaired_socket_reads_eof_and_writes_epipe() {
        let mut s = sockets();
        let minor = open(&mut s);
        let a = index_of(&s, minor).unwrap();
        assert_eq!(read_plan(&s, a, 8), Ok(0));
        assert_eq!(write_plan(&s, a, 8), Err(EPIPE));
    }

    #[test]
    fn closing_the_client_detaches_the_queued_end() {
        // The client hung up before the server accepted: the queued end is
        // handed over detached, so the server sees EOF rather than a peer.
        let (mut s, _l) = listener();
        let c = open(&mut s);
        assert_eq!(connect_inner(&mut s, c, 1000, PATH), 0);
        assert_eq!(close_inner(&mut s, c), 0);
        let accepted = open(&mut s);
        assert_eq!(accept_inner(&mut s, accepted, PATH), 0);
        let ai = index_of(&s, accepted).unwrap();
        assert_eq!(s[ai].peer, -1);
        assert_eq!(read_plan(&s, ai, 8), Ok(0), "end-of-input");
    }

    #[test]
    fn closing_the_listener_resets_unaccepted_connections() {
        let (mut s, l) = listener();
        let c = open(&mut s);
        assert_eq!(connect_inner(&mut s, c, 1000, PATH), 0);
        assert_eq!(close_inner(&mut s, l), 0);
        // The client's peer is gone, so it sees EOF.
        let ci = index_of(&s, c).unwrap();
        assert_eq!(s[ci].peer, -1);
        assert_eq!(read_plan(&s, ci, 8), Ok(0));
    }

    #[test]
    fn closing_one_end_gives_the_other_eof_and_epipe() {
        let (mut s, a, b) = paired();
        let b_minor = s[b].minor;
        assert_eq!(close_inner(&mut s, b_minor), 0);
        assert_eq!(read_plan(&s, a, 8), Ok(0));
        assert_eq!(write_plan(&s, a, 8), Err(EPIPE));
        assert!(!s[b].in_use);
    }

    #[test]
    fn a_closed_socket_is_ebadf() {
        let (mut s, _a, b) = paired();
        let b_minor = s[b].minor;
        assert_eq!(close_inner(&mut s, b_minor), 0);
        assert_eq!(close_inner(&mut s, b_minor), EBADF);
        assert_eq!(read_plan(&s, b, 1), Err(EBADF));
    }

    #[test]
    fn queued_data_survives_the_peer_closing() {
        let (mut s, a, b) = paired();
        let a_minor = s[a].minor;
        let (peer, _) = write_plan(&s, a, 4).unwrap();
        commit_write(&mut s, peer, b"last");
        assert_eq!(close_inner(&mut s, a_minor), 0);

        let mut out = [0u8; 8];
        let n = read_plan(&s, b, out.len()).unwrap();
        assert_eq!(commit_read(&mut s, b, &mut out[..n]), 4);
        assert_eq!(&out[..4], b"last");
        assert_eq!(read_plan(&s, b, 8), Ok(0), "then EOF");
    }

    #[test]
    fn a_reused_slot_gets_a_clean_socket() {
        let (mut s, a, b) = paired();
        let b_minor = s[b].minor;
        let (peer, _) = write_plan(&s, a, 4).unwrap();
        commit_write(&mut s, peer, b"data");
        assert_eq!(close_inner(&mut s, b_minor), 0);
        let reused = open(&mut s);
        assert_eq!(reused, b_minor);
        assert_eq!(s[b].rx_len, 0);
        assert_eq!(s[b].peer, -1);
        assert!(s[b].in_use);
    }

    // ---- addressing ----

    #[test]
    fn a_sockaddr_un_parses_to_family_and_path() {
        let mut raw = [0u8; SOCKADDR_UN_SIZE];
        raw[..2].copy_from_slice(&AF_UNIX.to_ne_bytes());
        raw[2..2 + PATH.len()].copy_from_slice(PATH);
        let (family, path) = parse_addr(&raw).unwrap();
        assert_eq!(family, AF_UNIX);
        assert_eq!(path, PATH);
        // An unterminated path runs to the end of the field.
        let mut full = [0x41u8; SOCKADDR_UN_SIZE];
        full[..2].copy_from_slice(&AF_UNIX.to_ne_bytes());
        assert_eq!(parse_addr(&full).unwrap().1.len(), UDS_PATH_MAX);
    }

    // ---- peer identity ----

    #[test]
    fn peer_cred_needs_a_named_peer() {
        let (mut s, a, b) = paired();
        let a_minor = s[a].minor;
        // The peer has recorded no owner, so it cannot be named.
        assert_eq!(peer_cred(&s, a_minor), Err(ENOTCONN));
        // With an owner present the host cannot reach PM, so it reports ESRCH
        // rather than inventing credentials.
        s[b].owner_ep = 7;
        assert_eq!(peer_cred(&s, a_minor), Err(ESRCH));
        // And an unpaired socket has no peer to ask about at all, while an
        // unknown minor is simply EBADF.
        let unpaired = open(&mut s);
        assert_eq!(peer_cred(&s, unpaired), Err(ENOTCONN));
        assert_eq!(peer_cred(&s, 0x7fff), Err(EBADF));
    }

    #[test]
    fn accept_moves_the_connection_under_the_acceptor_owner() {
        let (mut s, l) = listener();
        let client = open(&mut s);
        assert_eq!(connect_inner(&mut s, client, 1000, PATH), 0);
        // The client owns its end; the queued server end has no owner yet.
        s[index_of(&s, client).unwrap()].owner_ep = 42;
        let queued = s[index_of(&s, l).unwrap()].acceptq[0] as usize;
        assert_eq!(s[queued].owner_ep, -1);

        let accepted = open(&mut s);
        let dst = index_of(&s, accepted).unwrap();
        s[dst].owner_ep = 77; // the acceptor's ioctl recorded it
        assert_eq!(accept_inner(&mut s, accepted, PATH), 0);
        assert_eq!(s[dst].owner_ep, 77, "the acceptor owns the accepted end");
        // The connection is intact: the client's peer is the accepted socket.
        assert_eq!(s[index_of(&s, client).unwrap()].peer as usize, dst);
        assert_eq!(s[dst].peer, index_of(&s, client).unwrap() as i32);
    }
}
