//! UNIX-domain socket wire types and ioctl codes (`/dev/uds`).
//!
//! The reference configures its UDS driver with `NWIO*` ioctls on the fd,
//! exactly as the INET driver is configured (`minix/drivers/net/uds/ioc_uds.c`,
//! `include/sys/ioc_net.h`): `socket(2)` is `open("/dev/uds")` plus a type
//! ioctl, `bind`/`listen`/`connect`/`accept` are further ioctls carrying a
//! `sockaddr_un`, and `socketpair(2)` is two opens plus `NWIOSUDSPAIR`.
//!
//! These follow the same NetBSD `_IOW`/`_IOR` encoding as the `NWIO*` codes in
//! `nwio.rs`/`tcp.rs`, so [`crate::ioc_size`]/[`crate::ioc_is_in`] — which read
//! the generic encoding rather than a per-code table — size the argument copy
//! VFS makes for an ioctl. The *numbers* are this port's own, not the
//! reference's, so they are free to change until a reference-built binary has
//! to interoperate with them.

use crate::{IOC_IN, IOC_INOUT, IOC_OUT, ioc_encode};

/// Address family value for local sockets (reference `sys/socket.h`).
pub const AF_UNIX: u16 = 1;

/// Longest path a `sockaddr_un` can carry (reference `sys/un.h`).
pub const UDS_PATH_MAX: usize = 108;

/// `struct sockaddr_un`: a 16-bit family and a 108-byte path.
///
/// The port's own definition rather than a borrowed header, but the layout is
/// the reference's — VFS copies exactly [`SOCKADDR_UN_SIZE`] bytes for an ioctl
/// that carries one, which is what keeps the client and the driver agreeing.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SockAddrUn {
    pub sun_family: u16,
    pub sun_path: [u8; UDS_PATH_MAX],
}

impl SockAddrUn {
    /// An address for `path` in the local family. The path must fit.
    pub fn new(path: &[u8]) -> Option<Self> {
        if path.len() > UDS_PATH_MAX {
            return None;
        }
        let mut a = Self {
            sun_family: AF_UNIX,
            sun_path: [0u8; UDS_PATH_MAX],
        };
        a.sun_path[..path.len()].copy_from_slice(path);
        Some(a)
    }

    /// The path, up to the first NUL or the end of the field.
    pub fn path(&self) -> &[u8] {
        let end = self
            .sun_path
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(UDS_PATH_MAX);
        &self.sun_path[..end]
    }

    /// The address as it travels over the wire (VFS copies this many bytes).
    pub fn as_bytes(&self) -> &[u8] {
        // Safety: `SockAddrUn` is `repr(C)` plain data.
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }
}

/// Bytes of a [`SockAddrUn`] — the size VFS copies for an ioctl carrying one.
pub const SOCKADDR_UN_SIZE: usize = core::mem::size_of::<SockAddrUn>();

/// `NWIOSUDSPAIR` — pair this socket with the socket whose device number is
/// passed as the argument. The reference's `lib/libc/sys/socketpair.c` forms
/// the argument from `fstat(sv[1]).st_rdev`, so this port does the same.
pub const NWIOSUDSPAIR: u32 = ioc_encode(IOC_IN, b'u', 1, 4);
/// `NWIOSUDSADDR` — `bind`: name this socket. Arg: a [`SockAddrUn`].
pub const NWIOSUDSADDR: u32 = ioc_encode(IOC_IN, b'u', 2, SOCKADDR_UN_SIZE);
/// `NWIOGUDSADDR` — `getsockname`: the socket's bound name. Arg: a [`SockAddrUn`].
pub const NWIOGUDSADDR: u32 = ioc_encode(IOC_OUT, b'u', 3, SOCKADDR_UN_SIZE);
/// `NWIOSUDSBLOG` — `listen`: begin accepting, with an `i32` backlog.
pub const NWIOSUDSBLOG: u32 = ioc_encode(IOC_IN, b'u', 4, 4);
/// `NWIOSUDSCONN` — `connect`: join a listening socket. Arg: a [`SockAddrUn`].
pub const NWIOSUDSCONN: u32 = ioc_encode(IOC_IN, b'u', 5, SOCKADDR_UN_SIZE);
/// `NWIOSUDSACCEPT` — `accept`: pop a queued connection onto this socket. Arg:
/// the *listener's* [`SockAddrUn`], as the reference's `_uds_accept` passes.
pub const NWIOSUDSACCEPT: u32 = ioc_encode(IOC_IN, b'u', 6, SOCKADDR_UN_SIZE);
/// `NWIOGUDSMINOR` — this socket's own clone minor (`i32`). Not a reference
/// ioctl: the reference's `socketpair.c` takes the peer's minor from
/// `fstat(sv[1]).st_dev`, which names the *clone* only because MINIX's VFS
/// gives every clone open its own temporary node (`cdev_clone` → PFS
/// `newnode`, whose inode's `i_dev` is the clone device). This port has no clone
/// nodes, so the driver reports its own minor instead (`KNOWN_ISSUES.md` 28).
pub const NWIOGUDSMINOR: u32 = ioc_encode(IOC_OUT, b'u', 7, 4);
/// `NWIOGUDSPEERCRED` — `getsockopt(SO_PEERCRED)`: the peer's credentials, also
/// the reference's `NWIOGUDSPEERCRED` (`include/sys/ioc_net.h`), carrying a
/// [`UUCRED_SIZE`]-byte `struct uucred`.
pub const NWIOGUDSPEERCRED: u32 = ioc_encode(IOC_OUT, b'u', 9, UUCRED_SIZE);

// ---- ancillary data (sendmsg/recvmsg control) ----
//
// The reference's `sendmsg(2)`/`recvmsg(2)` split the control data out of the
// data path: control travels in one `NWIOSUDSCTRL` ioctl issued *before* the
// data write, and `recvmsg` lifts it back out with `NWIOGUDSCTRL` *after* the
// read (`lib/libc/sys/sendmsg.c`, `recvmsg.c`). These two codes keep that
// shape; what differs is *who* acts on them — see the `msg_control` note.

/// `MSG_CONTROL_MAX` — the control byte area of a [`MsgControl`]
/// (`sys/ioc_net.h`: `1024 - sizeof(socklen_t)`).
pub const MSG_CONTROL_MAX: usize = 1024 - 4;
/// `sizeof(struct msg_control)`: the control area plus the trailing `socklen_t`.
pub const MSG_CONTROL_SIZE: usize = 1024;
/// Offset of `msg_controllen` within the [`MsgControl`] wire layout.
pub const MSG_CONTROL_LEN_OFF: usize = MSG_CONTROL_MAX;

/// `SOL_SOCKET` (`sys/sys/socket.h`).
pub const SOL_SOCKET: i32 = 0xffff;
/// `SCM_RIGHTS` — the control type carrying an array of descriptors.
pub const SCM_RIGHTS: i32 = 0x01;
/// `SCM_CREDS` — the control type carrying a `struct sockcred`.
pub const SCM_CREDS: i32 = 0x04;

/// `sizeof(struct cmsghdr)`: `socklen_t cmsg_len` plus two `int`s.
pub const CMSG_HDR_SIZE: usize = 12;
/// The CMSG alignment MINIX uses: `__ALIGNBYTES == sizeof(int) - 1` on x86
/// (`sys/arch/i386/include/cdefs.h`). `cmsghdr` is already 12 bytes, so the
/// aligned data offset happens to equal the header size here.
pub const CMSG_ALIGNBYTES: usize = 3;
/// Offset of `cmsg_data` within a `cmsghdr` (`CMSG_DATA`).
pub const CMSG_DATA_OFF: usize = (CMSG_HDR_SIZE + CMSG_ALIGNBYTES) & !CMSG_ALIGNBYTES;
/// Offsets of the `cmsghdr` fields.
pub const CMSG_LEN_OFF: usize = 0;
pub const CMSG_LEVEL_OFF: usize = 4;
pub const CMSG_TYPE_OFF: usize = 8;

/// `CMSG_ALIGN(n)` — round `n` up to the CMSG alignment.
pub const fn cmsg_align(n: usize) -> usize {
    (n + CMSG_ALIGNBYTES) & !CMSG_ALIGNBYTES
}

/// `CMSG_LEN(n)` — a header plus `n` payload bytes, *not* rounded up.
pub const fn cmsg_len(n: usize) -> usize {
    CMSG_DATA_OFF + n
}

/// `CMSG_SPACE(n)` — a header plus `n` payload bytes, rounded up.
pub const fn cmsg_space(n: usize) -> usize {
    CMSG_DATA_OFF + cmsg_align(n)
}

/// `NGROUPS_MAX` (`sys/sys/syslimits.h`).
pub const NGROUPS_MAX: usize = 16;
/// `sizeof(struct uucred)` (`sys/sys/ucred.h`): `cr_unused` (2, padded), a
/// 4-byte `cr_uid`, a 4-byte `cr_gid`, `cr_ngroups` (2, padded), then
/// `cr_groups[NGROUPS_MAX]`.
pub const UUCRED_SIZE: usize = 16 + 4 * NGROUPS_MAX;
/// Offsets of the `uucred` fields this port fills.
pub const UUCRED_UID_OFF: usize = 4;
pub const UUCRED_GID_OFF: usize = 8;

/// `NWIOSUDSCTRL` — `sendmsg` control data. The argument is a [`MsgControl`]
/// whose length field says how much control data the caller supplied.
///
/// VFS intercepts this one: it keeps the descriptors itself (`vfs::scm`) and
/// forwards the request only to learn which socket is the peer, which the
/// driver answers in the reply payload rather than in a user buffer. See
/// `WAYLAND.md` §5.
pub const NWIOSUDSCTRL: u32 = ioc_encode(IOC_IN, b'u', 10, MSG_CONTROL_SIZE);
/// `NWIOGUDSCTRL` — `recvmsg` control data. Bidirectional: the caller's
/// `msg_controllen` is the space available, and the reply's is the amount
/// written back.
pub const NWIOGUDSCTRL: u32 = ioc_encode(IOC_INOUT, b'u', 11, MSG_CONTROL_SIZE);

/// Why a control buffer could not be decoded into its descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmsgError {
    /// A header's `cmsg_len` is smaller than a header, or runs past the
    /// control data the caller declared.
    BadHeader,
    /// The buffer holds more descriptors than the caller's array can take.
    TooMany,
}

/// Collect the descriptors carried by every `SCM_RIGHTS` message in `ctrl`,
/// which holds `controllen` bytes of control data, into `out`.
///
/// The chain is walked the way `CMSG_FIRSTHDR`/`CMSG_NXTHDR` walk it: each
/// header's `cmsg_len` includes the header itself, and the next header starts
/// at the aligned end of that one. Control messages of other kinds are skipped.
pub fn parse_rights(ctrl: &[u8], controllen: usize, out: &mut [i32]) -> Result<usize, CmsgError> {
    let limit = controllen.min(ctrl.len()).min(MSG_CONTROL_MAX);
    let mut off = 0usize;
    let mut n = 0usize;
    while off + CMSG_HDR_SIZE <= limit {
        let clen = read_u32(ctrl, off + CMSG_LEN_OFF) as usize;
        if clen < CMSG_HDR_SIZE || off + clen > limit {
            return Err(CmsgError::BadHeader);
        }
        let level = read_u32(ctrl, off + CMSG_LEVEL_OFF) as i32;
        let ctype = read_u32(ctrl, off + CMSG_TYPE_OFF) as i32;
        if level == SOL_SOCKET && ctype == SCM_RIGHTS {
            let mut at = off + CMSG_DATA_OFF;
            let end = off + clen;
            while at + 4 <= end {
                if n == out.len() {
                    return Err(CmsgError::TooMany);
                }
                out[n] = read_u32(ctrl, at) as i32;
                n += 1;
                at += 4;
            }
        }
        off += cmsg_space(clen - CMSG_HDR_SIZE);
    }
    Ok(n)
}

/// Write one `SCM_RIGHTS` message carrying `fds`, then an `SCM_CREDS` message
/// carrying `cred` when there is one, into `ctrl`.
///
/// That is the layout the reference's `do_recvmsg` produces: `recv_fds` fills
/// the first message and `recv_cred` appends the second (and uses the first
/// header itself when there were no descriptors). Returns the `msg_controllen`
/// a reader should be given, or 0 when nothing fits.
pub fn build_control(ctrl: &mut [u8], fds: &[i32], cred: Option<&[u8; UUCRED_SIZE]>) -> usize {
    let mut at = 0usize;
    if !fds.is_empty() {
        let space = cmsg_space(4 * fds.len());
        if space > ctrl.len() {
            return 0;
        }
        ctrl[..space].fill(0);
        write_u32(ctrl, CMSG_LEN_OFF, cmsg_len(4 * fds.len()) as u32);
        write_u32(ctrl, CMSG_LEVEL_OFF, SOL_SOCKET as u32);
        write_u32(ctrl, CMSG_TYPE_OFF, SCM_RIGHTS as u32);
        for (k, &fd) in fds.iter().enumerate() {
            write_u32(ctrl, CMSG_DATA_OFF + 4 * k, fd as u32);
        }
        at = space;
    }
    if let Some(cred) = cred {
        let space = cmsg_space(UUCRED_SIZE);
        if at + space > ctrl.len() {
            return at;
        }
        ctrl[at..at + space].fill(0);
        write_u32(ctrl, at + CMSG_LEN_OFF, cmsg_len(UUCRED_SIZE) as u32);
        write_u32(ctrl, at + CMSG_LEVEL_OFF, SOL_SOCKET as u32);
        write_u32(ctrl, at + CMSG_TYPE_OFF, SCM_CREDS as u32);
        ctrl[at + CMSG_DATA_OFF..at + CMSG_DATA_OFF + UUCRED_SIZE].copy_from_slice(cred);
        at += space;
    }
    at
}

/// A `struct uucred` with only `cr_uid` and `cr_gid` filled, which is all the
/// reference's `getnucred` fills — the group list stays empty.
pub fn uucred(uid: u32, gid: u32) -> [u8; UUCRED_SIZE] {
    let mut cred = [0u8; UUCRED_SIZE];
    cred[UUCRED_UID_OFF..UUCRED_UID_OFF + 4].copy_from_slice(&uid.to_ne_bytes());
    cred[UUCRED_GID_OFF..UUCRED_GID_OFF + 4].copy_from_slice(&gid.to_ne_bytes());
    cred
}

/// The uid and gid of the first `SCM_CREDS` message in `ctrl`, which holds
/// `controllen` bytes of control data, if it carries one.
pub fn parse_creds(ctrl: &[u8], controllen: usize) -> Option<(u32, u32)> {
    let limit = controllen.min(ctrl.len()).min(MSG_CONTROL_MAX);
    let mut off = 0usize;
    while off + CMSG_HDR_SIZE <= limit {
        let clen = read_u32(ctrl, off + CMSG_LEN_OFF) as usize;
        if clen < CMSG_HDR_SIZE || off + clen > limit {
            return None;
        }
        if read_u32(ctrl, off + CMSG_LEVEL_OFF) as i32 == SOL_SOCKET
            && read_u32(ctrl, off + CMSG_TYPE_OFF) as i32 == SCM_CREDS
            && clen >= CMSG_DATA_OFF + UUCRED_SIZE
        {
            let uid = read_u32(ctrl, off + CMSG_DATA_OFF + UUCRED_UID_OFF);
            let gid = read_u32(ctrl, off + CMSG_DATA_OFF + UUCRED_GID_OFF);
            return Some((uid, gid));
        }
        off += cmsg_space(clen - CMSG_HDR_SIZE);
    }
    None
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(
        buf.get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 4]),
    )
}

fn write_u32(buf: &mut [u8], at: usize, value: u32) {
    if let Some(field) = buf.get_mut(at..at + 4) {
        field.copy_from_slice(&value.to_ne_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ioc_is_in, ioc_is_out, ioc_size};

    #[test]
    fn sockaddr_un_is_family_then_path() {
        assert_eq!(SOCKADDR_UN_SIZE, 110);
        let a = SockAddrUn::new(b"/run/wayland-0").unwrap();
        assert_eq!(a.sun_family, AF_UNIX);
        assert_eq!(a.path(), b"/run/wayland-0");
        assert_eq!(a.as_bytes().len(), SOCKADDR_UN_SIZE);
        // The family is at the front of the wire form.
        assert_eq!(&a.as_bytes()[..2], &AF_UNIX.to_ne_bytes());
    }

    #[test]
    fn a_path_longer_than_the_field_is_rejected() {
        let long = [b'x'; UDS_PATH_MAX + 1];
        assert!(SockAddrUn::new(&long).is_none());
        assert!(SockAddrUn::new(&[b'x'; UDS_PATH_MAX]).is_some());
    }

    #[test]
    fn the_ioctls_carry_the_sizes_vfs_must_copy() {
        // Pair carries the peer's device number; listen an i32 backlog.
        assert_eq!(ioc_size(NWIOSUDSPAIR), 4);
        assert_eq!(ioc_size(NWIOSUDSBLOG), 4);
        // The address-carrying calls are all one sockaddr_un.
        for req in [NWIOSUDSADDR, NWIOGUDSADDR, NWIOSUDSCONN, NWIOSUDSACCEPT] {
            assert_eq!(ioc_size(req), SOCKADDR_UN_SIZE);
        }
        // getsockname and the minor query are the only ones that return data.
        assert!(ioc_is_out(NWIOGUDSADDR));
        assert!(!ioc_is_in(NWIOGUDSADDR));
        assert!(ioc_is_out(NWIOGUDSMINOR));
        assert_eq!(ioc_size(NWIOGUDSMINOR), 4);
        assert!(ioc_is_in(NWIOSUDSADDR));
        assert!(ioc_is_in(NWIOSUDSCONN));
    }

    #[test]
    fn the_control_ioctls_carry_a_msg_control() {
        assert_eq!(MSG_CONTROL_SIZE, 1024);
        assert_eq!(MSG_CONTROL_MAX, 1020);
        assert_eq!(MSG_CONTROL_LEN_OFF, 1020);
        assert_eq!(ioc_size(NWIOSUDSCTRL), MSG_CONTROL_SIZE);
        assert_eq!(ioc_size(NWIOGUDSCTRL), MSG_CONTROL_SIZE);
        // sendmsg control only travels in; recvmsg's both takes the caller's
        // space and returns what was written.
        assert!(ioc_is_in(NWIOSUDSCTRL) && !ioc_is_out(NWIOSUDSCTRL));
        assert!(ioc_is_in(NWIOGUDSCTRL) && ioc_is_out(NWIOGUDSCTRL));
        assert_eq!(ioc_size(NWIOGUDSPEERCRED), UUCRED_SIZE);
    }

    #[test]
    fn cmsg_layout_matches_the_reference_headers() {
        // NetBSD's `cmsghdr` is a `socklen_t` plus two `int`s, and MINIX aligns
        // to 4, so the aligned data offset is the header size itself.
        assert_eq!(CMSG_HDR_SIZE, 12);
        assert_eq!(CMSG_DATA_OFF, 12);
        // One descriptor: CMSG_LEN(4) and CMSG_SPACE(4) are both 16.
        assert_eq!(cmsg_len(4), 16);
        assert_eq!(cmsg_space(4), 16);
        // Sixteen descriptors (OPEN_MAX): 12 + 64, already aligned.
        assert_eq!(cmsg_space(4 * 16), 76);
        // An unaligned payload rounds up in SPACE but not in LEN.
        assert_eq!(cmsg_len(6), 18);
        assert_eq!(cmsg_space(6), 20);
        // uucred: cr_unused, a 4-byte uid, a 4-byte gid, cr_ngroups, then
        // cr_groups[16] — 16 bytes of scalars plus 64 of group ids.
        assert_eq!(UUCRED_SIZE, 80);
        assert_eq!(cmsg_space(UUCRED_SIZE), 12 + 80);
    }

    /// A control buffer holding `msgs` as consecutive `cmsghdr`s, and the
    /// `msg_controllen` covering them.
    fn control(msgs: &[(i32, i32, &[u8])]) -> ([u8; MSG_CONTROL_SIZE], usize) {
        let mut buf = [0u8; MSG_CONTROL_SIZE];
        let mut off = 0usize;
        for &(level, ctype, payload) in msgs {
            write_u32(&mut buf, off + CMSG_LEN_OFF, cmsg_len(payload.len()) as u32);
            write_u32(&mut buf, off + CMSG_LEVEL_OFF, level as u32);
            write_u32(&mut buf, off + CMSG_TYPE_OFF, ctype as u32);
            buf[off + CMSG_DATA_OFF..off + CMSG_DATA_OFF + payload.len()].copy_from_slice(payload);
            off += cmsg_space(payload.len());
        }
        (buf, off)
    }

    #[test]
    fn a_rights_message_yields_its_descriptors() {
        let (buf, len) = control(&[(SOL_SOCKET, SCM_RIGHTS, &[3u8, 0, 0, 0, 7, 0, 0, 0])]);
        assert_eq!(len, cmsg_space(8));
        let mut out = [-1i32; 16];
        assert_eq!(parse_rights(&buf, len, &mut out), Ok(2));
        assert_eq!(&out[..2], &[3, 7]);
    }

    #[test]
    fn unrelated_and_empty_control_data_is_ignored() {
        let mut out = [-1i32; 16];
        // A non-socket level, then a socket level with a different type.
        let (buf, len) = control(&[(1, SCM_RIGHTS, &[1u8, 0, 0, 0]), (SOL_SOCKET, 9, &[0u8; 4])]);
        assert_eq!(parse_rights(&buf, len, &mut out), Ok(0));
        // An empty control area yields nothing either.
        assert_eq!(parse_rights(&buf, 0, &mut out), Ok(0));
    }

    #[test]
    fn a_truncated_or_lying_header_is_a_bad_header() {
        let mut out = [-1i32; 16];
        let mut buf = [0u8; MSG_CONTROL_SIZE];
        // cmsg_len smaller than the header it describes.
        write_u32(&mut buf, CMSG_LEN_OFF, 4);
        assert_eq!(parse_rights(&buf, 16, &mut out), Err(CmsgError::BadHeader));
        // cmsg_len running past the control data the caller declared.
        write_u32(&mut buf, CMSG_LEN_OFF, 64);
        assert_eq!(parse_rights(&buf, 16, &mut out), Err(CmsgError::BadHeader));
    }

    #[test]
    fn too_many_descriptors_is_reported() {
        let (buf, len) = control(&[(SOL_SOCKET, SCM_RIGHTS, &[0u8; 16])]);
        let mut out = [0i32; 2];
        assert_eq!(parse_rights(&buf, len, &mut out), Err(CmsgError::TooMany));
    }

    #[test]
    fn build_then_parse_round_trips() {
        let mut buf = [0u8; MSG_CONTROL_SIZE];
        let len = build_control(&mut buf, &[5, 6, 7], None);
        assert_eq!(len, cmsg_space(12));
        let mut out = [-1i32; 16];
        assert_eq!(parse_rights(&buf, len, &mut out), Ok(3));
        assert_eq!(&out[..3], &[5, 6, 7]);
        assert_eq!(parse_creds(&buf, len), None);
    }

    #[test]
    fn descriptors_and_credentials_lay_out_one_after_the_other() {
        let cred = uucred(1000, 100);
        let mut buf = [0u8; MSG_CONTROL_SIZE];
        let len = build_control(&mut buf, &[9], Some(&cred));
        // The reference's `clen_desired`: CMSG_SPACE(4) + CMSG_SPACE(uucred).
        assert_eq!(len, cmsg_space(4) + cmsg_space(UUCRED_SIZE));
        assert_eq!(len, 16 + 92);

        let mut out = [-1i32; 16];
        assert_eq!(parse_rights(&buf, len, &mut out), Ok(1));
        assert_eq!(out[0], 9);
        assert_eq!(parse_creds(&buf, len), Some((1000, 100)));

        // With no descriptors the credentials take the first header, as the
        // reference's `recv_cred` does when `recv_fds` was never called.
        let len = build_control(&mut buf, &[], Some(&cred));
        assert_eq!(len, cmsg_space(UUCRED_SIZE));
        assert_eq!(parse_rights(&buf, len, &mut out), Ok(0));
        assert_eq!(parse_creds(&buf, len), Some((1000, 100)));
    }

    #[test]
    fn a_uucred_carries_only_the_ids() {
        let cred = uucred(7, 8);
        assert_eq!(cred.len(), UUCRED_SIZE);
        assert_eq!(&cred[..4], &[0, 0, 0, 0], "cr_unused stays clear");
        assert_eq!(
            &cred[UUCRED_UID_OFF..UUCRED_UID_OFF + 4],
            &7u32.to_ne_bytes()
        );
        assert_eq!(
            &cred[UUCRED_GID_OFF..UUCRED_GID_OFF + 4],
            &8u32.to_ne_bytes()
        );
        assert_eq!(cred[16..], [0u8; UUCRED_SIZE - 16], "no group list");
    }
}
