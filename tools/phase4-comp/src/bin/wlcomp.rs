//! The compositor 4d-2 put on `/run/wlcomp`: smithay's `CompositorState` and `ShmState`, the seat
//! 4d-3 added, and a renderer behind smithay's `Renderer` seam. The committed surfaces are drawn
//! into an offscreen target, the frame is read back, and the fb server's scan-out is re-pointed at
//! it (D17's renderer, D18's present). The renderer is chosen once at startup: `gl` is smithay's
//! `GlesRenderer` over EGL, `soft` is this project's own software rasteriser, and `gl` falls back
//! to `soft` when EGL cannot give a context, which is an output with no GPU.
//!
//! `/bin/wlcomp` binds the socket, forks, and leaves the compositor in the child, so the shell's
//! prompt comes back and this boot's gate can type the client next (`/bin/wlshm`, `/bin/wlseat`).
//! The child is an orphan from the shell's point of view, which is what the gates need: the last
//! frame has to still be on the scanout when a probe takes its screendump.
//!
//! `/dev/fb` is opened only for the re-point: the frame is the compositor's own buffer, handed to
//! the fb server as the scanout (`FBIO_SCANOUT_SRC`, through VFS because this is an ordinary user
//! process), so a commit costs one read-back and no blit. `/dev/kbd` is read the same way, on a
//! tick, because the input server's ring is a char device rather than a descriptor that signals.

#[path = "../dmabuf.rs"]
mod dmabuf;
#[path = "../gl.rs"]
mod gl;
#[path = "../keymap.rs"]
mod keymap;
#[path = "../soft.rs"]
mod soft;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, Mode, PostAction};

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Format, Modifier};
use smithay::backend::input::{
    ButtonState, Device, DeviceCapability, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
    UnusedEvent,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{draw_render_elements, on_commit_buffer_handler};
use smithay::backend::renderer::{
    Bind, Color32F, ExportMem, Frame, ImportAll, ImportDma, Offscreen, Renderer,
};
use smithay::input::Seat;
use smithay::input::SeatHandler;
use smithay::input::SeatState;
use smithay::input::keyboard::{FilterResult, Keycode, XkbConfig};
use smithay::input::pointer::{ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle};
use smithay::utils::{
    Buffer, Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Size, Transform,
};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_dmabuf, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
};

use soft::{SoftRenderer, SoftTarget};

/// The socket a client connects to, and the only name this gate uses.
const SOCKET: &str = "/run/wlcomp";

/// The output's mode. Every fb backend in the port adopts 1024x768 XRGB8888 (`.notes/wayland.md`
/// 6.5), and the fb server's scan-out re-point is built for exactly that frame.
const WIDTH: usize = 1024;
const HEIGHT: usize = 768;
const STRIDE: usize = WIDTH * 4;
const FRAME_LEN: usize = STRIDE * HEIGHT;
/// Rows per read-back. Each band is one pixel-pack buffer on the render node, so reading a
/// whole frame in bands asks the node for a quarter of a megabyte at a time instead of three.
const BAND_ROWS: usize = 64;

/// The frame the scan-out points at. Page-aligned because `FBIO_SCANOUT_SRC` resolves it page by
/// page and refuses a range whose address or length is not a page multiple.
#[repr(C, align(4096))]
struct Scanout([u8; FRAME_LEN]);

static mut SCANOUT: Scanout = Scanout([0; FRAME_LEN]);

/// What the operator asked for on the command line. The renderer is fixed at startup (D17), and
/// the compositor below is written against `Renderer` rather than either implementation.
#[derive(Clone, Copy)]
enum Choice {
    Gl,
    Soft,
}

/// The renderer and its offscreen target. Both variants answer the same calls; `render_scene`
/// instantiates against the trait, so nothing below `present` knows which one is in use.
enum RenderPath {
    Gl {
        renderer: GlesRenderer,
        target: GlesTexture,
    },
    Soft {
        renderer: SoftRenderer,
        target: SoftTarget,
    },
}

impl RenderPath {
    /// `gl` falls back to `soft` when EGL cannot produce a context, which is what an output with
    /// no GPU looks like; `soft` never touches EGL at all. The gate asserts which one ran, so a
    /// fallback cannot pass for the accelerated path.
    fn new(choice: Choice) -> Result<Self, String> {
        let size = Size::<i32, Buffer>::from((WIDTH as i32, HEIGHT as i32));
        match choice {
            Choice::Gl => match Self::gl(size) {
                Ok(path) => Ok(path),
                Err(e) => {
                    println!("wlcomp: GL renderer unavailable ({e}), using the software renderer");
                    Self::soft(size)
                }
            },
            Choice::Soft => Self::soft(size),
        }
    }

    fn gl(size: Size<i32, Buffer>) -> Result<Self, String> {
        let (bootstrap, context) = gl::create_context(WIDTH as i32, HEIGHT as i32)?;
        println!("wlcomp: {}", gl::describe(&bootstrap));
        // SAFETY: the bootstrap made this context current and nothing else uses GL in this thread.
        let mut renderer = unsafe { GlesRenderer::new(context) }
            .map_err(|e| format!("GLES renderer failed: {e:?}"))?;
        let target = Offscreen::<GlesTexture>::create_buffer(&mut renderer, Fourcc::Abgr8888, size)
            .map_err(|e| format!("offscreen target failed: {e:?}"))?;
        let formats = ImportDma::dmabuf_formats(&renderer);
        println!(
            "wlcomp: EGL offers {} dmabuf format(s), first {:?}",
            formats.iter().count(),
            formats.iter().next()
        );
        Ok(Self::Gl { renderer, target })
    }

    fn soft(size: Size<i32, Buffer>) -> Result<Self, String> {
        let mut renderer = SoftRenderer::new();
        let target = Offscreen::<SoftTarget>::create_buffer(&mut renderer, Fourcc::Abgr8888, size)
            .map_err(|e| format!("software target failed: {e}"))?;
        Ok(Self::Soft { renderer, target })
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Gl { .. } => "gl",
            Self::Soft { .. } => "soft",
        }
    }

    /// The formats a `zwp_linux_dmabuf_v1` global can offer. The GL renderer answers from the EGL
    /// display, which is the only thing that knows what it can import; the software renderer
    /// samples the node's linear memory, so it offers its four 4-byte formats as `Linear`.
    fn dmabuf_formats(&self) -> Vec<Format> {
        match self {
            Self::Gl { renderer, .. } => renderer.dmabuf_formats().iter().copied().collect(),
            Self::Soft { .. } => soft_dmabuf_formats(),
        }
    }

    /// Imports a client's dma-buf into the renderer that draws, which is what decides whether the
    /// buffer is usable at all. The texture is not kept: the draw imports it again from the
    /// surface's buffer, because the client may write the object between the two.
    fn import_dmabuf(&mut self, buffer: &Dmabuf) -> Result<(), String> {
        match self {
            Self::Gl { renderer, .. } => renderer
                .import_dmabuf(buffer, None)
                .map(|_| ())
                .map_err(|e| format!("{e:?}")),
            Self::Soft { renderer, .. } => renderer
                .import_dmabuf(buffer, None)
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }

    fn render(&mut self, surfaces: &[WlSurface], out: &mut [u8]) -> Result<usize, String> {
        match self {
            Self::Gl { renderer, target } => render_scene(renderer, target, surfaces, out),
            Self::Soft { renderer, target } => render_scene(renderer, target, surfaces, out),
        }
    }
}

/// What the software renderer can import: one linear plane in any of its four 4-byte formats.
fn soft_dmabuf_formats() -> Vec<Format> {
    [
        Fourcc::Argb8888,
        Fourcc::Xrgb8888,
        Fourcc::Abgr8888,
        Fourcc::Xbgr8888,
    ]
    .into_iter()
    .map(|code| Format {
        code,
        modifier: Modifier::Linear,
    })
    .collect()
}

/// Draws every committed surface into `target`, reads the frame back and packs it for the
/// scan-out. This is the one body both renderers share: it is written against smithay's traits,
/// so the only thing that differs between them is the type parameter.
fn render_scene<R, T>(
    renderer: &mut R,
    target: &mut T,
    surfaces: &[WlSurface],
    out: &mut [u8],
) -> Result<usize, String>
where
    R: Renderer + ImportAll + ExportMem + Bind<T> + Offscreen<T>,
    R::TextureId: Clone + 'static,
{
    let size = Size::<i32, Physical>::from((WIDTH as i32, HEIGHT as i32));
    let region = Rectangle::from_size(size);

    let mut elements: Vec<WaylandSurfaceRenderElement<R>> = Vec::new();
    for surface in surfaces {
        elements.extend(render_elements_from_surface_tree::<
            R,
            WaylandSurfaceRenderElement<R>,
        >(
            renderer, surface, (0, 0), 1.0, 1.0, Kind::Unspecified
        ));
    }
    if elements.is_empty() {
        // A commit with no buffer (the first one, or an unmap) leaves the display alone.
        return Ok(0);
    }

    let mut bound = renderer
        .bind(target)
        .map_err(|e| format!("bind failed: {e}"))?;
    let mut frame = renderer
        .render(&mut bound, size, Transform::Normal)
        .map_err(|e| format!("render failed: {e}"))?;
    frame
        .clear(Color32F::new(0.02, 0.02, 0.03, 1.0), &[region])
        .map_err(|e| format!("clear failed: {e}"))?;
    draw_render_elements(&mut frame, 1.0, &elements, &[region])
        .map_err(|e| format!("draw failed: {e}"))?;
    let _sync = frame.finish().map_err(|e| format!("finish failed: {e}"))?;

    // Row 0 of a read-back is the output's top row, for both renderers: a client's top rows
    // arrive in the mapping's first rows (the gate's band proves it), and the scan-out and the
    // client both count rows from the top. The read-back is RGBA, the fb's frame is XRGB8888, so
    // the pack is the R and B swap and the opaque byte.
    for top in (0..HEIGHT).step_by(BAND_ROWS) {
        let rows = BAND_ROWS.min(HEIGHT - top);
        let band = Rectangle::new(
            smithay::utils::Point::<i32, Buffer>::from((0, top as i32)),
            Size::<i32, Buffer>::from((WIDTH as i32, rows as i32)),
        );
        let mapping = renderer
            .copy_framebuffer(&bound, band, Fourcc::Abgr8888)
            .map_err(|e| format!("read-back at row {top} failed: {e}"))?;
        let bytes = renderer
            .map_texture(&mapping)
            .map_err(|e| format!("map at row {top} failed: {e}"))?;
        if bytes.len() < rows * STRIDE {
            return Err(format!(
                "read back {} bytes at row {top}, wanted {}",
                bytes.len(),
                rows * STRIDE
            ));
        }
        for y in 0..rows {
            let src = &bytes[y * STRIDE..][..STRIDE];
            let dst = &mut out[(top + y) * STRIDE..][..STRIDE];
            for (dst_px, src_px) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                dst_px[0] = src_px[2];
                dst_px[1] = src_px[1];
                dst_px[2] = src_px[0];
                dst_px[3] = 0xFF;
            }
        }
        drop(bytes);
        drop(mapping);
        // A read-back buffer is a node object. Freeing this band's before the next band asks for
        // one leaves the table's spare slots to the surfaces that are still on screen.
        renderer
            .cleanup_texture_cache()
            .map_err(|e| format!("cleanup at row {top} failed: {e}"))?;
    }
    drop(bound);
    Ok(elements.len())
}

struct State {
    dh: DisplayHandle,
    compositor: CompositorState,
    shm: ShmState,
    /// Kept for its global and its `wl_output` state; the compositor renders every surface at
    /// the origin, so the output's mode is what the frame is sized to.
    _output: Output,
    seat_state: SeatState<State>,
    seat: Seat<State>,
    surfaces: Vec<WlSurface>,
    /// The `zwp_linux_dmabuf_v1` delegate. The global is created only when the selected renderer
    /// can import a format, so a client is never offered a buffer the compositor would refuse.
    dmabuf_state: DmabufState,
    /// The `xdg_wm_base` delegate. A toplevel is configured as soon as it is made, because this
    /// compositor has no layout policy to wait for: the window is the output.
    xdg_shell: XdgShellState,
    /// The pointer's position in the output, fed from the input ring's absolute and relative
    /// pages. Every surface is drawn at the origin, so this is also the seat's pointer location.
    pointer_at: Point<f64, Logical>,
    path: RenderPath,
    /// `/dev/fb`, opened for the scan-out re-point. An ordinary user process reaches the fb
    /// server through VFS; a boot process would send `CDEV_IOCTL` directly (§6.11).
    fb: i32,
    /// `/dev/kbd`, read non-blocking on the input tick.
    kbd: i32,
    /// Whether a surface has been given keyboard focus. The gate has one window, so the first
    /// mapped surface takes it, there is no window manager to ask.
    focused: bool,
    presented: u32,
}

impl State {
    fn new(dh: DisplayHandle, path: RenderPath) -> Result<Self, String> {
        let compositor = CompositorState::new::<Self>(&dh);
        let shm = ShmState::new::<Self>(&dh, vec![]);

        // smithay compiles its keymap from xkb components, and this image ships none; the
        // components are materialised from the compiled keymap the in-house server serves.
        keymap::materialize()?;
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "minix");
        seat.add_keyboard(
            XkbConfig {
                layout: "us",
                ..XkbConfig::default()
            },
            200,
            25,
        )
        .map_err(|e| format!("seat keyboard failed: {e:?}"))?;
        seat.add_pointer();
        println!("wlcomp: wl_seat advertises a keyboard and a pointer");

        let xdg_shell = XdgShellState::new::<Self>(&dh);

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

        let mut dmabuf_state = DmabufState::new();
        let formats = path.dmabuf_formats();
        if formats.is_empty() {
            println!(
                "wlcomp: no dmabuf global: the {} renderer imports no format",
                path.name()
            );
        } else {
            println!(
                "wlcomp: wl_dmabuf global offers {} format(s) to the {} renderer",
                formats.len(),
                path.name()
            );
            dmabuf_state.create_global::<Self>(&dh, formats);
        }

        // SAFETY: fixed paths, opened for the present path and for the input tick below.
        let fb = unsafe { minix_std::fs::open(b"/dev/fb", minix_std::fs::O_RDWR, 0) }
            .map_err(|e| format!("open /dev/fb failed: {e:?}"))?;
        let mode = minix_std::fs::O_RDONLY | minix_std::fs::O_NONBLOCK;
        let kbd = unsafe { minix_std::fs::open(b"/dev/kbd", mode, 0) }
            .map_err(|e| format!("open /dev/kbd failed: {e:?}"))?;

        Ok(Self {
            dh,
            compositor,
            shm,
            _output: output,
            seat_state,
            seat,
            surfaces: Vec::new(),
            dmabuf_state,
            xdg_shell,
            pointer_at: (0.0, 0.0).into(),
            path,
            fb,
            kbd,
            focused: false,
            presented: 0,
        })
    }

    /// Render every committed surface into the offscreen target and hand the frame to the fb
    /// server. Called from the surface commit handler, which wayland-server runs inside its own
    /// dispatch, so the frame is on the scan-out before the client's roundtrip is answered.
    fn present(&mut self) {
        // SAFETY: this process is the buffer's only writer, and the borrow ends with the call.
        let out = unsafe { &mut *core::ptr::addr_of_mut!(SCANOUT.0) };
        let presented = match self.path.render(&self.surfaces, out) {
            // A commit with no buffer (the first one, or an unmap) leaves the display alone.
            Ok(0) => return,
            Ok(presented) => presented,
            Err(e) => {
                println!("wlcomp: {e}");
                return;
            }
        };
        let va = unsafe { core::ptr::addr_of_mut!(SCANOUT.0) } as u64;
        // Revert first: the fb server skips a re-point it believes is the same buffer, and this
        // buffer's *contents* change between commits, so a flush would leave the host showing the
        // frame it already had (D27, `VirtioGpuArch::present_resource`).
        self.scanout(0, 0);
        let status = self.scanout(va, FRAME_LEN as u64);
        self.presented += 1;
        if status == 0 {
            println!("wlcomp: presented {presented} surface(s), scanout re-pointed");
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

    /// Give the first *mapped* surface the keyboard. There is no window manager to arbitrate, so
    /// this compositor's policy is "the first window that has a buffer"; the seat sends `enter`
    /// to every keyboard that is (or later becomes) bound.
    fn focus(&mut self, surface: &WlSurface) {
        if self.focused {
            return;
        }
        if !is_mapped(surface) {
            return;
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(surface.clone()), SERIAL_COUNTER.next_serial());
            self.focused = true;
            println!("wlcomp: keyboard focus -> the first mapped surface");
        }
    }

    /// Moves the seat's pointer to `pointer_at`, focused on the first mapped surface. Every
    /// surface is drawn at the origin, so the hit test is "is there a window at all", and the
    /// focus's point is each surface's *origin*, which is where smithay subtracts it from the
    /// event's location before the client sees it.
    fn pointer_motion(&mut self) {
        let location = self.pointer_at;
        let focus = self.surfaces.iter().find(|s| is_mapped(s)).cloned();
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let event = MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time: (now_us() / 1000) as u32,
        };
        let origin = Point::<f64, Logical>::from((0.0, 0.0));
        pointer.motion(self, focus.map(|s| (s, origin)), &event);
        pointer.frame(self);
    }

    /// Hands the seat one button, on whichever surface the pointer is over.
    fn pointer_button(&mut self, button: u32, pressed: bool) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let event = ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: (now_us() / 1000) as u32,
            button,
            state: if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
        };
        pointer.button(self, &event);
        pointer.frame(self);
    }

    /// Reads the input server's ring until it is empty and hands every record to the seat.
    ///
    /// `</dev/kbd` is a char device, not a descriptor that signals, so this runs on the loop's
    /// tick, the same 20 ms `wlserver` uses. A record is 8 bytes: HID page, usage and value, all
    /// little-endian. The keyboard page becomes a key, the button page a pointer button, and the
    /// absolute and relative position pages a motion. The absolute page is a tablet: QEMU
    /// normalises it to 0..0x7FFF, so it is scaled to the output; the relative page is a mouse's
    /// deltas and is accumulated.
    fn drain_input(&mut self) {
        let mut batch = [0u8; 48];
        loop {
            // SAFETY: the descriptor is open for reading and the batch is writable for its length.
            let n = match unsafe { minix_std::fs::read(self.kbd, &mut batch) } {
                Ok(n) if n > 0 => n as usize,
                _ => break,
            };
            for record in batch[..n].chunks_exact(8) {
                let page = u16::from_le_bytes([record[0], record[1]]);
                let usage = u16::from_le_bytes([record[2], record[3]]);
                let value = i32::from_le_bytes([record[4], record[5], record[6], record[7]]);
                match page {
                    PAGE_KEY => {
                        let Some(evdev) = wayland::input::hid_to_evdev(usage) else {
                            continue;
                        };
                        let event = MinixKeyEvent {
                            device: MinixDevice,
                            time: now_us(),
                            // The keyboard takes xkb keycodes: the evdev code plus 8.
                            key: Keycode::from(u32::from(evdev) + 8),
                            state: if value != 0 {
                                KeyState::Pressed
                            } else {
                                KeyState::Released
                            },
                        };
                        self.dispatch(InputEvent::Keyboard { event });
                    }
                    PAGE_BUTTON => {
                        let button = match usage {
                            BUTTON_1 => wayland::input::BTN_LEFT,
                            BUTTON_2 => wayland::input::BTN_RIGHT,
                            BUTTON_3 => wayland::input::BTN_MIDDLE,
                            _ => continue,
                        };
                        self.pointer_button(button, value != 0);
                    }
                    PAGE_ABS => {
                        let scaled = value.clamp(0, 0x7FFF) as f64 / 0x8000 as f64;
                        match usage {
                            GD_X => self.pointer_at.x = scaled * WIDTH as f64,
                            GD_Y => self.pointer_at.y = scaled * HEIGHT as f64,
                            _ => continue,
                        }
                        self.pointer_motion();
                    }
                    PAGE_POINTER => {
                        match usage {
                            GD_X => self.pointer_at.x += value as f64,
                            GD_Y => self.pointer_at.y += value as f64,
                            _ => continue,
                        }
                        self.pointer_at.x = self.pointer_at.x.clamp(0.0, (WIDTH - 1) as f64);
                        self.pointer_at.y = self.pointer_at.y.clamp(0.0, (HEIGHT - 1) as f64);
                        self.pointer_motion();
                    }
                    _ => {}
                }
            }
        }
    }

    /// Hands one backend event to smithay's seat.
    fn dispatch(&mut self, event: InputEvent<MinixInput>) {
        if let InputEvent::Keyboard { event } = event {
            let serial = SERIAL_COUNTER.next_serial();
            let time = smithay::backend::input::Event::time_msec(&event);
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.input::<(), _>(
                    self,
                    event.key_code(),
                    event.state(),
                    serial,
                    time,
                    |_, _, _| FilterResult::Forward,
                );
            }
        }
    }
}

/// Whether a surface has a buffer, which is what makes it visible and focusable. Smithay records
/// it in the surface's renderer state on commit.
fn is_mapped(surface: &WlSurface) -> bool {
    smithay::backend::renderer::utils::with_renderer_surface_state(surface, |state| {
        state.buffer().is_some()
    })
    .unwrap_or(false)
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
        self.focus(surface);
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

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        match self.path.import_dmabuf(&dmabuf) {
            Ok(()) => {
                println!(
                    "wlcomp: dmabuf imported by the {} renderer",
                    self.path.name()
                );
                if let Err(e) = notifier.successful::<Self>() {
                    println!("wlcomp: the dmabuf's client went away before its buffer: {e}");
                }
            }
            Err(e) => {
                println!("wlcomp: dmabuf import failed: {e}");
                notifier.failed();
            }
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }

    /// A client made a toplevel. There is no layout policy to wait for: the window is the output,
    /// so it is configured at the size the output already has, and the commit with its first
    /// buffer is what puts it on the scan-out.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.send_configure();
        println!("wlcomp: a toplevel, configured");
    }

    /// Popups are placed by their positioner, which this compositor has nothing to disagree with.
    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let _ = surface.send_configure();
        println!("wlcomp: a popup, configured");
    }

    /// A popup asked to be placed again. Nothing here moves popups, so the request is ignored and
    /// the client keeps the position it was given.
    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }

    /// A popup asked for the pointer. Nothing here takes grabs, so the request is refused by
    /// simply not sending `popup_done`, which is what the client is waiting for.
    fn grab(
        &mut self,
        _surface: PopupSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
}

delegate_compositor!(State);
delegate_shm!(State);
delegate_output!(State);
delegate_seat!(State);
delegate_dmabuf!(State);
delegate_xdg_shell!(State);

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

/// The HID keyboard page (`drivers::input::constants::INPUT_PAGE_KEY`); the only page this
/// backend translates.
const PAGE_KEY: u16 = 0x07;
/// The HID button page (`drivers::input::constants::INPUT_PAGE_BUTTON`) and the generic-desktop
/// page a relative mouse reports through; `0xFD` is the absolute page a tablet reports through.
const PAGE_BUTTON: u16 = 0x09;
const PAGE_POINTER: u16 = 0x01;
const PAGE_ABS: u16 = 0xfd;
/// The generic-desktop X and Y usage ids both position pages use.
const GD_X: u16 = 0x30;
const GD_Y: u16 = 0x31;
/// The button page's three buttons, in the order the input server numbers them.
const BUTTON_1: u16 = 0x01;
const BUTTON_2: u16 = 0x02;
const BUTTON_3: u16 = 0x03;

/// The input server's tick. The ring is a char device, so it has to be polled; 20 ms is a
/// frame's worth, the same value `wlserver` uses.
const TICK: Duration = Duration::from_millis(20);

/// This port's `InputBackend`: the input server's HID records, one keyboard device.
///
/// The trait carries only the event *types* — a backend exposes its own `dispatch_*` — so the
/// implementation is the association list plus the two event impls below. Everything this port
/// does not source (pointer, touch, gestures, tablets) is `UnusedEvent`, which is what the enum's
/// `match *self {}` impls are for.
struct MinixInput;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MinixDevice;

impl Device for MinixDevice {
    fn id(&self) -> String {
        "minix-kbd".to_string()
    }

    fn name(&self) -> String {
        "the input server's keyboard".to_string()
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        capability == DeviceCapability::Keyboard
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<PathBuf> {
        None
    }
}

#[derive(Debug)]
struct MinixKeyEvent {
    device: MinixDevice,
    time: u64,
    key: Keycode,
    state: KeyState,
}

impl smithay::backend::input::Event<MinixInput> for MinixKeyEvent {
    fn time(&self) -> u64 {
        self.time
    }

    fn device(&self) -> MinixDevice {
        self.device.clone()
    }
}

impl KeyboardKeyEvent<MinixInput> for MinixKeyEvent {
    fn key_code(&self) -> Keycode {
        self.key
    }

    fn state(&self) -> KeyState {
        self.state
    }

    fn count(&self) -> u32 {
        0
    }
}

impl InputBackend for MinixInput {
    type Device = MinixDevice;
    type KeyboardKeyEvent = MinixKeyEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = UnusedEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = UnusedEvent;
    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = ();
}

/// Microseconds from the port's monotonic clock, the base `Event::time` documents as undefined.
fn now_us() -> u64 {
    match minix_std::time::clock_gettime(minix_std::time::CLOCK_MONOTONIC) {
        Ok(ts) => (ts.tv_sec as u64)
            .wrapping_mul(1_000_000)
            .wrapping_add(ts.tv_nsec as u64 / 1_000),
        Err(_) => 0,
    }
}

fn serve(listener: std::os::minix::net::UnixListener, choice: Choice) -> ! {
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
    let path = match RenderPath::new(choice) {
        Ok(path) => path,
        Err(e) => {
            println!("wlcomp: FAIL {e}");
            std::process::exit(1);
        }
    };
    println!("wlcomp: renderer {}", path.name());
    let mut state = match State::new(dh, path) {
        Ok(state) => state,
        Err(e) => {
            println!("wlcomp: FAIL {e}");
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

    // The input tick. The ring has no descriptor to wait on, so the loop polls it; anything the
    // tick produces is queued on the display, which then has to be flushed or it would sit in the
    // socket until the client's next request.
    if let Err(e) = handle.insert_source(Timer::from_duration(TICK), |_, _, state| {
        state.drain_input();
        if let Err(e) = state.dh.flush_clients() {
            println!("wlcomp: input flush failed: {e}");
        }
        TimeoutAction::ToDuration(TICK)
    }) {
        println!("wlcomp: registering the input tick failed: {e}");
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
    let choice = match std::env::args().nth(1).as_deref() {
        None | Some("gl") => Choice::Gl,
        Some("soft") => Choice::Soft,
        Some(other) => {
            println!("wlcomp: unknown renderer {other:?}: pass `gl` or `soft`");
            std::process::exit(2);
        }
    };

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
        serve(listener, choice);
    }
    if child < 0 {
        println!("wlcomp: fork failed");
        std::process::exit(1);
    }
    println!("wlcomp: compositor left serving on {SOCKET} (pid {child})");
    std::process::exit(0);
}
