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

/// The output's geometry, which every backend this port has adopts (2d reads the
/// framebuffer back at these coordinates).
const FB_WIDTH: usize = 1024;
const FB_HEIGHT: usize = 768;
const FB_PITCH: usize = FB_WIDTH * 4;

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

/// The registry names of the globals this client may bind.
struct Globals {
    shm: u32,
    compositor: u32,
    seat: u32,
    /// `xdg_wm_base` — 0 if the server does not offer it (2b).
    wm_base: u32,
    /// `zwlr_layer_shell_v1` — 0 if the server does not offer it (2e).
    layer_shell: u32,
    /// `zxdg_decoration_manager_v1` — 0 if the server does not offer it (2e).
    decoration_manager: u32,
}

/// Read registry events until the factories a client may bind are known.
fn find_globals(
    conn: i32,
    client: &Client,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<Globals, i32> {
    let mut shm = None;
    let mut compositor = None;
    let mut seat = None;
    let mut wm_base = None;
    let mut layer_shell = None;
    let mut decoration_manager = None;
    while shm.is_none() || compositor.is_none() || seat.is_none() || wm_base.is_none() {
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
                } else if interface == b"xdg_wm_base" {
                    wm_base = Some(name);
                } else if interface == b"zwlr_layer_shell_v1" {
                    layer_shell = Some(name);
                } else if interface == b"zxdg_decoration_manager_v1" {
                    decoration_manager = Some(name);
                }
            }
            inbuf.consume(h.size as usize);
        }
    }
    // All four are set by the loop's exit condition.
    Ok(Globals {
        shm: shm.unwrap_or(0),
        compositor: compositor.unwrap_or(0),
        seat: seat.unwrap_or(0),
        wm_base: wm_base.unwrap_or(0),
        layer_shell: layer_shell.unwrap_or(0),
        decoration_manager: decoration_manager.unwrap_or(0),
    })
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
    let g = find_globals(conn, &client, &mut inbuf, &mut chunk)?;

    let compositor = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            g.compositor,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let shm = send_req(conn, &mut req, |q| {
        client.bind(registry, g.shm, b"wl_shm", 1, Kind::Shm, q)
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

/// Wait for `wl_keyboard.keymap`, resolve the fd it named, and check the bytes it
/// carries. Returns the size the server advertised.
///
/// The fd arrives by `SCM_RIGHTS` with the message, not in it (Phase 2a). The
/// control is lifted as soon as the event is seen, the way the server lifts a pool
/// fd — and, for the same reason, before any later read can claim it.
fn await_keymap(
    conn: i32,
    client: &Client,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<u32, i32> {
    loop {
        if !fill(conn, inbuf, chunk, 4000) {
            return Err(fail(34, b"wlkey: no keymap arrived\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(35, b"wlkey: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(36, b"wlkey: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let (kind, argc) = match client.decode(msg, &mut args) {
                Ok((_, k, n)) => (k, n),
                Err(_) => return Err(fail(37, b"wlkey: undecodable event\n")),
            };
            let is_keymap = kind == Kind::Keyboard && h.opcode == protocol::keyboard_ev::KEYMAP;
            let format = args[0].as_uint();
            let size = args[2].as_uint();
            inbuf.consume(h.size as usize);
            if !is_keymap {
                continue;
            }
            if argc != 3 {
                return Err(fail(38, b"wlkey: keymap arity\n"));
            }
            if format.unwrap_or(u32::MAX) != protocol::WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1 {
                return Err(fail(39, b"wlkey: keymap is not XKB_V1\n"));
            }
            let size = size.unwrap_or(0);
            let mut fds = [-1i32; 1];
            if uds::recv_fds(conn, &mut fds).unwrap_or(0) != 1 || fds[0] < 0 {
                return Err(fail(40, b"wlkey: keymap fd did not arrive\n"));
            }
            let km = fds[0];
            let checked = check_keymap(km, size);
            let _ = uds::close_fd(km);
            checked?;
            return Ok(size);
        }
    }
}

/// Read a keymap fd whole: NUL-terminated XKB text whose length is `size`.
fn check_keymap(fd: i32, size: u32) -> Result<(), i32> {
    const PREFIX: &[u8] = b"xkb_keymap {";
    let mut first = [0u8; PREFIX.len()];
    let mut got = 0usize;
    while got < first.len() {
        let n = match unsafe { fs::read(fd, &mut first[got..]) } {
            Ok(0) => break,
            Ok(n) => n as usize,
            Err(_) => return Err(fail(41, b"wlkey: keymap read failed\n")),
        };
        got += n;
    }
    if &first[..got] != PREFIX {
        return Err(fail(
            42,
            b"wlkey: keymap does not start with xkb_keymap {\n",
        ));
    }
    let mut total = got as u32;
    let mut last = first[got - 1];
    let mut buf = [0u8; 4096];
    loop {
        let n = match unsafe { fs::read(fd, &mut buf) } {
            Ok(n) if n > 0 => n as usize,
            Ok(_) => break,
            Err(_) => return Err(fail(41, b"wlkey: keymap read failed\n")),
        };
        last = buf[n - 1];
        total += n as u32;
    }
    if total != size {
        return Err(fail(43, b"wlkey: keymap size mismatch\n"));
    }
    if last != 0 {
        return Err(fail(44, b"wlkey: keymap is not NUL-terminated\n"));
    }
    Ok(())
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
    let g = find_globals(conn, &client, &mut inbuf, &mut chunk)?;

    let compositor = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            g.compositor,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let seat = send_req(conn, &mut req, |q| {
        client.bind(registry, g.seat, b"wl_seat", 1, Kind::Seat, q)
    })?;
    let _keyboard = send_req(conn, &mut req, |q| client.get_keyboard(seat, q))?;
    let surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;
    // A commit is what focuses the surface and makes the server send `enter`; no
    // buffer is needed for that, so this client draws nothing.
    send_req(conn, &mut req, |q| client.commit(surface, q))?;

    // The seat's keymap rides with the keyboard object (Phase 2a): resolve the fd
    // and check the bytes before announcing readiness, so `ready` means "the
    // keyboard is usable", not merely "the requests went out".
    let size = await_keymap(conn, &client, &mut inbuf, &mut chunk)?;
    write_out(b"wlkey: keymap ");
    put_dec(size);
    write_out(b" ok\n");
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

/// One connection's state for the two-client focus gate (2c).
struct Peer {
    conn: i32,
    client: Client,
    inbuf: MessageBuffer,
    chunk: [u8; 512],
    req: [u8; 512],
    compositor: u32,
    wm_base: u32,
    shm: u32,
    /// The window this connection currently has mapped.
    surface: u32,
    xdg_surface: u32,
    toplevel: u32,
    /// What the server has said so far.
    configure: Option<u32>,
    size: Option<(i32, i32)>,
    enters: u32,
    leaves: u32,
    releases: u32,
    keys: u32,
    last_key: u32,
    callbacks: u32,
}

/// Connect one peer: bind the factories, make a keyboard (so an `enter` has
/// somewhere to land), and map its first window.
fn peer_connect() -> Result<Peer, i32> {
    let conn = connect()?;
    let mut p = Peer {
        conn,
        client: Client::new(),
        inbuf: MessageBuffer::new(),
        chunk: [0u8; 512],
        req: [0u8; 512],
        compositor: 0,
        wm_base: 0,
        shm: 0,
        surface: 0,
        xdg_surface: 0,
        toplevel: 0,
        configure: None,
        size: None,
        enters: 0,
        leaves: 0,
        releases: 0,
        keys: 0,
        last_key: 0,
        callbacks: 0,
    };
    let registry = send_req(conn, &mut p.req, |q| p.client.get_registry(q))?;
    let g = find_globals(conn, &p.client, &mut p.inbuf, &mut p.chunk)?;
    if g.wm_base == 0 {
        return Err(fail(80, b"wlx2: no xdg_wm_base global\n"));
    }
    p.compositor = send_req(conn, &mut p.req, |q| {
        p.client.bind(
            registry,
            g.compositor,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    p.shm = send_req(conn, &mut p.req, |q| {
        p.client.bind(registry, g.shm, b"wl_shm", 1, Kind::Shm, q)
    })?;
    let seat = send_req(conn, &mut p.req, |q| {
        p.client
            .bind(registry, g.seat, b"wl_seat", 1, Kind::Seat, q)
    })?;
    p.wm_base = send_req(conn, &mut p.req, |q| {
        p.client
            .bind(registry, g.wm_base, b"xdg_wm_base", 1, Kind::XdgWmBase, q)
    })?;
    send_req(conn, &mut p.req, |q| p.client.get_keyboard(seat, q))?;
    peer_map_window(&mut p)?;
    Ok(p)
}

/// Map one window on a peer and commit it. A surface's *first* commit is what the
/// server configures, and (2c) what asks it for focus.
fn peer_map_window(p: &mut Peer) -> Result<(), i32> {
    let (conn, compositor, wm_base) = (p.conn, p.compositor, p.wm_base);
    let (surface, xdg_surface, toplevel) = {
        let client = &mut p.client;
        let req = &mut p.req;
        let surface = send_req(conn, req, |q| client.create_surface(compositor, q))?;
        let xdg_surface = send_req(conn, req, |q| client.get_xdg_surface(wm_base, surface, q))?;
        let toplevel = send_req(conn, req, |q| client.get_toplevel(xdg_surface, q))?;
        send_req(conn, req, |q| {
            client.set_app_id(toplevel, b"minixrs.wlx2", q)
        })?;
        send_req(conn, req, |q| client.commit(surface, q))?;
        (surface, xdg_surface, toplevel)
    };
    p.surface = surface;
    p.xdg_surface = xdg_surface;
    p.toplevel = toplevel;
    p.configure = None;
    p.size = None;
    Ok(())
}

/// Read every complete message available on one peer and count it. `false` when the
/// connection ended.
fn read_peer(p: &mut Peer) -> bool {
    let n = match uds::recv(p.conn, &mut p.chunk) {
        Ok(n) if n > 0 => n as usize,
        _ => return false,
    };
    if p.inbuf.push(&p.chunk[..n]).is_err() {
        return false;
    }
    loop {
        let msg = match p.inbuf.next() {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(_) => return false,
        };
        let h = match Header::parse(msg) {
            Ok(h) => h,
            Err(_) => return false,
        };
        let mut args = [Arg::Uint(0); MAX_ARGS];
        let (kind, argc) = match p.client.decode(msg, &mut args) {
            Ok((_, k, n)) => (k, n),
            Err(_) => return false,
        };
        let a0 = args[0].as_uint();
        let i0 = args[0].as_int();
        let i1 = args[1].as_int();
        let a2 = args[2].as_uint();
        p.inbuf.consume(h.size as usize);
        match (kind, h.opcode) {
            (Kind::XdgSurface, protocol::xdg_surface_ev::CONFIGURE) if argc == 1 => {
                p.configure = a0;
            }
            (Kind::XdgToplevel, protocol::xdg_toplevel_ev::CONFIGURE) if argc == 3 => {
                p.size = Some((i0.unwrap_or(0), i1.unwrap_or(0)));
            }
            (Kind::Keyboard, protocol::keyboard_ev::ENTER) => p.enters += 1,
            (Kind::Keyboard, protocol::keyboard_ev::LEAVE) => p.leaves += 1,
            (Kind::Keyboard, protocol::keyboard_ev::KEY) if argc == 4 => {
                p.keys += 1;
                p.last_key = a2.unwrap_or(0);
            }
            (Kind::Buffer, protocol::buffer_ev::RELEASE) => p.releases += 1,
            (Kind::Callback, protocol::callback_ev::DONE) => p.callbacks += 1,
            (Kind::XdgWmBase, protocol::xdg_wm_base_ev::PING) => {
                // A ping is answered `pong` with the same serial.
                let mut buf = [0u8; 64];
                let mut q = DispatchBuf::new(&mut buf);
                if p.client.pong(p.wm_base, a0.unwrap_or(0), &mut q).is_ok() {
                    let _ = uds::send(p.conn, q.bytes());
                }
            }
            _ => {}
        }
    }
    true
}

/// Drain whichever peer is readable. A poll timeout is not a failure; a dropped
/// connection is.
fn pump_peers(peers: &mut [Peer; 2], ms: i32) -> bool {
    let mut pf = [
        PollFd {
            fd: peers[0].conn,
            events: POLLIN,
            revents: 0,
        },
        PollFd {
            fd: peers[1].conn,
            events: POLLIN,
            revents: 0,
        },
    ];
    match fs::poll(&mut pf, ms) {
        Ok(n) if n > 0 => {}
        _ => return true,
    }
    for i in 0..2 {
        if pf[i].revents & POLLIN != 0 && !read_peer(&mut peers[i]) {
            return false;
        }
    }
    true
}

/// Drain both peers until `done` holds, for up to `tries` polls of 10 ms.
fn wait_peers(
    peers: &mut [Peer; 2],
    tries: i32,
    done: impl Fn(&[Peer; 2]) -> bool,
) -> Result<(), i32> {
    for _ in 0..tries {
        if done(&*peers) {
            return Ok(());
        }
        if !pump_peers(peers, 10) {
            return Err(fail(70, b"wlx2: a connection dropped\n"));
        }
    }
    if done(&*peers) {
        return Ok(());
    }
    Err(fail(71, b"wlx2: timed out waiting for the server\n"))
}

/// Ack the configure, then put a barrier after it so the pool fd's control message
/// cannot share a read with a request sent before it.
fn peer_ack(p: &mut Peer) -> Result<(), i32> {
    let serial = p
        .configure
        .ok_or_else(|| fail(72, b"wlx2: no configure serial\n"))?;
    let (conn, xdg_surface) = (p.conn, p.xdg_surface);
    let client = &mut p.client;
    let req = &mut p.req;
    send_req(conn, req, |q| client.ack_configure(xdg_surface, serial, q))?;
    send_req(conn, req, |q| client.sync(q))?;
    Ok(())
}

/// Present a full buffer at the configured size on one peer.
fn peer_map_buffer(p: &mut Peer) -> Result<(), i32> {
    let (w, h) = p
        .size
        .ok_or_else(|| fail(73, b"wlx2: no configured size\n"))?;
    if w <= 0 || h <= 0 {
        return Err(fail(74, b"wlx2: the server configured no size\n"));
    }
    let stride = w * 4;
    let size = stride * h;
    let conn = p.conn;
    let pool_fd = fs::memfd_create(0).map_err(|_| fail(75, b"wlx2: memfd_create failed\n"))?;
    fs::truncate(pool_fd, size as i64).map_err(|_| fail(76, b"wlx2: ftruncate failed\n"))?;
    let pool = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            size as usize,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            pool_fd,
            0,
        )
    };
    if pool == vmem::MAP_FAILED {
        return Err(fail(77, b"wlx2: mmap of the pool failed\n"));
    }
    for i in 0..(w * h) as usize {
        unsafe { core::ptr::write_volatile(pool.add(i * 4).cast::<u32>(), COLOR) };
    }
    uds::send_fds(conn, &[pool_fd]).map_err(|_| fail(78, b"wlx2: send_fds failed\n"))?;
    {
        let (shm, surface) = (p.shm, p.surface);
        let client = &mut p.client;
        let req = &mut p.req;
        let pool_obj = send_req(conn, req, |q| client.create_pool(shm, 0, size, q))?;
        let buffer = send_req(conn, req, |q| {
            client.create_buffer(
                pool_obj,
                0,
                w,
                h,
                stride,
                protocol::WL_SHM_FORMAT_ARGB8888,
                q,
            )
        })?;
        send_req(conn, req, |q| client.attach(surface, buffer, 0, 0, q))?;
        send_req(conn, req, |q| client.damage(surface, 0, 0, w, h, q))?;
        send_req(conn, req, |q| client.commit(surface, q))?;
    }
    let _ = unsafe { vmem::munmap(pool, size as usize) };
    let _ = fs::close(pool_fd);
    Ok(())
}

/// `/bin/wlx2`: the Phase 2c gate's client — two connections, one focus.
///
/// It maps a window on each; the second's appearance must take the focus, saying
/// `leave` to the first and `enter` to the second, and each connection must then be
/// released its own buffer. A key the host injects must reach the *focused*
/// connection. Then the first connection maps another window, which must move the
/// focus back, and a second key must follow it there.
pub fn wl_focus(_args: &[&str]) -> i32 {
    match run_focus() {
        Ok(()) => {
            write_out(b"wlx2: PASS\n");
            0
        }
        Err(code) => code,
    }
}

fn run_focus() -> Result<(), i32> {
    let a = peer_connect()?;
    let mut peers = [a, peer_connect()?];

    // The second window appeared last, so it must hold the focus and the first must
    // have been told it lost it.
    wait_peers(&mut peers, 400, |p| {
        p[0].configure.is_some() && p[1].configure.is_some() && p[0].leaves >= 1 && p[1].enters >= 1
    })?;

    // Present on both; each buffer must come back to the connection that owns it.
    for i in 0..2 {
        let before = peers[i].callbacks;
        peer_ack(&mut peers[i])?;
        wait_peers(&mut peers, 400, |p| p[i].callbacks > before)?;
        peer_map_buffer(&mut peers[i])?;
    }
    wait_peers(&mut peers, 400, |p| {
        p[0].releases >= 1 && p[1].releases >= 1
    })?;
    write_out(b"wlx2: ready\n");

    // The host injects a key: it must reach the focused connection, the second.
    let (a0, b0) = (peers[0].keys, peers[1].keys);
    wait_peers(&mut peers, 800, |p| p[0].keys + p[1].keys > a0 + b0)?;
    if peers[0].keys != a0 {
        return Err(fail(81, b"wlx2: the key reached the unfocused client\n"));
    }
    if peers[1].keys == b0 {
        return Err(fail(82, b"wlx2: no client got the key\n"));
    }
    write_out(b"wlx2: key ");
    put_dec(peers[1].last_key);
    write_out(b" to B\n");

    // Another window on the first connection must take the focus back.
    peer_map_window(&mut peers[0])?;
    wait_peers(&mut peers, 400, |p| p[0].enters >= 2 && p[1].leaves >= 1)?;
    write_out(b"wlx2: focus A\n");

    // And the second key must follow the focus.
    let (a1, b1) = (peers[0].keys, peers[1].keys);
    wait_peers(&mut peers, 800, |p| p[0].keys + p[1].keys > a1 + b1)?;
    if peers[1].keys != b1 {
        return Err(fail(83, b"wlx2: the second key stayed unfocused\n"));
    }
    if peers[0].keys == a1 {
        return Err(fail(84, b"wlx2: the second key went nowhere\n"));
    }
    write_out(b"wlx2: key ");
    put_dec(peers[0].last_key);
    write_out(b" to A\n");
    Ok(())
}

/// Wait for the toplevel's `configure`, answering the server's `ping`.
///
/// Returns the surface's configure serial and the size the toplevel was offered.
fn await_configure(
    conn: i32,
    client: &Client,
    xdg_surface: u32,
    toplevel: u32,
    wm_base: u32,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<(u32, i32, i32), i32> {
    let mut serial = None;
    let mut size = None;
    while serial.is_none() || size.is_none() {
        if !fill(conn, inbuf, chunk, 4000) {
            return Err(fail(58, b"wlx: no configure arrived\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(59, b"wlx: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(60, b"wlx: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let (kind, argc) = match client.decode(msg, &mut args) {
                Ok((_, k, n)) => (k, n),
                Err(_) => return Err(fail(61, b"wlx: undecodable event\n")),
            };
            let opcode = h.opcode;
            let object = h.object_id;
            // `configure`'s serial is a uint; the toplevel's w/h are ints in args 0/1.
            let a0u = args[0].as_uint();
            let i0 = args[0].as_int();
            let i1 = args[1].as_int();
            inbuf.consume(h.size as usize);
            match (kind, opcode) {
                (Kind::XdgSurface, protocol::xdg_surface_ev::CONFIGURE)
                    if object == xdg_surface =>
                {
                    if argc != 1 {
                        return Err(fail(62, b"wlx: configure arity\n"));
                    }
                    serial = a0u;
                }
                (Kind::XdgToplevel, protocol::xdg_toplevel_ev::CONFIGURE) if object == toplevel => {
                    if argc != 3 {
                        return Err(fail(63, b"wlx: toplevel configure arity\n"));
                    }
                    size = Some((i0.unwrap_or(0), i1.unwrap_or(0)));
                }
                (Kind::XdgWmBase, protocol::xdg_wm_base_ev::PING) => {
                    // A ping is answered `pong` with the same serial, or the server is
                    // entitled to treat this client as gone.
                    let mut pong = [0u8; 64];
                    let mut q = DispatchBuf::new(&mut pong);
                    client
                        .pong(wm_base, a0u.unwrap_or(0), &mut q)
                        .map_err(|_| fail(64, b"wlx: pong encode failed\n"))?;
                    if uds::send(conn, q.bytes()).is_err() {
                        return Err(fail(65, b"wlx: pong send failed\n"));
                    }
                }
                _ => {}
            }
        }
    }
    let (w, h) = size.unwrap_or((0, 0));
    Ok((serial.unwrap_or(0), w, h))
}

/// `/bin/wlx`: the Phase 2b `xdg_shell` client.
///
/// It binds `xdg_wm_base`, maps an `xdg_toplevel`, waits for the `configure` the
/// initial commit earns, acks it, draws a full frame at the configured size, and
/// commits it. The frame must then be on `/dev/fb`.
pub fn wl_xdg(_args: &[&str]) -> i32 {
    match run_xdg() {
        Ok(()) => {
            write_out(b"wlx: PASS\n");
            0
        }
        Err(code) => code,
    }
}

fn run_xdg() -> Result<(), i32> {
    // Map the framebuffer, so the frame can be read back the way 1b's client does.
    let fb_fd = unsafe { fs::open(b"/dev/fb", fs::O_RDWR, 0) }
        .map_err(|_| fail(50, b"wlx: open /dev/fb failed\n"))?;
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
        return Err(fail(51, b"wlx: mmap /dev/fb failed\n"));
    }

    let conn = connect()?;
    let mut client = Client::new();
    let mut req = [0u8; 512];
    let mut inbuf = MessageBuffer::new();
    let mut chunk = [0u8; 512];

    let registry = send_req(conn, &mut req, |q| client.get_registry(q))?;
    let g = find_globals(conn, &client, &mut inbuf, &mut chunk)?;
    if g.wm_base == 0 {
        return Err(fail(52, b"wlx: no xdg_wm_base global\n"));
    }
    let compositor = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            g.compositor,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let shm = send_req(conn, &mut req, |q| {
        client.bind(registry, g.shm, b"wl_shm", 1, Kind::Shm, q)
    })?;
    let wm_base = send_req(conn, &mut req, |q| {
        client.bind(registry, g.wm_base, b"xdg_wm_base", 1, Kind::XdgWmBase, q)
    })?;
    let surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;
    let xdg_surface = send_req(conn, &mut req, |q| {
        client.get_xdg_surface(wm_base, surface, q)
    })?;
    let toplevel = send_req(conn, &mut req, |q| client.get_toplevel(xdg_surface, q))?;
    send_req(conn, &mut req, |q| client.set_title(toplevel, b"wlx", q))?;
    send_req(conn, &mut req, |q| {
        client.set_app_id(toplevel, b"minixrs.wlx", q)
    })?;

    // The initial commit carries no buffer: it is the client saying "I am ready",
    // and the server answers it with the configuration it wants.
    send_req(conn, &mut req, |q| client.commit(surface, q))?;
    let (serial, width, height) = await_configure(
        conn,
        &client,
        xdg_surface,
        toplevel,
        wm_base,
        &mut inbuf,
        &mut chunk,
    )?;
    if width <= 0 || height <= 0 {
        return Err(fail(66, b"wlx: the server configured no size\n"));
    }
    write_out(b"wlx: configure ");
    put_dec(width as u32);
    write_out(b"x");
    put_dec(height as u32);
    write_out(b"\n");

    send_req(conn, &mut req, |q| {
        client.ack_configure(xdg_surface, serial, q)
    })?;
    // Drain the ack before the pool fd travels, for the reason 1b's `await_callback`
    // documents: the control message must land on the `create_pool` read.
    let barrier = send_req(conn, &mut req, |q| client.sync(q))?;
    await_callback(conn, &client, barrier, &mut inbuf, &mut chunk)?;

    // A buffer of exactly the configured size.
    let stride = width * 4;
    let size = stride * height;
    let pool_fd = fs::memfd_create(0).map_err(|_| fail(53, b"wlx: memfd_create failed\n"))?;
    fs::truncate(pool_fd, size as i64).map_err(|_| fail(54, b"wlx: ftruncate failed\n"))?;
    let pool = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            size as usize,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            pool_fd,
            0,
        )
    };
    if pool == vmem::MAP_FAILED {
        return Err(fail(55, b"wlx: mmap of the pool failed\n"));
    }
    for i in 0..(width * height) as usize {
        unsafe { core::ptr::write_volatile(pool.add(i * 4).cast::<u32>(), COLOR) };
    }
    uds::send_fds(conn, &[pool_fd]).map_err(|_| fail(56, b"wlx: send_fds failed\n"))?;
    let pool_obj = send_req(conn, &mut req, |q| client.create_pool(shm, 0, size, q))?;
    let buffer = send_req(conn, &mut req, |q| {
        client.create_buffer(
            pool_obj,
            0,
            width,
            height,
            stride,
            protocol::WL_SHM_FORMAT_ARGB8888,
            q,
        )
    })?;
    send_req(conn, &mut req, |q| client.attach(surface, buffer, 0, 0, q))?;
    send_req(conn, &mut req, |q| {
        client.damage(surface, 0, 0, width, height, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(surface, q))?;

    await_release(conn, &client, buffer, &mut inbuf, &mut chunk)?;

    // The frame must be on the display, at the size the toplevel asked for.
    let px = unsafe { core::ptr::read_volatile(fb.cast::<u32>()) };
    if px != COLOR {
        write_err(b"wlx: framebuffer pixel is 0x");
        print_hex(px);
        write_err(b", wanted 0x00FF00FF\n");
        return Err(fail(57, b"wlx: the frame did not reach /dev/fb\n"));
    }

    let _ = unsafe { vmem::munmap(pool, size as usize) };
    let _ = uds::close_fd(conn);
    Ok(())
}

/// Map `/dev/fb`, so a frame can be read back.
fn map_fb() -> Result<*mut u8, i32> {
    let fd = unsafe { fs::open(b"/dev/fb", fs::O_RDWR, 0) }
        .map_err(|_| fail(20, b"wlclient: open /dev/fb failed\n"))?;
    let fb = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            FB_MAP_LEN,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            fd,
            0,
        )
    };
    if fb == vmem::MAP_FAILED {
        return Err(fail(21, b"wlclient: mmap /dev/fb failed\n"));
    }
    Ok(fb)
}

/// One framebuffer pixel at (x, y).
fn read_px(fb: *mut u8, x: i32, y: i32) -> u32 {
    let off = y as usize * FB_PITCH + x as usize * 4;
    unsafe { core::ptr::read_volatile(fb.add(off).cast::<u32>()) }
}

/// Whether a colour appears anywhere on the framebuffer. Scanning rather than
/// picking a coordinate keeps a gate independent of where the pointer happens to be.
fn scan_for(fb: *mut u8, color: u32) -> bool {
    for i in 0..FB_WIDTH * FB_HEIGHT {
        if unsafe { core::ptr::read_volatile(fb.add(i * 4).cast::<u32>()) } == color {
            return true;
        }
    }
    false
}

/// Fill a mapped pool with one colour.
fn fill_pool(pool: *mut u8, pixels: i32, color: u32) {
    for i in 0..pixels as usize {
        unsafe { core::ptr::write_volatile(pool.add(i * 4).cast::<u32>(), color) };
    }
}

/// `/bin/wlxd`: the Phase 2d gate's client — damage and the pointer's cursor.
///
/// It draws a full red frame, then a blue buffer damaged only in a small rectangle:
/// red must remain everywhere else, which is what proves the compositor recomposited
/// only the damage. It then gives the pointer an 8x8 image and commits a window frame
/// again, so the cursor must appear over it.
pub fn wl_damage(_args: &[&str]) -> i32 {
    match run_damage() {
        Ok(()) => {
            write_out(b"wlxd: PASS\n");
            0
        }
        Err(code) => code,
    }
}

const A_COLOR: u32 = 0x00FF_0000;
const B_COLOR: u32 = 0x0000_00FF;
/// Opaque green: the cursor is `ARGB8888`, and a zero alpha would be skipped as
/// transparent by the compositor.
const C_COLOR: u32 = 0xFF00_FF00;

fn run_damage() -> Result<(), i32> {
    let fb = map_fb()?;
    let conn = connect()?;
    let mut client = Client::new();
    let mut req = [0u8; 512];
    let mut inbuf = MessageBuffer::new();
    let mut chunk = [0u8; 512];

    let registry = send_req(conn, &mut req, |q| client.get_registry(q))?;
    let g = find_globals(conn, &client, &mut inbuf, &mut chunk)?;
    let compositor = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            g.compositor,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let shm = send_req(conn, &mut req, |q| {
        client.bind(registry, g.shm, b"wl_shm", 1, Kind::Shm, q)
    })?;
    let seat = send_req(conn, &mut req, |q| {
        client.bind(registry, g.seat, b"wl_seat", 1, Kind::Seat, q)
    })?;
    let pointer = send_req(conn, &mut req, |q| client.get_pointer(seat, q))?;
    let surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;

    // The window: a full-screen buffer, so the whole output is under the client's
    // control and a pixel outside the damage means something.
    let stride = FB_WIDTH as i32 * 4;
    let size = stride * FB_HEIGHT as i32;
    let pool_fd = fs::memfd_create(0).map_err(|_| fail(95, b"wlxd: memfd_create failed\n"))?;
    fs::truncate(pool_fd, size as i64).map_err(|_| fail(96, b"wlxd: ftruncate failed\n"))?;
    let pool = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            size as usize,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            pool_fd,
            0,
        )
    };
    if pool == vmem::MAP_FAILED {
        return Err(fail(97, b"wlxd: mmap of the pool failed\n"));
    }
    fill_pool(pool, FB_WIDTH as i32 * FB_HEIGHT as i32, A_COLOR);
    uds::send_fds(conn, &[pool_fd]).map_err(|_| fail(98, b"wlxd: send_fds failed\n"))?;
    let pool_obj = send_req(conn, &mut req, |q| client.create_pool(shm, 0, size, q))?;
    let buffer = send_req(conn, &mut req, |q| {
        client.create_buffer(
            pool_obj,
            0,
            FB_WIDTH as i32,
            FB_HEIGHT as i32,
            stride,
            protocol::WL_SHM_FORMAT_XRGB8888,
            q,
        )
    })?;
    send_req(conn, &mut req, |q| client.attach(surface, buffer, 0, 0, q))?;
    send_req(conn, &mut req, |q| {
        client.damage(surface, 0, 0, FB_WIDTH as i32, FB_HEIGHT as i32, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(surface, q))?;
    await_release(conn, &client, buffer, &mut inbuf, &mut chunk)?;
    if read_px(fb, 0, 0) != A_COLOR {
        return Err(fail(99, b"wlxd: the first frame is not on /dev/fb\n"));
    }

    // Second frame: the buffer is now blue, but only a small rectangle is damaged.
    // Red must survive everywhere else — that is the whole claim of 2d.
    fill_pool(pool, FB_WIDTH as i32 * FB_HEIGHT as i32, B_COLOR);
    send_req(conn, &mut req, |q| {
        client.damage_buffer(surface, 16, 16, 8, 8, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(surface, q))?;
    await_release(conn, &client, buffer, &mut inbuf, &mut chunk)?;
    if read_px(fb, 0, 0) != A_COLOR {
        return Err(fail(
            100,
            b"wlxd: the whole frame was recomposited, not the damage\n",
        ));
    }
    if read_px(fb, 18, 18) != B_COLOR {
        return Err(fail(101, b"wlxd: the damage was not recomposited\n"));
    }

    // The pointer's image: an 8x8 opaque green buffer on its own surface.
    let cursurf = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;
    let csize = 8 * 8 * 4;
    let cfd = fs::memfd_create(0).map_err(|_| fail(102, b"wlxd: cursor memfd failed\n"))?;
    fs::truncate(cfd, csize as i64).map_err(|_| fail(103, b"wlxd: cursor truncate failed\n"))?;
    let cmap = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            csize as usize,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            cfd,
            0,
        )
    };
    if cmap == vmem::MAP_FAILED {
        return Err(fail(104, b"wlxd: mmap of the cursor pool failed\n"));
    }
    fill_pool(cmap, 64, C_COLOR);
    uds::send_fds(conn, &[cfd]).map_err(|_| fail(105, b"wlxd: cursor send_fds failed\n"))?;
    let cpool = send_req(conn, &mut req, |q| client.create_pool(shm, 0, csize, q))?;
    let cbuf = send_req(conn, &mut req, |q| {
        client.create_buffer(cpool, 0, 8, 8, 32, protocol::WL_SHM_FORMAT_ARGB8888, q)
    })?;
    send_req(conn, &mut req, |q| {
        client.set_cursor(pointer, 0, cursurf, 0, 0, q)
    })?;
    send_req(conn, &mut req, |q| client.attach(cursurf, cbuf, 0, 0, q))?;
    send_req(conn, &mut req, |q| client.commit(cursurf, q))?;

    // One more window frame, so the cursor is composited over it. The damage is well
    // away from both the first pixel and the pointer.
    send_req(conn, &mut req, |q| {
        client.damage(surface, 100, 100, 8, 8, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(surface, q))?;
    await_release(conn, &client, buffer, &mut inbuf, &mut chunk)?;
    if !scan_for(fb, C_COLOR) {
        return Err(fail(106, b"wlxd: the cursor was not drawn\n"));
    }

    let _ = unsafe { vmem::munmap(pool, size as usize) };
    let _ = unsafe { vmem::munmap(cmap, csize as usize) };
    let _ = uds::close_fd(conn);
    Ok(())
}

/// `/bin/wlxe`: the Phase 2e gate's client — panels, popups and decorations.
///
/// It maps an opaque window, then a `zwlr_layer_shell_v1` panel 200x32 and an
/// `xdg_popup` 8x8 over it, and requires each to be composited where the layer
/// surface asked and no wider: a panel that covered the whole output, or a popup
/// that did, would fail the rows below them. It also requires the decoration
/// manager to answer client-side, which is the only decoration this port draws.
pub fn wl_layer_popup_decoration(_args: &[&str]) -> i32 {
    match run_extra() {
        Ok(()) => {
            write_out(b"wlxe: PASS\n");
            0
        }
        Err(code) => code,
    }
}

/// The base window's colour (XRGB8888 opaque red, LE bytes B,G,R,0 = 00,00,FF,00).
const BASE_COLOR: u32 = 0x00FF_0000;
/// The panel's colour (opaque green).
const PANEL_COLOR: u32 = 0x0000_FF00;
/// The popup's colour (opaque blue).
const POPUP_COLOR: u32 = 0x0000_00FF;
const PANEL_W: i32 = 200;
const PANEL_H: i32 = 32;
const POPUP_W: i32 = 8;
const POPUP_H: i32 = 8;

/// Map a fresh pool of `size` bytes read-write, for a client to draw into.
fn map_pool(fd: i32, size: i32, code: i32, msg: &[u8]) -> Result<*mut u8, i32> {
    let p = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            size as usize,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            fd,
            0,
        )
    };
    if p == vmem::MAP_FAILED {
        return Err(fail(code, msg));
    }
    Ok(p)
}

fn run_extra() -> Result<(), i32> {
    let fb = map_fb()?;
    let conn = connect()?;
    let mut client = Client::new();
    let mut req = [0u8; 512];
    let mut inbuf = MessageBuffer::new();
    let mut chunk = [0u8; 512];

    let registry = send_req(conn, &mut req, |q| client.get_registry(q))?;
    let g = find_globals(conn, &client, &mut inbuf, &mut chunk)?;
    if g.wm_base == 0 {
        return Err(fail(160, b"wlxe: no xdg_wm_base global\n"));
    }
    if g.layer_shell == 0 {
        return Err(fail(161, b"wlxe: no zwlr_layer_shell_v1 global\n"));
    }
    if g.decoration_manager == 0 {
        return Err(fail(162, b"wlxe: no zxdg_decoration_manager_v1 global\n"));
    }
    let compositor = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            g.compositor,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let shm = send_req(conn, &mut req, |q| {
        client.bind(registry, g.shm, b"wl_shm", 1, Kind::Shm, q)
    })?;
    let wm_base = send_req(conn, &mut req, |q| {
        client.bind(registry, g.wm_base, b"xdg_wm_base", 1, Kind::XdgWmBase, q)
    })?;
    let layer_shell = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            g.layer_shell,
            b"zwlr_layer_shell_v1",
            1,
            Kind::LayerShell,
            q,
        )
    })?;
    let decorations = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            g.decoration_manager,
            b"zxdg_decoration_manager_v1",
            1,
            Kind::DecorationManager,
            q,
        )
    })?;

    // The base window: a full-screen opaque frame, so the panel and the popup have
    // something distinct to be drawn over.
    let surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;
    let xdg_surface = send_req(conn, &mut req, |q| {
        client.get_xdg_surface(wm_base, surface, q)
    })?;
    let toplevel = send_req(conn, &mut req, |q| client.get_toplevel(xdg_surface, q))?;
    send_req(conn, &mut req, |q| client.commit(surface, q))?;
    let (serial, width, height) = await_configure(
        conn,
        &client,
        xdg_surface,
        toplevel,
        wm_base,
        &mut inbuf,
        &mut chunk,
    )?;
    if width <= 0 || height <= 0 {
        return Err(fail(163, b"wlxe: the base window configured no size\n"));
    }
    send_req(conn, &mut req, |q| {
        client.ack_configure(xdg_surface, serial, q)
    })?;
    let barrier = send_req(conn, &mut req, |q| client.sync(q))?;
    await_callback(conn, &client, barrier, &mut inbuf, &mut chunk)?;

    let stride = width * 4;
    let size = stride * height;
    let pool_fd = fs::memfd_create(0).map_err(|_| fail(164, b"wlxe: memfd_create failed\n"))?;
    fs::truncate(pool_fd, size as i64).map_err(|_| fail(165, b"wlxe: truncate failed\n"))?;
    let pool = map_pool(pool_fd, size, 166, b"wlxe: mmap of the pool failed\n")?;
    fill_pool(pool, width * height, BASE_COLOR);
    uds::send_fds(conn, &[pool_fd]).map_err(|_| fail(167, b"wlxe: send_fds failed\n"))?;
    let pool_obj = send_req(conn, &mut req, |q| client.create_pool(shm, 0, size, q))?;
    let buffer = send_req(conn, &mut req, |q| {
        client.create_buffer(
            pool_obj,
            0,
            width,
            height,
            stride,
            protocol::WL_SHM_FORMAT_XRGB8888,
            q,
        )
    })?;
    send_req(conn, &mut req, |q| client.attach(surface, buffer, 0, 0, q))?;
    send_req(conn, &mut req, |q| {
        client.damage(surface, 0, 0, width, height, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(surface, q))?;
    await_release(conn, &client, buffer, &mut inbuf, &mut chunk)?;
    if read_px(fb, 4, 4) != BASE_COLOR {
        return Err(fail(168, b"wlxe: the base window is not on /dev/fb\n"));
    }

    // The decoration: this port draws none, so the manager must answer client-side
    // the moment it is asked.
    let decoration = send_req(conn, &mut req, |q| {
        client.get_toplevel_decoration(decorations, toplevel, q)
    })?;
    await_decoration(conn, &client, decoration, &mut inbuf, &mut chunk)?;

    // A panel: a layer surface, 200x32, anchored top. It is placed at the output's
    // origin, so it must cover the top-left pixel and leave the rows below it alone.
    let panel_surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;
    let panel = send_req(conn, &mut req, |q| {
        client.get_layer_surface(
            layer_shell,
            panel_surface,
            0,
            protocol::LAYER_TOP,
            b"wlxe-panel",
            q,
        )
    })?;
    send_req(conn, &mut req, |q| {
        client.set_layer_size(panel, PANEL_W as u32, PANEL_H as u32, q)
    })?;
    send_req(conn, &mut req, |q| {
        client.set_anchor(panel, protocol::LAYER_ANCHOR_TOP, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(panel_surface, q))?;
    let (p_serial, p_w, p_h) = await_layer_configure(conn, &client, panel, &mut inbuf, &mut chunk)?;
    if p_w != PANEL_W as u32 || p_h != PANEL_H as u32 {
        return Err(fail(
            169,
            b"wlxe: the panel was configured the wrong size\n",
        ));
    }
    send_req(conn, &mut req, |q| {
        client.ack_layer_configure(panel, p_serial, q)
    })?;
    let barrier = send_req(conn, &mut req, |q| client.sync(q))?;
    await_callback(conn, &client, barrier, &mut inbuf, &mut chunk)?;

    let pstride = PANEL_W * 4;
    let psize = pstride * PANEL_H;
    let pfd = fs::memfd_create(0).map_err(|_| fail(170, b"wlxe: panel memfd failed\n"))?;
    fs::truncate(pfd, psize as i64).map_err(|_| fail(171, b"wlxe: panel truncate failed\n"))?;
    let pmap = map_pool(pfd, psize, 172, b"wlxe: mmap of the panel pool failed\n")?;
    fill_pool(pmap, PANEL_W * PANEL_H, PANEL_COLOR);
    uds::send_fds(conn, &[pfd]).map_err(|_| fail(173, b"wlxe: panel send_fds failed\n"))?;
    let ppool = send_req(conn, &mut req, |q| client.create_pool(shm, 0, psize, q))?;
    let pbuf = send_req(conn, &mut req, |q| {
        client.create_buffer(
            ppool,
            0,
            PANEL_W,
            PANEL_H,
            pstride,
            protocol::WL_SHM_FORMAT_XRGB8888,
            q,
        )
    })?;
    send_req(conn, &mut req, |q| {
        client.attach(panel_surface, pbuf, 0, 0, q)
    })?;
    send_req(conn, &mut req, |q| {
        client.damage(panel_surface, 0, 0, PANEL_W, PANEL_H, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(panel_surface, q))?;
    await_release(conn, &client, pbuf, &mut inbuf, &mut chunk)?;
    if read_px(fb, 4, 4) != PANEL_COLOR {
        return Err(fail(174, b"wlxe: the panel is not on /dev/fb\n"));
    }
    if read_px(fb, 4, PANEL_H + 8) != BASE_COLOR {
        return Err(fail(175, b"wlxe: the panel covered more than its size\n"));
    }

    // A popup: positioner-sized at 8x8, over the base window. Drawn after the panel,
    // so it must cover the panel's top-left corner and no more.
    let popup_surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;
    let popup_xdg = send_req(conn, &mut req, |q| {
        client.get_xdg_surface(wm_base, popup_surface, q)
    })?;
    let positioner = send_req(conn, &mut req, |q| client.create_positioner(wm_base, q))?;
    send_req(conn, &mut req, |q| {
        client.set_positioner_size(positioner, POPUP_W, POPUP_H, q)
    })?;
    send_req(conn, &mut req, |q| {
        client.get_popup(popup_xdg, xdg_surface, positioner, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(popup_surface, q))?;
    let popup_serial =
        await_popup_configure(conn, &client, popup_xdg, wm_base, &mut inbuf, &mut chunk)?;
    send_req(conn, &mut req, |q| {
        client.ack_configure(popup_xdg, popup_serial, q)
    })?;
    let barrier = send_req(conn, &mut req, |q| client.sync(q))?;
    await_callback(conn, &client, barrier, &mut inbuf, &mut chunk)?;

    let ustride = POPUP_W * 4;
    let usize_ = ustride * POPUP_H;
    let ufd = fs::memfd_create(0).map_err(|_| fail(176, b"wlxe: popup memfd failed\n"))?;
    fs::truncate(ufd, usize_ as i64).map_err(|_| fail(177, b"wlxe: popup truncate failed\n"))?;
    let umap = map_pool(ufd, usize_, 178, b"wlxe: mmap of the popup pool failed\n")?;
    fill_pool(umap, POPUP_W * POPUP_H, POPUP_COLOR);
    uds::send_fds(conn, &[ufd]).map_err(|_| fail(179, b"wlxe: popup send_fds failed\n"))?;
    let upool = send_req(conn, &mut req, |q| client.create_pool(shm, 0, usize_, q))?;
    let ubuf = send_req(conn, &mut req, |q| {
        client.create_buffer(
            upool,
            0,
            POPUP_W,
            POPUP_H,
            ustride,
            protocol::WL_SHM_FORMAT_XRGB8888,
            q,
        )
    })?;
    send_req(conn, &mut req, |q| {
        client.attach(popup_surface, ubuf, 0, 0, q)
    })?;
    send_req(conn, &mut req, |q| {
        client.damage(popup_surface, 0, 0, POPUP_W, POPUP_H, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(popup_surface, q))?;
    await_release(conn, &client, ubuf, &mut inbuf, &mut chunk)?;
    if read_px(fb, 4, 4) != POPUP_COLOR {
        return Err(fail(180, b"wlxe: the popup is not on /dev/fb\n"));
    }
    if read_px(fb, 4, POPUP_H + 4) != PANEL_COLOR {
        return Err(fail(181, b"wlxe: the popup covered more than its size\n"));
    }

    let _ = unsafe { vmem::munmap(pool, size as usize) };
    let _ = unsafe { vmem::munmap(pmap, psize as usize) };
    let _ = unsafe { vmem::munmap(umap, usize_ as usize) };
    let _ = uds::close_fd(conn);
    Ok(())
}

/// Wait for a layer surface's `configure`. Returns its serial and its size.
fn await_layer_configure(
    conn: i32,
    client: &Client,
    layer_surface: u32,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<(u32, u32, u32), i32> {
    loop {
        if !fill(conn, inbuf, chunk, 4000) {
            return Err(fail(185, b"wlxe: no layer configure arrived\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(186, b"wlxe: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(187, b"wlxe: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let (kind, argc) = match client.decode(msg, &mut args) {
                Ok((_, k, n)) => (k, n),
                Err(_) => return Err(fail(188, b"wlxe: undecodable event\n")),
            };
            if kind == Kind::Display && h.opcode == protocol::display_ev::ERROR {
                return Err(fail(189, b"wlxe: server sent wl_display.error\n"));
            }
            let vals = if kind == Kind::LayerSurface
                && h.object_id == layer_surface
                && h.opcode == protocol::layer_surface_ev::CONFIGURE
                && argc == 3
            {
                Some((
                    args[0].as_uint().unwrap_or(0),
                    args[1].as_uint().unwrap_or(0),
                    args[2].as_uint().unwrap_or(0),
                ))
            } else {
                None
            };
            inbuf.consume(h.size as usize);
            if let Some(v) = vals {
                return Ok(v);
            }
        }
    }
}

/// Wait for a popup's `xdg_surface.configure`, answering the server's `ping`.
///
/// A popup has no toplevel, so unlike a window's configure there is no size to
/// collect — the serial the server expects to be acked is the whole event.
fn await_popup_configure(
    conn: i32,
    client: &Client,
    xdg_surface: u32,
    wm_base: u32,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<u32, i32> {
    loop {
        if !fill(conn, inbuf, chunk, 4000) {
            return Err(fail(190, b"wlxe: no popup configure arrived\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(191, b"wlxe: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(192, b"wlxe: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let kind = match client.decode(msg, &mut args) {
                Ok((_, k, _)) => k,
                Err(_) => return Err(fail(193, b"wlxe: undecodable event\n")),
            };
            let a0u = args[0].as_uint();
            inbuf.consume(h.size as usize);
            if kind == Kind::Display && h.opcode == protocol::display_ev::ERROR {
                return Err(fail(194, b"wlxe: server sent wl_display.error\n"));
            }
            if kind == Kind::XdgSurface
                && h.object_id == xdg_surface
                && h.opcode == protocol::xdg_surface_ev::CONFIGURE
            {
                return Ok(a0u.unwrap_or(0));
            }
            if kind == Kind::XdgWmBase && h.opcode == protocol::xdg_wm_base_ev::PING {
                let mut pong = [0u8; 64];
                let mut q = DispatchBuf::new(&mut pong);
                client
                    .pong(wm_base, a0u.unwrap_or(0), &mut q)
                    .map_err(|_| fail(195, b"wlxe: pong encode failed\n"))?;
                if uds::send(conn, q.bytes()).is_err() {
                    return Err(fail(196, b"wlxe: pong send failed\n"));
                }
            }
        }
    }
}

/// Wait for the decoration manager's answer and require client-side decoration.
fn await_decoration(
    conn: i32,
    client: &Client,
    decoration: u32,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<(), i32> {
    loop {
        if !fill(conn, inbuf, chunk, 2000) {
            return Err(fail(197, b"wlxe: no decoration answer arrived\n"));
        }
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(198, b"wlxe: bad frame\n")),
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => return Err(fail(199, b"wlxe: bad header\n")),
            };
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let kind = match client.decode(msg, &mut args) {
                Ok((_, k, _)) => k,
                Err(_) => return Err(fail(200, b"wlxe: undecodable event\n")),
            };
            let mode = if kind == Kind::ToplevelDecoration
                && h.object_id == decoration
                && h.opcode == protocol::toplevel_decoration_ev::CONFIGURE
            {
                args[0].as_uint()
            } else {
                None
            };
            inbuf.consume(h.size as usize);
            match mode {
                Some(m) if m == protocol::DECORATION_MODE_CLIENT_SIDE => return Ok(()),
                Some(_) => return Err(fail(201, b"wlxe: the manager chose server-side\n")),
                None => {}
            }
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
