//! Phase 4d-3's gate client: a stock `wayland-client` on smithay's seat.
//!
//! It binds `wl_seat`, takes its `wl_keyboard`, reads the keymap the seat serves, commits one
//! `wl_shm` frame (so the compositor has a mapped surface to focus), waits for `enter`, and then
//! for a `key`. The host injects `a` with QMP once this client says it is waiting; the evdev code
//! the event carries must be 30, and the keymap must have arrived and been readable.
//!
//! Everything here is the protocol's own path: the key is translated by the input server, turned
//! into an xkb keycode and forwarded by smithay's `KeyboardHandle`, and serialized back to evdev
//! for the client. A match therefore proves the whole 4d-3 chain, not this binary.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};

const SOCKET: &str = "/run/wlcomp";
const WIDTH: i32 = 1024;
const HEIGHT: i32 = 768;
const STRIDE: i32 = WIDTH * 4;
const FRAME: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// `Xrgb8888`, little-endian: a blue field, distinct from the 4d-2 test's magenta.
const COLOUR: u32 = 0x00C0_3020;

#[derive(Default)]
struct Client {
    keymap: usize,
    entered: bool,
    pressed: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Client {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Client {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if let wayland_client::WEnum::Value(caps) = capabilities {
                if caps.contains(wl_seat::Capability::Keyboard) {
                    println!("wlseat: seat has a keyboard");
                }
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                state.keymap = read_keymap(&fd, size as usize);
                println!("wlseat: keymap {} ok", state.keymap);
            }
            wl_keyboard::Event::Enter { .. } => {
                state.entered = true;
                println!("wlseat: enter");
            }
            wl_keyboard::Event::Key {
                key, state: which, ..
            } => {
                println!("wlseat: key {key} {which:?}");
                if key == 30
                    && which == wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed)
                {
                    state.pressed = true;
                }
            }
            wl_keyboard::Event::Leave { .. } => println!("wlseat: leave"),
            _ => {}
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for Client {
    fn event(
        _state: &mut Self,
        _shm: &wl_shm::WlShm,
        _event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for Client {
    fn event(
        _state: &mut Self,
        _pool: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Client {
    fn event(
        _state: &mut Self,
        _buffer: &wl_buffer::WlBuffer,
        _event: wl_buffer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for Client {
    fn event(
        _state: &mut Self,
        _compositor: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Client {
    fn event(
        _state: &mut Self,
        _surface: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

/// Seeks the keymap's descriptor to its start and reads it. The seat serves it from a file — this
/// port has no `shm_open`, so smithay's sealed-file path cannot be built and it writes a temporary
/// file instead — and the writer leaves the *shared* open file description at the end, so the
/// offset is reset here before reading.
fn read_keymap(fd: &OwnedFd, size: usize) -> usize {
    if size == 0 {
        return 0;
    }
    if let Err(e) = minix_std::fs::lseek(fd.as_raw_fd(), 0, minix_std::fs::SEEK_SET) {
        println!("wlseat: keymap seek failed: errno {}", e.0);
        return 0;
    }
    let mut buf = vec![0u8; size];
    let mut got = 0usize;
    while got < buf.len() {
        // SAFETY: the descriptor is open for reading and the slice is writable for its length.
        match unsafe { minix_std::fs::read(fd.as_raw_fd(), &mut buf[got..]) } {
            Ok(0) => break,
            Ok(n) => got += n as usize,
            Err(e) => {
                println!("wlseat: keymap read failed: errno {}", e.0);
                return 0;
            }
        }
    }
    got
}

fn run() -> Result<(), String> {
    let stream = std::os::minix::net::UnixStream::connect(SOCKET)
        .map_err(|e| format!("connect {SOCKET} failed: {e}"))?;
    let conn = Connection::from_socket(stream).map_err(|e| format!("connection failed: {e:?}"))?;
    let (globals, mut queue) =
        registry_queue_init::<Client>(&conn).map_err(|e| format!("registry init failed: {e:?}"))?;
    let qh = queue.handle();
    let mut state = Client::default();

    let shm: wl_shm::WlShm = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("could not bind wl_shm: {e:?}"))?;
    let compositor: wl_compositor::WlCompositor = globals
        .bind(&qh, 4..=4, ())
        .map_err(|e| format!("could not bind wl_compositor: {e:?}"))?;
    let seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=5, ())
        .map_err(|e| format!("could not bind wl_seat: {e:?}"))?;
    let _keyboard: wl_keyboard::WlKeyboard = seat.get_keyboard(&qh, ());

    // One frame, so the compositor has a mapped surface to hand the keyboard to.
    let fd = minix_std::fs::memfd_create(0).map_err(|e| format!("memfd_create failed: {e:?}"))?;
    minix_std::fs::truncate(fd, FRAME as i64).map_err(|e| format!("ftruncate failed: {e:?}"))?;
    // SAFETY: a fresh anonymous object of exactly this length, mapped read-write and shared.
    let ptr = unsafe {
        minix_rt::vmem::mmap(
            core::ptr::null_mut(),
            FRAME,
            minix_rt::vmem::PROT_READ | minix_rt::vmem::PROT_WRITE,
            minix_rt::vmem::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == minix_rt::vmem::MAP_FAILED {
        return Err("mmap of the pool failed".to_string());
    }
    // SAFETY: the mapping is `FRAME` bytes and `u32`-aligned, as `mmap` returns it.
    let pixels = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u32, FRAME / 4) };
    pixels.fill(COLOUR);

    // SAFETY: the fd came from `memfd_create`, and the mapping keeps the object alive after close.
    let pool_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let pool = shm.create_pool(pool_fd.as_fd(), FRAME as i32, &qh, ());
    let buffer = pool.create_buffer(0, WIDTH, HEIGHT, STRIDE, wl_shm::Format::Xrgb8888, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();

    // The roundtrip returns after the compositor has dispatched the commit — which is when it
    // gives this surface the keyboard — so the `enter` is in this batch.
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("roundtrip failed: {e:?}"))?;
    println!("wlseat: waiting for a key");

    while !state.pressed {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("dispatch failed: {e:?}"))?;
    }

    if state.keymap == 0 {
        return Err("the keymap event carried no readable keymap".to_string());
    }
    if !state.entered {
        return Err("no keyboard enter".to_string());
    }
    println!("wlseat: PASS keymap and a key through smithay's seat");
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            println!("wlseat: FAIL {e}");
            std::process::exit(1);
        }
    }
}
