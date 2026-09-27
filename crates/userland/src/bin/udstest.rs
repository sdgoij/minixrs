//! UNIX-domain socket smoke test — `/bin/udstest`.
//!
//! Drives the `uds` server end to end through the real syscall path: a
//! `socketpair(2)` round-trip, then `bind`/`listen`/`connect`/`accept` with a
//! round-trip in both directions on the accepted connection, `SO_PEERCRED`, and
//! an `SCM_RIGHTS` descriptor transfer over it. Expect `udstest: OK` and exit 0.
//!
//! Every step has its own failure code, so a gate failure names the call that
//! broke (1–4 socketpair, 5–10 bind/listen/connect/accept, 11–16 the two
//! round-trips, 17–19 `SO_PEERCRED`, 20–30 descriptor passing and its
//! `SCM_CREDS`, 31–33 `SO_PEERCRED` again after it).

#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// The name bound by the listening half. No filesystem node is created: this
/// server's name registry is its own table, so `bind` needs no inode (the
/// reference's libc `mknod`s one for the permission check only).
const PATH: &[u8] = b"/udstest.sock";

/// Unwrap a socket call, reporting `code` and `msg` on failure.
fn step<T>(r: Result<T, minix_std::MinixErr>, code: i32, msg: &[u8]) -> Result<T, i32> {
    r.map_err(|_| {
        userland::write_err(msg);
        code
    })
}

/// Report a mismatch between what was written and what was read.
fn expect(got: &[u8], want: &[u8], code: i32, msg: &[u8]) -> Result<(), i32> {
    if got == want {
        Ok(())
    } else {
        userland::write_err(msg);
        Err(code)
    }
}

/// A `(uid, gid)` pair as `SO_PEERCRED` reports it.
type Cred = (u32, u32);

/// `SO_PEERCRED` on both ends of the accepted connection, as
/// `[client's view of the server, server's view of the client]`.
///
/// Both ends here are this one process, so each must name the *other* socket's
/// recorded owner — the same credentials twice. `base` is the failure code for
/// the first call; the second reports `base + 1`.
fn peer_creds(c: i32, s: i32, base: i32) -> Result<[Cred; 2], i32> {
    let client_side = minix_std::uds::peer_cred(c).map_err(|_| {
        userland::write_err(b"udstest: peer_cred on client failed\n");
        base
    })?;
    let server_side = minix_std::uds::peer_cred(s).map_err(|_| {
        userland::write_err(b"udstest: peer_cred on server failed\n");
        base + 1
    })?;
    Ok([client_side, server_side])
}

fn run() -> Result<(), i32> {
    // ---- socketpair: write one end, read the other ----
    let (a, b) = step(
        minix_std::uds::socketpair(),
        1,
        b"udstest: socketpair failed\n",
    )?;
    step(
        minix_std::uds::send(a, b"hello"),
        2,
        b"udstest: socketpair write failed\n",
    )?;
    let mut buf = [0u8; 32];
    let n = step(
        minix_std::uds::recv(b, &mut buf),
        3,
        b"udstest: socketpair read failed\n",
    )?;
    expect(
        &buf[..n as usize],
        b"hello",
        4,
        b"udstest: socketpair bytes differ\n",
    )?;
    let _ = minix_std::uds::close_fd(a);
    let _ = minix_std::uds::close_fd(b);

    // ---- bind / listen / connect / accept: both directions ----
    let l = step(
        minix_std::uds::socket(),
        5,
        b"udstest: listener socket failed\n",
    )?;
    step(minix_std::uds::bind(l, PATH), 6, b"udstest: bind failed\n")?;
    step(minix_std::uds::listen(l, 4), 7, b"udstest: listen failed\n")?;
    let c = step(
        minix_std::uds::socket(),
        8,
        b"udstest: client socket failed\n",
    )?;
    step(
        minix_std::uds::connect(c, PATH),
        9,
        b"udstest: connect failed\n",
    )?;
    // The connection is queued by connect, so accept has something at once.
    let s = step(minix_std::uds::accept(l), 10, b"udstest: accept failed\n")?;

    // client -> server
    step(
        minix_std::uds::send(c, b"world"),
        11,
        b"udstest: client write failed\n",
    )?;
    let n = step(
        minix_std::uds::recv(s, &mut buf),
        12,
        b"udstest: server read failed\n",
    )?;
    expect(
        &buf[..n as usize],
        b"world",
        13,
        b"udstest: client->server bytes differ\n",
    )?;

    // server -> client
    step(
        minix_std::uds::send(s, b"reply"),
        14,
        b"udstest: server write failed\n",
    )?;
    let n = step(
        minix_std::uds::recv(c, &mut buf),
        15,
        b"udstest: client read failed\n",
    )?;
    expect(
        &buf[..n as usize],
        b"reply",
        16,
        b"udstest: server->client bytes differ\n",
    )?;

    // ---- SO_PEERCRED, before and after the descriptor transfer ----
    // Both must agree (each names the other end's owner) and the transfer must
    // leave them alone: the peer query VFS makes of the driver travels as an
    // ioctl, and an ioctl is what re-records a socket's owner.
    let before = peer_creds(c, s, 17)?;
    if before[0] != before[1] {
        userland::write_err(b"udstest: the two ends disagree on peer credentials\n");
        return Err(20);
    }

    // ---- descriptor passing: an SCM_RIGHTS transfer ----
    // The read end of a pipe travels over the connection while the write end
    // stays here. A byte written to the write end must then come back through
    // the descriptor the receiver was handed, which is only true if the two
    // name the same open file rather than a copy of its contents.
    let (pr, pw) = step(minix_std::fs::pipe(), 20, b"udstest: pipe failed\n")?;
    step(
        minix_std::uds::send_fds(c, &[pr]),
        21,
        b"udstest: send_fds failed\n",
    )?;
    // Control travels as its own ioctl, so the body is a separate message.
    step(
        minix_std::uds::send(c, b"fd"),
        22,
        b"udstest: descriptor announce failed\n",
    )?;
    let n = step(
        minix_std::uds::recv(s, &mut buf),
        23,
        b"udstest: descriptor announce read failed\n",
    )?;
    expect(
        &buf[..n as usize],
        b"fd",
        24,
        b"udstest: announce bytes differ\n",
    )?;

    let mut passed = [-1i32; 4];
    let (k, cred) = step(
        minix_std::uds::recv_fds_cred(s, &mut passed),
        25,
        b"udstest: recv_fds_cred failed\n",
    )?;
    if k != 1 {
        userland::write_err(b"udstest: expected one descriptor\n");
        return Err(26);
    }
    // The credentials that travelled with the descriptors are the sender's, and
    // this process is both ends, so they must be what the server's own view of
    // the client reports.
    if cred != Some(before[1]) {
        userland::write_err(b"udstest: SCM_CREDS disagree with SO_PEERCRED\n");
        return Err(27);
    }

    step(
        unsafe { minix_std::fs::write(pw, b"through") },
        28,
        b"udstest: pipe write failed\n",
    )?;
    let mut got = [0u8; 16];
    let n = step(
        unsafe { minix_std::fs::read(passed[0], &mut got) },
        29,
        b"udstest: passed-descriptor read failed\n",
    )?;
    expect(
        &got[..n as usize],
        b"through",
        30,
        b"udstest: passed bytes differ\n",
    )?;

    let _ = minix_std::fs::close(pr);
    let _ = minix_std::fs::close(pw);
    let _ = minix_std::fs::close(passed[0]);

    let after = peer_creds(c, s, 31)?;
    if after != before {
        userland::write_err(b"udstest: descriptor passing changed SO_PEERCRED\n");
        return Err(33);
    }

    let _ = minix_std::uds::close_fd(c);
    let _ = minix_std::uds::close_fd(s);
    let _ = minix_std::uds::close_fd(l);
    Ok(())
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    match run() {
        Ok(()) => {
            userland::write_out(b"udstest: OK\n");
            0
        }
        Err(code) => code,
    }
}
