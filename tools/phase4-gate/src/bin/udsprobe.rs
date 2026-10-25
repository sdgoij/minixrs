//! Diagnostic probe for the port's `/dev/uds` accept path (Phase 4c).
//!
//! `wlgate`'s server accepted a connection with `EINVAL`, while the in-house server's
//! identical handshake (`crates/userland`'s `waylandtest`) works. This probe walks the
//! same path one step at a time in a forked child — `getsockname`, a fresh socket, the
//! accept ioctl — then repeats the round through
//! `std::os::minix::net::UnixListener::accept`, so a failure names the step that produced
//! it instead of only the wrapper's errno.
//!
//! It also probes the C ABI's `send` with the flags wayland-backend uses
//! (`MSG_DONTWAIT | MSG_NOSIGNAL`), the other 4c blocker.

use std::os::fd::AsRawFd;
use std::os::minix::net::{UnixListener, UnixStream};
use std::time::Duration;

use minix_libc as _;

const SOCKET: &str = "/run/udsprobe";

/// `MSG_DONTWAIT` and `MSG_NOSIGNAL`, as the C headers spell them for this target.
const MSG_DONTWAIT: i32 = 0x0080;
const MSG_NOSIGNAL: i32 = 0x4000;

unsafe extern "C" {
    fn send(fd: i32, buf: *const core::ffi::c_void, len: usize, flags: i32) -> isize;
}

fn wait(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn main() {
    let listener = match UnixListener::bind(SOCKET) {
        Ok(listener) => listener,
        Err(e) => {
            println!("udsprobe: bind {SOCKET} failed: {e}");
            minix_rt::exit(1);
        }
    };
    let lfd = listener.as_raw_fd();
    match minix_std::uds::socket_minor(lfd) {
        Ok(minor) => println!("udsprobe: bound {SOCKET} fd={lfd} minor={minor}"),
        Err(e) => println!(
            "udsprobe: bound {SOCKET} fd={lfd} minor failed errno={}",
            e.0
        ),
    }

    let child = minix_rt::fork();
    if child == 0 {
        serve(lfd, &listener);
    }
    if child < 0 {
        println!("udsprobe: fork failed");
        minix_rt::exit(1);
    }

    // Two rounds: the first connection feeds the raw sub-steps, the second the std wrapper.
    connect_round(0, true);
    connect_round(1, false);

    let status = minix_rt::waitpid_status(child);
    println!("udsprobe: server_exit={status}");
    minix_rt::exit(if status == 0 { 0 } else { 1 });
}

/// The child: the raw path, then the `std` wrapper over a second connection.
fn serve(lfd: i32, listener: &UnixListener) -> ! {
    match minix_std::uds::socket_minor(lfd) {
        Ok(minor) => println!("udsprobe: child listener minor={minor}"),
        Err(e) => println!("udsprobe: child listener minor failed errno={}", e.0),
    }
    let raw_ok = raw_accept(lfd);
    let std_ok = std_accept(listener);
    println!("udsprobe: raw={raw_ok} std={std_ok}");
    if raw_ok && std_ok {
        println!("udsprobe: PASS both accept paths served a connection");
        minix_rt::exit(0);
    }
    minix_rt::exit(1);
}

/// `accept` spelled out: `getsockname`, a fresh `/dev/uds` socket, the accept ioctl.
fn raw_accept(lfd: i32) -> bool {
    for attempt in 0..400u32 {
        let mut addr = match minix_std::uds::getsockname(lfd) {
            Ok(addr) => addr,
            Err(e) => {
                println!("udsprobe: raw getsockname failed errno={}", e.0);
                wait(5);
                continue;
            }
        };
        if attempt == 0 {
            let family = u16::from_ne_bytes([addr[0], addr[1]]);
            let end = addr[2..]
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(addr.len() - 2);
            let path = core::str::from_utf8(&addr[2..2 + end]).unwrap_or("<bytes>");
            println!("udsprobe: raw getsockname family={family} path={path:?}");
        }

        let Some(fd) = open_socket() else {
            return false;
        };
        let r = unsafe { minix_std::fs::ioctl(fd, net::NWIOSUDSACCEPT, addr.as_mut_ptr()) };
        match r {
            Ok(_) => {
                println!("udsprobe: raw accept ok fd={fd}");
                return true;
            }
            Err(e) if e.0 == 11 => {
                let _ = minix_std::fs::close(fd);
                wait(5);
            }
            Err(e) => {
                println!("udsprobe: raw accept failed errno={}", e.0);
                let _ = minix_std::fs::close(fd);
                return false;
            }
        }
    }
    println!("udsprobe: raw accept never saw a connection");
    false
}

fn open_socket() -> Option<i32> {
    match minix_std::uds::socket() {
        Ok(fd) => {
            match minix_std::uds::socket_minor(fd) {
                Ok(minor) => println!("udsprobe: raw fresh socket fd={fd} minor={minor}"),
                Err(e) => println!(
                    "udsprobe: raw fresh socket fd={fd} minor failed errno={}",
                    e.0
                ),
            }
            Some(fd)
        }
        Err(e) => {
            println!("udsprobe: raw fresh socket failed errno={}", e.0);
            None
        }
    }
}

/// `std::os::minix::net::UnixListener::accept`, the wrapper the gate uses.
fn std_accept(listener: &UnixListener) -> bool {
    for _ in 0..400u32 {
        match listener.accept() {
            Ok((stream, _addr)) => {
                println!("udsprobe: std accept ok fd={}", stream.as_raw_fd());
                return true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => wait(5),
            Err(e) => {
                println!("udsprobe: std accept failed: {e}");
                return false;
            }
        }
    }
    println!("udsprobe: std accept never saw a connection");
    false
}

/// One client round: connect, probe the C shim's flags, hold the connection open, close.
fn connect_round(round: u32, probe_send: bool) {
    let stream = match UnixStream::connect(SOCKET) {
        Ok(stream) => stream,
        Err(e) => {
            println!("udsprobe: client {round} connect failed: {e}");
            return;
        }
    };
    let fd = stream.as_raw_fd();
    println!("udsprobe: client {round} connected fd={fd}");
    if probe_send {
        // Zero bytes, so the probe says only what the flags do.
        let none: [u8; 0] = [];
        let flags = MSG_DONTWAIT | MSG_NOSIGNAL;
        let r = unsafe { send(fd, none.as_ptr().cast(), 0, flags) };
        println!("udsprobe: client send(flags=0x{flags:x}) -> {r}");
        let r = unsafe { send(fd, none.as_ptr().cast(), 0, 0) };
        println!("udsprobe: client send(flags=0) -> {r}");
    }
    wait(300);
    drop(stream);
    wait(100);
}
