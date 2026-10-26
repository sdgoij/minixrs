//! `/bin/wldmabuf`: a stock `wayland-client` that commits a `zwp_linux_dmabuf_v1` buffer, which
//! is the client half of the dma-buf present path (`.notes/wayland.md` 6.13).
//!
//! The buffer is a `virtgpu` object on the render node, filled here and handed over as a
//! descriptor (`src/dmabuf.rs`), so the compositor's pixels are the client's object and neither
//! side copies the frame. Two objects are committed in turn, the way `/bin/wlshm` commits two
//! `wl_shm` frames: the second replaces the first, and the release of the first is what proves
//! the compositor held it while it drew.
//!
//! B is not one colour. Its top 100 rows are yellow and the rest blue, so the gate can tell
//! upright from flipped and one client from another.

#[path = "../dmabuf.rs"]
mod dmabuf;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};

const SOCKET: &str = "/run/wlcomp";
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;

/// `DRM_FORMAT_XRGB8888`, which the node's `B8G8R8X8` object holds byte for byte.
const FORMAT_XRGB8888: u32 = 0x3432_5258;

/// Pixels as `Xrgb8888`: `0x00RRGGBB`. A is green, B is blue with a yellow top band.
const COLOUR_A: u32 = 0x0000_FF00;
const COLOUR_B: u32 = 0x0000_00FF;
const COLOUR_BAND: u32 = 0x00FF_FF00;
const BAND_ROWS: usize = 100;

#[derive(Default)]
struct Client {
    released: u32,
    formats: u32,
    modifiers: u32,
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

impl Dispatch<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, ()> for Client {
    fn event(
        state: &mut Self,
        _dmabuf: &zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
        event: zwp_linux_dmabuf_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_linux_dmabuf_v1::Event::Format { .. } => state.formats += 1,
            zwp_linux_dmabuf_v1::Event::Modifier { .. } => state.modifiers += 1,
            _ => {}
        }
    }
}

impl Dispatch<zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, ()> for Client {
    fn event(
        _state: &mut Self,
        _params: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        _event: zwp_linux_buffer_params_v1::Event,
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
            println!("wldmabuf: wl_buffer released");
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

/// One full-output object, filled row by row. `band` paints the top rows a second colour.
fn make(body: u32, band: Option<u32>) -> Result<dmabuf::Owned, String> {
    dmabuf::create(WIDTH, HEIGHT, |pixels| {
        let band_end = BAND_ROWS * WIDTH as usize;
        for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
            let colour = match band {
                Some(band) if i < band_end => band,
                _ => body,
            };
            px.copy_from_slice(&colour.to_le_bytes());
        }
    })
}

fn run() -> Result<(), String> {
    let stream = std::os::minix::net::UnixStream::connect(SOCKET)
        .map_err(|e| format!("connect {SOCKET} failed: {e}"))?;
    let conn = Connection::from_socket(stream).map_err(|e| format!("connection failed: {e:?}"))?;
    let (globals, mut queue) =
        registry_queue_init::<Client>(&conn).map_err(|e| format!("registry init failed: {e:?}"))?;
    let qh = queue.handle();
    let mut state = Client::default();

    let compositor: wl_compositor::WlCompositor = globals
        .bind(&qh, 1..=4, ())
        .map_err(|e| format!("could not bind wl_compositor: {e:?}"))?;
    let dmabuf: zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1 = globals
        .bind(&qh, 1..=3, ())
        .map_err(|e| format!("could not bind zwp_linux_dmabuf_v1: {e:?}"))?;

    let surface = compositor.create_surface(&qh, ());
    for (name, colour, band) in [("A", COLOUR_A, None), ("B", COLOUR_B, Some(COLOUR_BAND))] {
        let owned = make(colour, band)?;
        let params = dmabuf.create_params(&qh, ());
        params.add(owned.as_fd(), 0, 0, owned.stride, 0, 0);
        let buffer = params.create_immed(
            WIDTH as i32,
            HEIGHT as i32,
            FORMAT_XRGB8888,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &qh,
            (),
        );
        params.destroy();

        surface.attach(Some(&buffer), 0, 0);
        surface.commit();
        queue
            .roundtrip(&mut state)
            .map_err(|e| format!("roundtrip after {name} failed: {e:?}"))?;
        // The descriptor travels with the request, which is flushed by the roundtrip, so it is
        // closed only after the server has processed it.
        drop(owned);
        println!("wldmabuf: committed {name}");
        if name == "A" {
            // The format events arrive with the first roundtrip the global is bound on.
            println!(
                "wldmabuf: the compositor advertised {} format(s), {} modifier(s)",
                state.formats, state.modifiers
            );
        }
    }

    if state.released == 0 {
        return Err("A's wl_buffer was never released".to_string());
    }
    println!("wldmabuf: PASS two dma-buf frames committed, A's buffer released");
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            println!("wldmabuf: FAIL {e}");
            std::process::exit(1);
        }
    }
}
