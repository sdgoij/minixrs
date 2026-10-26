//! Phase 4d-2's client: a stock `wayland-client` that commits two whole-output `wl_shm` frames
//! and waits for the first one's `wl_buffer.release`.
//!
//! The release is the interesting half. smithay sends it when the second commit replaces the
//! first buffer, so seeing it means the compositor took the buffer, held it while it drew, and
//! gave it back, the client-side half of 4d-2's gate. The other half is the display: buffer B
//! is what remains on the scan-out, and the host's screendump has to be its colour.
//!
//! B is not one colour. Its top 100 rows are green and the rest magenta, which is what lets the
//! gate tell an upright present from a flipped one: a single-colour frame looks the same either
//! way, and a flipped frame did pass this gate before the band was added.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};

const SOCKET: &str = "/run/wlcomp";
const WIDTH: i32 = 1024;
const HEIGHT: i32 = 768;
const STRIDE: i32 = WIDTH * 4;
const FRAME: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// `Xrgb8888` pixels, little-endian: `0x00RRGGBB`. A is green, B is magenta, and B is what the
/// display must show once both commits have been drawn.
const COLOUR_A: u32 = 0x0000_FF00;
const COLOUR_B: u32 = 0x00FF_00FF;
/// Frame B's first rows, in the client's own order, so the gate can see whether the present path
/// kept the frame upright.
const COLOUR_BAND: u32 = 0x0000_FF00;
const BAND_ROWS: usize = 100;

#[derive(Default)]
struct Client {
    released: u32,
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
        state: &mut Self,
        _buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            println!("wlshm: wl_buffer released");
            state.released += 1;
        }
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

    // One pool, two frames: A at offset 0 and B after it. The frames are written through a
    // shared mapping, which is the same object the compositor receives the descriptor for — so
    // no `write(2)` and no offset bookkeeping (`.notes/wayland.md` §6.4).
    let fd = minix_std::fs::memfd_create(0).map_err(|e| format!("memfd_create failed: {e:?}"))?;
    let pool_len = FRAME * 2;
    minix_std::fs::truncate(fd, pool_len as i64).map_err(|e| format!("ftruncate failed: {e:?}"))?;
    // SAFETY: a fresh anonymous object of exactly this length, mapped read-write and shared.
    let ptr = unsafe {
        minix_rt::vmem::mmap(
            core::ptr::null_mut(),
            pool_len,
            minix_rt::vmem::PROT_READ | minix_rt::vmem::PROT_WRITE,
            minix_rt::vmem::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == minix_rt::vmem::MAP_FAILED {
        return Err("mmap of the pool failed".to_string());
    }
    // SAFETY: the mapping is `pool_len` bytes and `u32`-aligned, as `mmap` returns it.
    let pixels = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u32, pool_len / 4) };
    pixels[..FRAME / 4].fill(COLOUR_A);
    pixels[FRAME / 4..].fill(COLOUR_B);
    let band = FRAME / 4 + BAND_ROWS * WIDTH as usize;
    pixels[FRAME / 4..band].fill(COLOUR_BAND);

    // SAFETY: the fd came from `memfd_create` and the mapping keeps the object alive after close.
    let pool_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let pool = shm.create_pool(pool_fd.as_fd(), pool_len as i32, &qh, ());
    let a = pool.create_buffer(0, WIDTH, HEIGHT, STRIDE, wl_shm::Format::Xrgb8888, &qh, ());
    let b = pool.create_buffer(
        FRAME as i32,
        WIDTH,
        HEIGHT,
        STRIDE,
        wl_shm::Format::Xrgb8888,
        &qh,
        (),
    );

    let surface = compositor.create_surface(&qh, ());

    // Commit A and wait for the server: the roundtrip is answered after the commit handler has
    // drawn, so A has reached the scan-out by the time the next line runs.
    surface.attach(Some(&a), 0, 0);
    surface.commit();
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("roundtrip after A failed: {e:?}"))?;
    println!("wlshm: committed A");

    // B replaces A. smithay releases A as part of applying this commit's state.
    surface.attach(Some(&b), 0, 0);
    surface.commit();
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("roundtrip after B failed: {e:?}"))?;
    println!("wlshm: committed B");

    if state.released == 0 {
        return Err("A's wl_buffer was never released".to_string());
    }
    println!("wlshm: PASS two frames committed, A's buffer released");
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            println!("wlshm: FAIL {e}");
            std::process::exit(1);
        }
    }
}
