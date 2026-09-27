//! UNIX-domain socket client API over `/dev/uds`.
//!
//! The wire protocol is the `net` crate's `NWIO*` codes and `sockaddr_un`
//! (`net::uds`); the data path is plain `read`/`write` on the fd, which VFS
//! routes to the `uds` server because the open reply marks the filp as a
//! datagram channel.
//!
//! `accept` follows the reference's `_uds_accept` shape: the queued connection
//! is handed to a **fresh** socket, so the wrapper opens one and names the
//! listener in the ioctl.
//!
//! Sockets are non-blocking: `connect` completes at once (queuing the connection
//! on the listener) and `accept` answers `EAGAIN` when nothing is queued, so a
//! caller retries rather than waits (`WAYLAND.md` §6.1). `accept` returning
//! `EAGAIN` is not an error to give up on — poll and retry.

use crate::fs::{O_RDWR, close, ioctl, open, read, write};
use crate::{EINVAL, EOVERFLOW, MinixErr};
use net::{
    MSG_CONTROL_LEN_OFF, MSG_CONTROL_SIZE, NWIOGUDSADDR, NWIOGUDSCTRL, NWIOGUDSMINOR,
    NWIOGUDSPEERCRED, NWIOSUDSACCEPT, NWIOSUDSADDR, NWIOSUDSBLOG, NWIOSUDSCONN, NWIOSUDSCTRL,
    NWIOSUDSPAIR, SOCKADDR_UN_SIZE, SockAddrUn, UUCRED_GID_OFF, UUCRED_SIZE, UUCRED_UID_OFF,
};

/// The device every local-domain socket is an instance of.
pub const UDS_DEVICE: &[u8] = b"/dev/uds";

/// A `sockaddr_un` as it travels over the wire.
fn wire(addr: &SockAddrUn) -> [u8; SOCKADDR_UN_SIZE] {
    let mut b = [0u8; SOCKADDR_UN_SIZE];
    b[..2].copy_from_slice(&addr.sun_family.to_ne_bytes());
    b[2..].copy_from_slice(&addr.sun_path);
    b
}

/// `socket(AF_UNIX, SOCK_STREAM, 0)`: open a local-domain socket.
pub fn socket() -> Result<i32, MinixErr> {
    unsafe { open(UDS_DEVICE, O_RDWR, 0) }
}

/// The socket's own clone minor, as the driver knows it.
///
/// Not a reference ioctl — see [`socketpair`] for why `fstat` cannot supply it.
pub fn socket_minor(fd: i32) -> Result<i32, MinixErr> {
    let mut b = [0u8; 4];
    unsafe { ioctl(fd, NWIOGUDSMINOR, b.as_mut_ptr()) }?;
    Ok(i32::from_ne_bytes(b))
}

/// `socketpair(2)`: two connected sockets.
///
/// The pair ioctl names the peer by its device number. The reference reads that
/// from `fstat(sv[1]).st_dev`, which works there because MINIX's VFS gives each
/// clone open its own node and so the stat names the clone; this port has no
/// clone nodes (`KNOWN_ISSUES.md` 28), so the peer's minor comes from the driver
/// instead ([`socket_minor`]).
pub fn socketpair() -> Result<(i32, i32), MinixErr> {
    let a = socket()?;
    let b = match socket() {
        Ok(fd) => fd,
        Err(e) => {
            let _ = close(a);
            return Err(e);
        }
    };
    let mut peer = match socket_minor(b) {
        Ok(m) => m.to_ne_bytes(),
        Err(e) => {
            let _ = close(a);
            let _ = close(b);
            return Err(e);
        }
    };
    match unsafe { ioctl(a, NWIOSUDSPAIR, peer.as_mut_ptr()) } {
        Ok(_) => Ok((a, b)),
        Err(e) => {
            let _ = close(a);
            let _ = close(b);
            Err(e)
        }
    }
}

/// `bind(2)`: name the socket `path`.
pub fn bind(fd: i32, path: &[u8]) -> Result<(), MinixErr> {
    let addr = SockAddrUn::new(path).ok_or(MinixErr::from_i32(EINVAL))?;
    let mut b = wire(&addr);
    unsafe { ioctl(fd, NWIOSUDSADDR, b.as_mut_ptr()) }.map(|_| ())
}

/// `listen(2)`: accept connections, with at most `backlog` waiting unaccepted.
pub fn listen(fd: i32, backlog: i32) -> Result<(), MinixErr> {
    let mut b = backlog.to_ne_bytes();
    unsafe { ioctl(fd, NWIOSUDSBLOG, b.as_mut_ptr()) }.map(|_| ())
}

/// `connect(2)`: join the listening socket bound to `path`.
pub fn connect(fd: i32, path: &[u8]) -> Result<(), MinixErr> {
    let addr = SockAddrUn::new(path).ok_or(MinixErr::from_i32(EINVAL))?;
    let mut b = wire(&addr);
    unsafe { ioctl(fd, NWIOSUDSCONN, b.as_mut_ptr()) }.map(|_| ())
}

/// `getsockname(2)`: the socket's bound name, as wire bytes.
pub fn getsockname(fd: i32) -> Result<[u8; SOCKADDR_UN_SIZE], MinixErr> {
    let mut b = [0u8; SOCKADDR_UN_SIZE];
    unsafe { ioctl(fd, NWIOGUDSADDR, b.as_mut_ptr()) }?;
    Ok(b)
}

/// `accept(2)`: the next queued connection on `listen_fd`, as a new socket.
///
/// `EAGAIN` when nothing is queued yet.
pub fn accept(listen_fd: i32) -> Result<i32, MinixErr> {
    // Name the listener, then hand its queued connection to a fresh socket —
    // the reference's `_uds_accept` opens one for exactly this reason.
    let mut addr = getsockname(listen_fd)?;
    let newfd = socket()?;
    match unsafe { ioctl(newfd, NWIOSUDSACCEPT, addr.as_mut_ptr()) } {
        Ok(_) => Ok(newfd),
        Err(e) => {
            let _ = close(newfd);
            Err(e)
        }
    }
}

/// `write(2)` on a socket.
pub fn send(fd: i32, buf: &[u8]) -> Result<i64, MinixErr> {
    unsafe { write(fd, buf) }
}

/// `sendmsg(2)` with an `SCM_RIGHTS` list: hand `fds` to the socket's peer.
///
/// The control data travels as one `NWIOSUDSCTRL` ioctl issued *before* the
/// data write, which is the reference's split (`lib/libc/sys/sendmsg.c`); a
/// caller follows it with a normal [`send`] for the message body. The peer
/// receives descriptors for the same open files, not copies of their contents.
pub fn send_fds(fd: i32, fds: &[i32]) -> Result<(), MinixErr> {
    if fds.is_empty() {
        return Ok(());
    }
    let mut ctrl = [0u8; MSG_CONTROL_SIZE];
    let controllen = net::build_control(&mut ctrl, fds, None);
    if controllen == 0 {
        return Err(MinixErr::from_i32(EINVAL));
    }
    ctrl[MSG_CONTROL_LEN_OFF..MSG_CONTROL_LEN_OFF + 4]
        .copy_from_slice(&(controllen as u32).to_ne_bytes());
    unsafe { ioctl(fd, NWIOSUDSCTRL, ctrl.as_mut_ptr()) }.map(|_| ())
}

/// `recvmsg(2)` control data: collect the descriptors the peer passed, and
/// return how many landed in `fds`.
///
/// The reference lifts control out with `NWIOGUDSCTRL` *after* the data read
/// (`lib/libc/sys/recvmsg.c`), so read the body first and call this after.
/// Credentials are not offered room, so [`recv_fds_cred`] is the call to make
/// when the sender's identity matters.
pub fn recv_fds(fd: i32, fds: &mut [i32]) -> Result<usize, MinixErr> {
    recv_control(fd, fds, false).map(|(n, _)| n)
}

/// `recvmsg(2)` control data including the sender's credentials.
///
/// Returns the descriptor count and the credentials of whatever process last
/// sent on this socket — the same pair `SO_PEERCRED` reports — or `None` when
/// the sender recorded none (see `WAYLAND.md` §6.2). Room for the credentials
/// is offered on top of `fds.len()` descriptors, so a caller wanting both must
/// leave space for the descriptors it expects.
pub fn recv_fds_cred(fd: i32, fds: &mut [i32]) -> Result<(usize, Option<(u32, u32)>), MinixErr> {
    recv_control(fd, fds, true)
}

/// The shared body of [`recv_fds`] and [`recv_fds_cred`].
fn recv_control(
    fd: i32,
    fds: &mut [i32],
    want_cred: bool,
) -> Result<(usize, Option<(u32, u32)>), MinixErr> {
    let mut ctrl = [0u8; MSG_CONTROL_SIZE];
    // Offer exactly the room the caller's slice can hold, plus the credentials
    // message when they are wanted: the reply carries only what fits, and
    // answering `EOVERFLOW` would leave the descriptors pending.
    let mut offer = net::cmsg_space(4 * fds.len());
    if want_cred {
        offer += net::cmsg_space(UUCRED_SIZE);
    }
    if offer > net::MSG_CONTROL_MAX {
        return Err(MinixErr::from_i32(EINVAL));
    }
    ctrl[MSG_CONTROL_LEN_OFF..MSG_CONTROL_LEN_OFF + 4]
        .copy_from_slice(&(offer as u32).to_ne_bytes());
    unsafe { ioctl(fd, NWIOGUDSCTRL, ctrl.as_mut_ptr()) }?;
    let controllen = u32::from_ne_bytes(
        ctrl[MSG_CONTROL_LEN_OFF..MSG_CONTROL_LEN_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    ) as usize;
    let n = net::parse_rights(&ctrl, controllen, fds).map_err(|_| MinixErr::from_i32(EOVERFLOW))?;
    Ok((n, net::parse_creds(&ctrl, controllen)))
}

/// `getsockopt(SO_PEERCRED)`: the peer's effective uid and gid.
///
/// The reference serves this from the peer's recorded owner
/// (`ioc_uds.c` `do_getsockopt_peercred`), and it is how a Wayland or D-Bus
/// server checks who it is talking to.
pub fn peer_cred(fd: i32) -> Result<(u32, u32), MinixErr> {
    let mut cred = [0u8; UUCRED_SIZE];
    unsafe { ioctl(fd, NWIOGUDSPEERCRED, cred.as_mut_ptr()) }?;
    let uid = u32::from_ne_bytes(
        cred[UUCRED_UID_OFF..UUCRED_UID_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    );
    let gid = u32::from_ne_bytes(
        cred[UUCRED_GID_OFF..UUCRED_GID_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    );
    Ok((uid, gid))
}

/// `read(2)` on a socket.
pub fn recv(fd: i32, buf: &mut [u8]) -> Result<i64, MinixErr> {
    unsafe { read(fd, buf) }
}

/// `close(2)`.
pub fn close_fd(fd: i32) -> Result<(), MinixErr> {
    close(fd)
}
