//! `wl_shm` client — `/bin/wlclient`.
//!
//! The Phase 1b gate's client: connects to `/bin/wlserver` over `/run/wayland-0`,
//! binds `wl_compositor` and `wl_shm`, draws into a `memfd` pool, passes the pool
//! fd with `SCM_RIGHTS`, and commits a surface. It then waits for
//! `wl_buffer.release` and reads the framebuffer back to prove the frame reached
//! `/dev/fb`.
//!
//! It speaks only the published protocol — no private opcodes — so it exercises
//! the server exactly as a stock client would. Every step has its own failure
//! code so a gate failure names the call that broke.

use minix_std::fs::{self, PollFd};
use minix_std::uds;
use minix_std::vmem;
use wayland::client::Client;
use wayland::protocol::{self, Kind};
use wayland::wire::{Arg, DispatchBuf, Header, MAX_ARGS, MessageBuffer, WireError};

use crate::{write_err, write_out};

/// The socket the server binds.
const SOCK_PATH: &[u8] = b"/run/wayland-0";

/// The surface the client draws: a 64x64 magenta square, sized so it fits any
/// mode the port has (the smallest is 1024x768).
const WIDTH: i32 = 64;
const HEIGHT: i32 = 64;
const STRIDE: i32 = WIDTH * 4;
const POOL_SIZE: i32 = STRIDE * HEIGHT;

/// XRGB8888 magenta (LE bytes B,G,R,0 = FF,00,FF,00) — distinctive enough that a
/// framebuffer pixel equal to it is this frame and not a leftover.
const COLOR: u32 = 0x00FF_00FF;

/// The `/dev/fb` BAR is larger than the mode's frame; the mode itself is
/// 1024x768, so reading the first pixel needs only a page.
const FB_MAP_LEN: usize = 4 * 1024 * 1024;

const POLLIN: i16 = 0x001;

fn fail(code: i32, msg: &[u8]) -> i32 {
    write_err(msg);
    code
}

/// Build one request with `f` and send it.
fn send_req<R, F>(conn: i32, req: &mut [u8; 512], f: F) -> Result<R, i32>
where
    F: FnOnce(&mut DispatchBuf<'_>) -> Result<R, WireError>,
{
    let mut q = DispatchBuf::new(req);
    let r = f(&mut q).map_err(|_| fail(2, b"wlclient: encode failed\n"))?;
    match uds::send(conn, q.bytes()) {
        Ok(n) if n as usize == q.len() => Ok(r),
        _ => Err(fail(3, b"wlclient: send failed\n")),
    }
}

/// Poll `conn` for readability, then read one chunk into `inbuf`.
fn fill(conn: i32, inbuf: &mut MessageBuffer, chunk: &mut [u8; 512], timeout_ms: i32) -> bool {
    let mut pf = [PollFd {
        fd: conn,
        events: POLLIN,
        revents: 0,
    }];
    match fs::poll(&mut pf, timeout_ms) {
        Ok(n) if n > 0 && pf[0].revents & POLLIN != 0 => {}
        _ => return false,
    }
    match uds::recv(conn, chunk) {
        Ok(n) if n > 0 => inbuf.push(&chunk[..n as usize]).is_ok(),
        _ => false,
    }
}

/// Connect to the server, retrying while it is still coming up.
fn connect() -> Result<i32, i32> {
    let fd = uds::socket().map_err(|_| fail(4, b"wlclient: socket failed\n"))?;
    for _ in 0..100_000 {
        if uds::connect(fd, SOCK_PATH).is_ok() {
            return Ok(fd);
        }
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
    let _ = uds::close_fd(fd);
    Err(fail(5, b"wlclient: connect failed\n"))
}

/// Read registry events until the factories a client may bind are known.
///
/// Returns the registry names of `wl_shm`, `wl_compositor` and `wl_seat`; a client
/// uses the ones it needs (the `wl_shm` client the first two, the input client the
/// last two).
fn find_globals(
    conn: i32,
    client: &Client,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<(u32, u32, u32), i32> {
    let mut shm = None;
    let mut compositor = None;
    let mut seat = None;
    while shm.is_none() || compositor.is_none() || seat.is_none() {
        if !fill(conn, inbuf, chunk, 2000) {
            return Err(fail(6, b"wlclient: registry went quiet\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(7, b"wlclient: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(8, b"wlclient: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let (kind, argc) = match client.decode(msg, &mut args) {
                Ok((_, k, n)) => (k, n),
                Err(_) => return Err(fail(9, b"wlclient: undecodable event\n")),
            };
            if kind == Kind::Registry
                && h.opcode == protocol::registry_ev::GLOBAL
                && argc == 3
                && let Some(name) = args[0].as_uint()
                && let Some(interface) = args[1].as_str()
            {
                if interface == b"wl_shm" {
                    shm = Some(name);
                } else if interface == b"wl_compositor" {
                    compositor = Some(name);
                } else if interface == b"wl_seat" {
                    seat = Some(name);
                }
            }
            inbuf.consume(h.size as usize);
        }
    }
    // All three are set by the loop's exit condition.
    Ok((shm.unwrap_or(0), compositor.unwrap_or(0), seat.unwrap_or(0)))
}

/// Wait for a one-shot callback's `done`, so the server has drained every request
/// sent before it. This is what makes the pool fd's control message land on the
/// `create_pool` read rather than sharing one with earlier requests.
fn await_callback(
    conn: i32,
    client: &Client,
    callback: u32,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<(), i32> {
    for _ in 0..64 {
        if !fill(conn, inbuf, chunk, 2000) {
            return Err(fail(10, b"wlclient: sync went quiet\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(11, b"wlclient: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(12, b"wlclient: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let kind = match client.decode(msg, &mut args) {
                Ok((_, k, _)) => k,
                Err(_) => return Err(fail(13, b"wlclient: undecodable event\n")),
            };
            let done = kind == Kind::Callback
                && h.object_id == callback
                && h.opcode == protocol::callback_ev::DONE;
            inbuf.consume(h.size as usize);
            if done {
                return Ok(());
            }
        }
    }
    Err(fail(14, b"wlclient: sync never completed\n"))
}

/// Wait until the server releases `buffer`, or reports an error.
fn await_release(
    conn: i32,
    client: &Client,
    buffer: u32,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<(), i32> {
    loop {
        if !fill(conn, inbuf, chunk, 4000) {
            return Err(fail(15, b"wlclient: no release (server never presented)\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(16, b"wlclient: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(17, b"wlclient: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let kind = match client.decode(msg, &mut args) {
                Ok((_, k, _)) => k,
                Err(_) => return Err(fail(18, b"wlclient: undecodable event\n")),
            };
            if kind == Kind::Display && h.opcode == protocol::display_ev::ERROR {
                return Err(fail(19, b"wlclient: server sent wl_display.error\n"));
            }
            let released = kind == Kind::Buffer
                && h.object_id == buffer
                && h.opcode == protocol::buffer_ev::RELEASE;
            inbuf.consume(h.size as usize);
            if released {
                return Ok(());
            }
        }
    }
}

fn run() -> Result<(), i32> {
    // ---- map the framebuffer so the frame can be read back ----
    let fb_fd = unsafe { fs::open(b"/dev/fb", fs::O_RDWR, 0) }
        .map_err(|_| fail(20, b"wlclient: open /dev/fb failed\n"))?;
    let fb = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            FB_MAP_LEN,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            fb_fd,
            0,
        )
    };
    if fb == vmem::MAP_FAILED {
        return Err(fail(21, b"wlclient: mmap /dev/fb failed\n"));
    }

    // ---- the pool: a memfd this client draws into and shares ----
    let pool_fd = fs::memfd_create(0).map_err(|_| fail(22, b"wlclient: memfd_create failed\n"))?;
    fs::truncate(pool_fd, POOL_SIZE as i64)
        .map_err(|_| fail(23, b"wlclient: ftruncate failed\n"))?;
    let pool = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            POOL_SIZE as usize,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            pool_fd,
            0,
        )
    };
    if pool == vmem::MAP_FAILED {
        return Err(fail(24, b"wlclient: mmap of the pool failed\n"));
    }
    for i in 0..(WIDTH * HEIGHT) as usize {
        unsafe { core::ptr::write_volatile(pool.add(i * 4).cast::<u32>(), COLOR) };
    }

    // ---- the protocol ----
    let conn = connect()?;
    let mut client = Client::new();
    let mut req = [0u8; 512];
    let mut inbuf = MessageBuffer::new();
    let mut chunk = [0u8; 512];

    let registry = send_req(conn, &mut req, |q| client.get_registry(q))?;
    let (shm_name, compositor_name, _seat_name) =
        find_globals(conn, &client, &mut inbuf, &mut chunk)?;

    let compositor = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            compositor_name,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let shm = send_req(conn, &mut req, |q| {
        client.bind(registry, shm_name, b"wl_shm", 1, Kind::Shm, q)
    })?;
    let surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;

    // Drain every request above before the fd travels: see `await_callback`.
    let callback = send_req(conn, &mut req, |q| client.sync(q))?;
    await_callback(conn, &client, callback, &mut inbuf, &mut chunk)?;

    // ---- create the pool, passing the fd by SCM_RIGHTS ----
    uds::send_fds(conn, &[pool_fd]).map_err(|_| fail(25, b"wlclient: send_fds failed\n"))?;
    let pool_obj = send_req(conn, &mut req, |q| client.create_pool(shm, 0, POOL_SIZE, q))?;
    let buffer = send_req(conn, &mut req, |q| {
        client.create_buffer(
            pool_obj,
            0,
            WIDTH,
            HEIGHT,
            STRIDE,
            protocol::WL_SHM_FORMAT_ARGB8888,
            q,
        )
    })?;

    // ---- present: attach, damage, commit ----
    send_req(conn, &mut req, |q| client.attach(surface, buffer, 0, 0, q))?;
    send_req(conn, &mut req, |q| {
        client.damage(surface, 0, 0, WIDTH, HEIGHT, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(surface, q))?;

    await_release(conn, &client, buffer, &mut inbuf, &mut chunk)?;

    // ---- the frame must be on the display ----
    // The server composites at (0,0); the client reads the same device memory
    // back, so this is the whole present path checked end to end.
    let px = unsafe { core::ptr::read_volatile(fb.cast::<u32>()) };
    if px != COLOR {
        write_err(b"wlclient: framebuffer pixel is 0x");
        print_hex(px);
        write_err(b", wanted 0x00FF00FF\n");
        return Err(fail(26, b"wlclient: the frame did not reach /dev/fb\n"));
    }

    let _ = unsafe { vmem::munmap(pool, POOL_SIZE as usize) };
    let _ = uds::close_fd(conn);
    Ok(())
}

/// Print a `u32` as eight lowercase hex digits.
fn print_hex(v: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 8];
    for (i, slot) in out.iter_mut().enumerate() {
        let shift = (7 - i) * 4;
        *slot = HEX[((v >> shift) & 0xf) as usize];
    }
    write_err(&out);
}

/// Print a `u32` in decimal.
fn put_dec(mut n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    write_out(&buf[i..]);
}

/// `/bin/wlkey`: bind `wl_seat`'s keyboard, commit a surface so it is entered, and
/// print each key the server sends. The Phase 1c gate's client.
///
/// It exits on the first key pressed, so the shell that ran it gets its prompt back
/// and the gate has a line to match.
pub fn wl_key(_args: &[&str]) -> i32 {
    match run_key() {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn run_key() -> Result<(), i32> {
    let conn = connect()?;
    let mut client = Client::new();
    let mut req = [0u8; 512];
    let mut inbuf = MessageBuffer::new();
    let mut chunk = [0u8; 512];

    let registry = send_req(conn, &mut req, |q| client.get_registry(q))?;
    let (_shm_name, compositor_name, seat_name) =
        find_globals(conn, &client, &mut inbuf, &mut chunk)?;

    let compositor = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            compositor_name,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let seat = send_req(conn, &mut req, |q| {
        client.bind(registry, seat_name, b"wl_seat", 1, Kind::Seat, q)
    })?;
    let _keyboard = send_req(conn, &mut req, |q| client.get_keyboard(seat, q))?;
    let surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;
    // A commit is what focuses the surface and makes the server send `enter`; no
    // buffer is needed for that, so this client draws nothing.
    send_req(conn, &mut req, |q| client.commit(surface, q))?;
    write_out(b"wlkey: ready\n");

    loop {
        if !fill(conn, &mut inbuf, &mut chunk, 4000) {
            return Err(fail(30, b"wlkey: no key arrived\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(31, b"wlkey: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(32, b"wlkey: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let kind = match client.decode(msg, &mut args) {
                Ok((_, k, _)) => k,
                Err(_) => return Err(fail(33, b"wlkey: undecodable event\n")),
            };
            match (kind, h.opcode) {
                (Kind::Keyboard, protocol::keyboard_ev::KEY) => {
                    let key = args[2].as_uint().unwrap_or(0);
                    let state = args[3].as_uint().unwrap_or(0);
                    let pressed = state == wayland::input::STATE_PRESSED;
                    write_out(b"wlkey: key ");
                    put_dec(key);
                    let word: &[u8] = if pressed {
                        b" pressed\n"
                    } else {
                        b" released\n"
                    };
                    write_out(word);
                    if pressed {
                        let _ = uds::close_fd(conn);
                        return Ok(());
                    }
                }
                (Kind::Keyboard, protocol::keyboard_ev::MODIFIERS) => {
                    let depressed = args[1].as_uint().unwrap_or(0);
                    write_out(b"wlkey: modifiers ");
                    put_dec(depressed);
                    write_out(b"\n");
                }
                _ => {}
            }
            inbuf.consume(h.size as usize);
        }
    }
}

/// Run the client: `PASS` on success, otherwise the failing step's code.
pub fn wl_client(_args: &[&str]) -> i32 {
    match run() {
        Ok(()) => {
            write_out(b"wlclient: PASS\n");
            0
        }
        Err(code) => code,
    }
}
