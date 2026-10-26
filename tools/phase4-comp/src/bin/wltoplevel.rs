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
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};

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
/// The popup's size and place inside the toplevel, and its colour: white, so the gate can tell
/// the two surfaces apart.
const POPUP_W: i32 = 200;
const POPUP_H: i32 = 100;
const POPUP_X: i32 = 100;
const POPUP_Y: i32 = 50;
const COLOUR_POPUP: u32 = 0x00FF_FFFF;

/// The user-data marker on the popup's `xdg_surface`, so its configure reaches its own handler.
struct PopupTag;

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
    /// The popup's `xdg_surface.configure` serial, or 0 before it.
    popup_configure: u32,
    /// The decoration mode the compositor answered with, once the client asked for one.
    decoration: Option<zxdg_toplevel_decoration_v1::Mode>,
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

impl Dispatch<xdg_positioner::XdgPositioner, ()> for Client {
    fn event(
        _state: &mut Self,
        _positioner: &xdg_positioner::XdgPositioner,
        _event: xdg_positioner::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_surface::XdgSurface, PopupTag> for Client {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _data: &PopupTag,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            state.popup_configure = serial;
            surface.ack_configure(serial);
            println!("wltoplevel: popup configure {serial}");
        }
    }
}

impl Dispatch<xdg_popup::XdgPopup, ()> for Client {
    fn event(
        _state: &mut Self,
        _popup: &xdg_popup::XdgPopup,
        event: xdg_popup::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::Configure {
            x,
            y,
            width,
            height,
        } = event
        {
            println!("wltoplevel: popup at {x},{y} {width}x{height}");
        }
    }
}

impl Dispatch<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1, ()> for Client {
    fn event(
        _state: &mut Self,
        _manager: &zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
        _event: zxdg_decoration_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1, ()> for Client {
    fn event(
        state: &mut Self,
        _decoration: &zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event {
            if let wayland_client::WEnum::Value(mode) = mode {
                state.decoration = Some(mode);
                println!("wltoplevel: decoration mode {mode:?}");
            }
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
    // Ask for decorations. This compositor draws no title bars, so it answers client-side.
    let decoration_manager: zxdg_decoration_manager_v1::ZxdgDecorationManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("could not bind zxdg_decoration_manager_v1: {e:?}"))?;
    let _decoration = decoration_manager.get_toplevel_decoration(&toplevel, &qh, ());
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

    // A popup, placed by the compositor from this positioner's geometry rather than by anything
    // the client says after the fact. Its own `xdg_surface` carries `PopupTag`, so its configure
    // lands in `popup_configure` and not in the toplevel's.
    let positioner = wm_base.create_positioner(&qh, ());
    positioner.set_size(POPUP_W, POPUP_H);
    positioner.set_anchor_rect(POPUP_X, POPUP_Y, 1, 1);
    // Anchor at the rect's top-left and grow towards the bottom-right, so the popup's top-left
    // *is* the anchor point. With the defaults (both unset) the protocol centres the popup over
    // the anchor point, which lands it at 0,0 here because the size is halved.
    positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
    positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
    let popup_surface = compositor.create_surface(&qh, ());
    let popup_xdg_surface = wm_base.get_xdg_surface(&popup_surface, &qh, PopupTag);
    let _popup = popup_xdg_surface.get_popup(Some(&xdg_surface), &positioner, &qh, ());
    positioner.destroy();
    popup_surface.commit();
    while state.popup_configure == 0 {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("waiting for the popup's configure failed: {e:?}"))?;
    }

    let popup_len = (POPUP_W * POPUP_H * 4) as usize;
    let popup_fd =
        minix_std::fs::memfd_create(0).map_err(|e| format!("memfd_create failed: {e:?}"))?;
    minix_std::fs::truncate(popup_fd, popup_len as i64)
        .map_err(|e| format!("ftruncate failed: {e:?}"))?;
    // SAFETY: a fresh anonymous object of exactly this length, mapped read-write and shared.
    let popup_ptr = unsafe {
        minix_rt::vmem::mmap(
            core::ptr::null_mut(),
            popup_len,
            minix_rt::vmem::PROT_READ | minix_rt::vmem::PROT_WRITE,
            minix_rt::vmem::MAP_SHARED,
            popup_fd,
            0,
        )
    };
    if popup_ptr == minix_rt::vmem::MAP_FAILED {
        return Err("mmap of the popup's pool failed".to_string());
    }
    // SAFETY: the mapping is `popup_len` bytes and `u32`-aligned, as `mmap` returns it.
    unsafe { core::slice::from_raw_parts_mut(popup_ptr as *mut u32, popup_len / 4) }
        .fill(COLOUR_POPUP);
    // SAFETY: the fd came from `memfd_create`, and the mapping keeps the object alive after close.
    let popup_pool_fd = unsafe { OwnedFd::from_raw_fd(popup_fd) };
    let popup_pool = shm.create_pool(popup_pool_fd.as_fd(), popup_len as i32, &qh, ());
    let popup_buffer = popup_pool.create_buffer(
        0,
        POPUP_W,
        POPUP_H,
        POPUP_W * 4,
        wl_shm::Format::Xrgb8888,
        &qh,
        (),
    );
    popup_surface.attach(Some(&popup_buffer), 0, 0);
    popup_surface.commit();
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("roundtrip after the popup failed: {e:?}"))?;
    println!("wltoplevel: popup mapped");

    println!("wltoplevel: waiting for input");

    // The host drives the tablet here: motion and a left click, which the seat must deliver.
    while !(state.keyboard_entered && state.pointer_entered && state.moved && state.pressed) {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("waiting for input failed: {e:?}"))?;
    }

    if state.popup_configure == 0 {
        return Err("the popup was never configured".to_string());
    }
    if state.decoration.is_none() {
        return Err("the compositor answered no decoration mode".to_string());
    }

    println!(
        "wltoplevel: PASS configured, decorated {:?}, popup at {},{} placed, keyboard focused, pointer entered and clicked at {},{}",
        state.decoration, POPUP_X, POPUP_Y, state.motion.0, state.motion.1
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
