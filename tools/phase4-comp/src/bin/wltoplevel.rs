//! `/bin/wltoplevel`: a stock `wayland-client` window.
//!
//! The client half of the compositor's `xdg_shell` and pointer support. It creates a toplevel,
//! waits for the configure that lets it map a buffer, and then waits for the seat's keyboard and
//! pointer: `enter`, a motion, and a button. The host injects those with QMP once this client
//! says it is waiting, so a pass covers the whole chain from the input ring through smithay's
//! seat to the protocol the client reads.
//!
//! The frame is one buffer, a body colour with a band on its top rows, so the gate's screendump
//! can also see whether the present path kept the frame upright.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const SOCKET: &str = "/run/wlcomp";
const WIDTH: i32 = 1024;
const HEIGHT: i32 = 768;
const STRIDE: i32 = WIDTH * 4;
const FRAME: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// `Xrgb8888`, little-endian: a body colour and a band on its top rows, distinct from the other
/// gates' colours so a stale frame cannot pass.
const COLOUR_BODY: u32 = 0x0010_4080;
const COLOUR_BAND: u32 = 0x00F0_E000;
const BAND_ROWS: usize = 100;

#[derive(Default)]
struct Client {
    /// The last `xdg_surface.configure` serial, or 0 before the first one.
    configure: u32,
    /// The size the toplevel was configured at; zero means "the client chooses".
    configured_size: (i32, i32),
    keyboard_entered: bool,
    pointer_entered: bool,
    motion: (f64, f64),
    moved: bool,
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
                    println!("wltoplevel: seat has a keyboard");
                }
                if caps.contains(wl_seat::Capability::Pointer) {
                    println!("wltoplevel: seat has a pointer");
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
            wl_keyboard::Event::Enter { .. } => {
                state.keyboard_entered = true;
                println!("wltoplevel: keyboard enter");
            }
            wl_keyboard::Event::Keymap { .. } => println!("wltoplevel: keymap"),
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_entered = true;
                state.motion = (surface_x, surface_y);
                println!("wltoplevel: pointer enter at {surface_x},{surface_y}");
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.motion = (surface_x, surface_y);
                state.moved = true;
                println!("wltoplevel: pointer motion at {surface_x},{surface_y}");
            }
            wl_pointer::Event::Button {
                button,
                state: which,
                ..
            } => {
                println!("wltoplevel: pointer button {button} {which:?}");
                if button == 0x110
                    && which == wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed)
                {
                    state.pressed = true;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _state: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            state.configure = serial;
            surface.ack_configure(serial);
            println!("wltoplevel: configure {serial}");
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _toplevel: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state.configured_size = (width, height);
            println!("wltoplevel: toplevel configure {width}x{height}");
        }
    }
}

/// One frame: the body colour with a band on its top rows, written through the pool's mapping.
fn fill(ptr: *mut u32) {
    // SAFETY: the mapping is `FRAME` bytes and `u32`-aligned, as `mmap` returns it.
    let pixels = unsafe { core::slice::from_raw_parts_mut(ptr, FRAME / 4) };
    pixels.fill(COLOUR_BODY);
    pixels[..BAND_ROWS * WIDTH as usize].fill(COLOUR_BAND);
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
    let wm_base: xdg_wm_base::XdgWmBase = globals
        .bind(&qh, 1..=5, ())
        .map_err(|e| format!("could not bind xdg_wm_base: {e:?}"))?;

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
    fill(ptr as *mut u32);

    // SAFETY: the fd came from `memfd_create`, and the mapping keeps the object alive after close.
    let pool_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let pool = shm.create_pool(pool_fd.as_fd(), FRAME as i32, &qh, ());
    let buffer = pool.create_buffer(0, WIDTH, HEIGHT, STRIDE, wl_shm::Format::Xrgb8888, &qh, ());

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("wltoplevel".to_string());
    // No buffer: a commit that only asks for a configure, which is how a client learns it may map.
    surface.commit();

    let _keyboard: wl_keyboard::WlKeyboard = seat.get_keyboard(&qh, ());
    let _pointer: wl_pointer::WlPointer = seat.get_pointer(&qh, ());

    while state.configure == 0 {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("waiting for a configure failed: {e:?}"))?;
    }

    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("roundtrip after the first frame failed: {e:?}"))?;
    println!(
        "wltoplevel: mapped at {}x{}",
        state.configured_size.0, state.configured_size.1
    );
    println!("wltoplevel: waiting for input");

    // The host drives the tablet here: motion and a left click, which the seat must deliver.
    while !(state.keyboard_entered && state.pointer_entered && state.moved && state.pressed) {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("waiting for input failed: {e:?}"))?;
    }

    println!(
        "wltoplevel: PASS configured, keyboard focused, pointer entered and clicked at {},{}",
        state.motion.0, state.motion.1
    );
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            println!("wltoplevel: FAIL {e}");
            std::process::exit(1);
        }
    }
}
