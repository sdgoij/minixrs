//! Phase 4d-2's compositor: smithay's `CompositorState` and `ShmState` on `/run/wlcomp`, drawing
//! the committed surfaces into an offscreen GLES target and presenting the read-back through the
//! fb server's scan-out (`FBIO_SCANOUT_SRC`) — D17's renderer with 3d-b-1's present (D18).
//!
//! `/bin/wlcomp` binds the socket, forks, and leaves the compositor in the child, so the shell's
//! prompt comes back and this boot's gate can type the client next (`/bin/wlshm`). The child is
//! an orphan from the shell's point of view, which is what the gate needs: the last frame has to
//! still be on the scanout when the probe takes its screendump.
//!
//! `/dev/fb` is opened only for the re-point: the frame is the compositor's own buffer, handed to
//! the fb server as the scanout (`FBIO_SCANOUT_SRC`, through VFS because this is an ordinary user
//! process), so a commit costs one read-back and no blit.

#[path = "../gl.rs"]
mod gl;

use std::sync::Arc;

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode, PostAction};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{draw_render_elements, on_commit_buffer_handler};
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{delegate_compositor, delegate_output, delegate_shm};

/// The socket a client connects to, and the only name this gate uses.
const SOCKET: &str = "/run/wlcomp";

/// The output's mode. Every fb backend in the port adopts 1024x768 XRGB8888 (`.notes/wayland.md`
/// 6.5), and the fb server's scan-out re-point is built for exactly that frame.
const WIDTH: usize = 1024;
const HEIGHT: usize = 768;
const STRIDE: usize = WIDTH * 4;
const FRAME_LEN: usize = STRIDE * HEIGHT;

/// The frame the scan-out points at. Page-aligned because `FBIO_SCANOUT_SRC` resolves it page by
/// page and refuses a range whose address or length is not a page multiple.
#[repr(C, align(4096))]
struct Scanout([u8; FRAME_LEN]);

static mut SCANOUT: Scanout = Scanout([0; FRAME_LEN]);

struct State {
    dh: DisplayHandle,
    compositor: CompositorState,
    shm: ShmState,
    /// Kept for its global and its `wl_output` state; nothing renders through it yet (4d-3 adds
    /// the seat, 4d-2 only needs the output to exist and be advertised).
    _output: Output,
    surfaces: Vec<WlSurface>,
    renderer: GlesRenderer,
    target: GlesTexture,
    /// `/dev/fb`, opened for the scan-out re-point. An ordinary user process reaches the fb
    /// server through VFS; a boot process would send `CDEV_IOCTL` directly (§6.11).
    fb: i32,
    presented: u32,
}

impl State {
    fn new(dh: DisplayHandle) -> Result<Self, String> {
        let compositor = CompositorState::new::<Self>(&dh);
        let shm = ShmState::new::<Self>(&dh, vec![]);

        let output = Output::new(
            "minix".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "minixrs".into(),
                model: "wlcomp".into(),
            },
        );
        output.create_global::<Self>(&dh);
        let mode = OutputMode {
            size: (WIDTH as i32, HEIGHT as i32).into(),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(mode);

        let (bootstrap, context) = gl::create_context(WIDTH as i32, HEIGHT as i32)?;
        println!("wlcomp: {}", gl::describe(&bootstrap));
        // SAFETY: the bootstrap made this context current and nothing else uses GL in this thread.
        let mut renderer = unsafe { GlesRenderer::new(context) }
            .map_err(|e| format!("GLES renderer failed: {e:?}"))?;
        let target_size = Size::<i32, Buffer>::from((WIDTH as i32, HEIGHT as i32));
        let target =
            Offscreen::<GlesTexture>::create_buffer(&mut renderer, Fourcc::Abgr8888, target_size)
                .map_err(|e| format!("offscreen target failed: {e:?}"))?;
        println!("wlcomp: offscreen target {WIDTH}x{HEIGHT} Abgr8888");

        // SAFETY: a fixed path, opened read-write for the present path below.
        let fb = unsafe { minix_std::fs::open(b"/dev/fb", minix_std::fs::O_RDWR, 0) }
            .map_err(|e| format!("open /dev/fb failed: {e:?}"))?;

        Ok(Self {
            dh,
            compositor,
            shm,
            _output: output,
            surfaces: Vec::new(),
            renderer,
            target,
            fb,
            presented: 0,
        })
    }

    /// Render every committed surface into the offscreen target and hand the frame to the fb
    /// server. Called from the surface commit handler, which wayland-server runs inside its own
    /// dispatch, so the frame is on the scan-out before the client's roundtrip is answered.
    fn present(&mut self) {
        let size = Size::<i32, Physical>::from((WIDTH as i32, HEIGHT as i32));
        let buffer_size = Size::<i32, Buffer>::from((WIDTH as i32, HEIGHT as i32));
        let region = Rectangle::from_size(size);

        let mut elements = Vec::new();
        for surface in &self.surfaces {
            elements.extend(render_elements_from_surface_tree::<
                GlesRenderer,
                WaylandSurfaceRenderElement<GlesRenderer>,
            >(
                &mut self.renderer,
                surface,
                (0, 0),
                1.0,
                1.0,
                Kind::Unspecified,
            ));
        }
        if elements.is_empty() {
            // A commit with no buffer (the first one, or an unmap) leaves the display alone.
            return;
        }

        let renderer = &mut self.renderer;
        let target = &mut self.target;
        let mut bound = match renderer.bind(target) {
            Ok(bound) => bound,
            Err(e) => {
                println!("wlcomp: bind failed: {e:?}");
                return;
            }
        };
        match renderer.render(&mut bound, size, Transform::Normal) {
            Ok(mut frame) => {
                if let Err(e) = frame.clear(Color32F::new(0.02, 0.02, 0.03, 1.0), &[region]) {
                    println!("wlcomp: clear failed: {e:?}");
                }
                match draw_render_elements(&mut frame, 1.0, &elements, &[region]) {
                    Ok(_) => {}
                    Err(e) => println!("wlcomp: draw failed: {e:?}"),
                }
                if let Err(e) = frame.finish() {
                    println!("wlcomp: frame finish failed: {e:?}");
                }
            }
            Err(e) => {
                println!("wlcomp: render failed: {e:?}");
                return;
            }
        }
        drop(bound);

        {
            let mapping = match renderer.copy_texture(
                target,
                Rectangle::from_size(buffer_size),
                Fourcc::Abgr8888,
            ) {
                Ok(mapping) => mapping,
                Err(e) => {
                    println!("wlcomp: copy_texture failed: {e:?}");
                    return;
                }
            };
            let bytes = match renderer.map_texture(&mapping) {
                Ok(bytes) => bytes,
                Err(e) => {
                    println!("wlcomp: map_texture failed: {e:?}");
                    return;
                }
            };
            if bytes.len() < FRAME_LEN {
                println!(
                    "wlcomp: read back {} bytes, wanted {FRAME_LEN}",
                    bytes.len()
                );
                return;
            }
            // The read-back is RGBA, bottom row first; the fb's frame is XRGB8888, top row first.
            // Doing both here is what lets the present stay a re-point with no copy.
            let out = unsafe { &mut *core::ptr::addr_of_mut!(SCANOUT.0) };
            for y in 0..HEIGHT {
                let src = &bytes[(HEIGHT - 1 - y) * STRIDE..][..STRIDE];
                let dst = &mut out[y * STRIDE..][..STRIDE];
                for (dst_px, src_px) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    dst_px[0] = src_px[2]; // B
                    dst_px[1] = src_px[1]; // G
                    dst_px[2] = src_px[0]; // R
                    dst_px[3] = 0xFF; // X
                }
            }
        }

        let va = unsafe { core::ptr::addr_of_mut!(SCANOUT.0) } as u64;
        // Revert first: the fb server skips a re-point it believes is the same buffer, and this
        // buffer's *contents* change between commits — a flush would leave the host showing the
        // frame it already had (D27, `VirtioGpuArch::present_resource`).
        let _ = self.scanout(0, 0);
        let status = self.scanout(va, FRAME_LEN as u64);
        self.presented += 1;
        if status == 0 {
            println!(
                "wlcomp: presented {} surface(s), scanout re-pointed",
                elements.len()
            );
        } else {
            println!("wlcomp: scanout refused ({status})");
        }
    }

    /// `FBIO_SCANOUT_SRC` through `/dev/fb`: VFS names this process to the fb server, which
    /// resolves the range through this process's page table. `len == 0` restores the driver's own
    /// buffer.
    fn scanout(&self, va: u64, len: u64) -> i32 {
        let mut src = FbScanoutSrc { va, len };
        // SAFETY: `src` is the argument struct the request names, and VFS copies it through a
        // magic grant of exactly its size before the fb server answers.
        match unsafe {
            minix_std::fs::ioctl(
                self.fb,
                minix_std::fs::FBIO_SCANOUT_SRC,
                &mut src as *mut _ as *mut u8,
            )
        } {
            Ok(status) => status,
            Err(e) => {
                println!("wlcomp: FBIO_SCANOUT_SRC failed: {e:?}");
                -1
            }
        }
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        self.surfaces.push(surface.clone());
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Smithay takes the buffer out of the pending state and tracks it; without this call the
        // renderer sees no texture and every frame would be the clear colour.
        on_commit_buffer_handler::<Self>(surface);
        self.present();
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        self.surfaces.retain(|s| s != surface);
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl OutputHandler for State {}

delegate_compositor!(State);
delegate_shm!(State);
delegate_output!(State);

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// `FBIO_SCANOUT_SRC`'s argument (`.notes/wayland.md` §5 row 25's family of device ABI structs):
/// the buffer's address in *this* process, page-aligned, and its length.
#[repr(C)]
struct FbScanoutSrc {
    va: u64,
    len: u64,
}

fn serve(listener: std::os::minix::net::UnixListener) -> ! {
    let mut event_loop: EventLoop<State> = match EventLoop::try_new() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            println!("wlcomp: event loop failed: {e}");
            std::process::exit(1);
        }
    };
    let display: Display<State> = match Display::new() {
        Ok(display) => display,
        Err(e) => {
            println!("wlcomp: display failed: {e}");
            std::process::exit(1);
        }
    };
    let dh = display.handle();
    let mut state = match State::new(dh) {
        Ok(state) => state,
        Err(e) => {
            println!("wlcomp: {e}");
            std::process::exit(1);
        }
    };
    println!("wlcomp: ready on {SOCKET}");

    let handle = event_loop.handle();
    if let Err(e) = handle.insert_source(
        Generic::new(listener, Interest::READ, Mode::Level),
        |_, listener, state| {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => match state
                        .dh
                        .insert_client(stream, Arc::new(ClientState::default()))
                    {
                        Ok(_) => println!("wlcomp: a client connected"),
                        Err(e) => println!("wlcomp: insert_client failed: {e}"),
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        println!("wlcomp: accept failed: {e}");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    ) {
        println!("wlcomp: registering the listening socket failed: {e}");
        std::process::exit(1);
    }

    if let Err(e) = handle.insert_source(
        Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            // SAFETY: the display is owned by this source and never dropped while the loop runs.
            let display = unsafe { display.get_mut() };
            match display.dispatch_clients(state) {
                Ok(_) => {}
                Err(e) => println!("wlcomp: dispatch stopped: {e}"),
            }
            if let Err(e) = display.flush_clients() {
                println!("wlcomp: flush failed: {e}");
            }
            Ok(PostAction::Continue)
        },
    ) {
        println!("wlcomp: registering the display failed: {e}");
        std::process::exit(1);
    }

    match event_loop.run(None, &mut state, |_| {}) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            println!("wlcomp: event loop stopped: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let listener = match std::os::minix::net::UnixListener::bind(SOCKET) {
        Ok(listener) => listener,
        Err(e) => {
            println!("wlcomp: bind {SOCKET} failed: {e}");
            std::process::exit(1);
        }
    };

    // The compositor outlives the command: the shell waits for the child it started, and that
    // child returns as soon as the socket is bound. The orphaned compositor keeps serving, which
    // is what leaves the frame on screen for the gate's screendump.
    let child = minix_rt::fork();
    if child == 0 {
        serve(listener);
    }
    if child < 0 {
        println!("wlcomp: fork failed");
        std::process::exit(1);
    }
    println!("wlcomp: compositor left serving on {SOCKET} (pid {child})");
    std::process::exit(0);
}
