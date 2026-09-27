//! The server side of the Wayland protocol: an object table and a request
//! dispatcher.
//!
//! Pure — no sockets, no fds, no syscalls. A caller reads a message off the
//! wire, hands [`Server::dispatch`] the object id, opcode and body, and gets
//! back the bytes of the events to send (framed, in order). This is what makes
//! the protocol host-testable, and it is the piece a real `/sbin/wlserver`
//! shares with the smoke test.
//!
//! Phase 1 scope (`WAYLAND.md` §6.11): the core handshake (`wl_display.sync`,
//! `wl_registry`), the factories a client binds (`wl_compositor`, `wl_shm`,
//! `wl_output`, `wl_seat`), and object creation for surfaces and regions.
//! `wl_shm` pools/buffers and input events are 1b/1c; the keymap event is 2a
//! (`WAYLAND.md` §6.12), whose bytes the caller supplies — nothing here touches
//! an fd.

use crate::protocol;
use crate::wire::{Arg, DispatchBuf, MAX_ARGS, WireError};

pub use crate::protocol::Kind;

/// Most objects one client may hold open.
pub const MAX_OBJECTS: usize = 64;

/// The id `wl_display` always has.
pub const DISPLAY_ID: u32 = 1;

/// An object the client created. The trailing fields are meaningful only for
/// the kinds that name them (`wl_shm_pool`, `wl_buffer`, `wl_surface`); every
/// other kind leaves them zero.
#[derive(Debug, Clone, Copy)]
pub struct Obj {
    pub id: u32,
    pub kind: Kind,
    /// `wl_shm_pool`: the fd's index in the `create_pool` message's
    /// `SCM_RIGHTS` list, and the pool's size in bytes.
    pub fd_index: u32,
    pub pool_size: i32,
    /// `wl_buffer`: the pool that backs it.
    pub pool: u32,
    /// `wl_buffer`: where in the pool it starts, and its geometry.
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: u32,
    /// `wl_surface`: the buffer most recently attached (0 = none).
    pub attached: u32,
    /// `wl_surface`: it has been committed at least once. A surface's *first*
    /// commit is a window appearing, which is what asks for focus (2c).
    pub committed: bool,
    /// `wl_surface`: the damage accumulated since the last commit, as its bounding
    /// rectangle. `wl_surface.damage`/`damage_buffer` grow it (2d).
    pub damage: Option<crate::shm::Rect>,
    /// `wl_surface`: the `xdg_surface` bound to it (0 = none). `xdg_toplevel`: the
    /// `xdg_surface` it belongs to. `xdg_popup`: its own `xdg_surface`.
    pub xdg_surface: u32,
    /// `xdg_surface`: the `wl_surface` it is for. `xdg_popup`: the *parent*
    /// `xdg_surface`. `zwlr_layer_surface_v1`: the `wl_surface` it is for.
    /// `zxdg_toplevel_decoration_v1`: the `xdg_toplevel` it decorates.
    pub surface: u32,
    /// `wl_surface`: the `zwlr_layer_surface_v1` bound to it (0 = none) (2e).
    pub layer: u32,
    /// `xdg_surface`: a `configure` has gone out (2b).
    pub configured: bool,
    /// `xdg_surface`: the client has acked the latest `configure`.
    pub acked: bool,
    /// `xdg_surface`: the serial a client must ack.
    pub configure_serial: u32,
}

impl Obj {
    const fn new(id: u32, kind: Kind) -> Self {
        Self {
            id,
            kind,
            fd_index: 0,
            pool_size: 0,
            pool: 0,
            offset: 0,
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            attached: 0,
            committed: false,
            damage: None,
            xdg_surface: 0,
            surface: 0,
            layer: 0,
            configured: false,
            acked: false,
            configure_serial: 0,
        }
    }
}

/// A surface commit the caller must present. It carries what is needed to find
/// the pixels: the pool (and the fd index its `create_pool` named) and the
/// buffer's geometry within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commit {
    pub surface: u32,
    pub buffer: u32,
    pub pool: u32,
    pub fd_index: u32,
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: u32,
    /// The pool's size, so the bytes can be re-validated at blit time.
    pub pool_size: i32,
    /// The damage this commit carried, if the client named any. `None` means "take
    /// the whole buffer", which is what a client that never calls `damage` gets.
    pub damage: Option<crate::shm::Rect>,
}

impl Commit {
    /// The `wl_shm` buffer this commit's pixels live in, for [`crate::shm::blit`].
    pub fn buffer(&self) -> crate::shm::Buffer {
        crate::shm::Buffer {
            offset: self.offset,
            width: self.width,
            height: self.height,
            stride: self.stride,
            format: self.format,
            pool_size: self.pool_size,
        }
    }
}

/// A cursor image a client handed the pointer: the surface's buffer, and where its
/// top-left sits relative to the pointer (2d).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorImage {
    pub buffer: crate::shm::Buffer,
    pub pool: u32,
    pub fd_index: u32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
}

/// What `dispatch` did that the caller must act on next.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Outcome {
    /// A `wl_shm.create_pool` created this pool object. Its `h` argument is an
    /// fd index the caller resolves with `recv_fds` and remembers for the pool.
    pub new_pool: Option<u32>,
}

/// A dispatch failure that ends the connection, as opposed to a protocol error
/// (which is answered with `wl_display.error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchError {
    /// The message did not decode against its interface's signature.
    Wire(WireError),
    /// The event buffer was too small to hold the reply.
    NoSpace,
}

impl From<WireError> for DispatchError {
    fn from(e: WireError) -> Self {
        DispatchError::Wire(e)
    }
}

/// The server's per-connection state.
pub struct Server {
    objs: [Obj; MAX_OBJECTS],
    n: usize,
    serial: u32,
    width: i32,
    height: i32,
    /// The most recent commit awaiting presentation ([`Server::take_commit`]).
    pending: Option<Commit>,
    /// The surface input goes to, once one has committed (0 = none). Phase 1 has
    /// no window management, so the first surface to commit keeps focus.
    focus: u32,
    /// Whether the `enter` events for `focus` have been sent.
    entered: bool,
    /// The surface whose *first* commit should take focus. The loop reads it with
    /// [`Server::take_focus_request`] to arbitrate between connections (2c).
    focus_request: Option<u32>,
    /// The keymap's byte length as the caller knows it (0 = none to serve). The
    /// bytes never pass through here: the caller holds them and sends the fd.
    keymap_size: u32,
    /// A `wl_keyboard.keymap` was emitted and its fd still owes the caller an
    /// out-of-band send ([`Server::take_keymap`]).
    keymap_owed: bool,
    /// The surface the client set as the pointer's image, and its hotspot (2d).
    cursor_surface: u32,
    cursor_hotspot: (i32, i32),
    /// The cursor image from that surface's latest commit.
    cursor: Option<CursorImage>,
}

impl Server {
    /// A server for one client, with the surface size it reports for
    /// `wl_output`.
    pub fn new(width: i32, height: i32) -> Self {
        let mut objs = [Obj::new(0, Kind::Display); MAX_OBJECTS];
        objs[0] = Obj::new(DISPLAY_ID, Kind::Display);
        Self {
            objs,
            n: 1,
            serial: 0,
            width,
            height,
            pending: None,
            focus: 0,
            entered: false,
            focus_request: None,
            keymap_size: 0,
            keymap_owed: false,
            cursor_surface: 0,
            cursor_hotspot: (0, 0),
            cursor: None,
        }
    }

    /// The kind of an object, or `None` if the client never created it.
    pub fn kind(&self, id: u32) -> Option<Kind> {
        self.objs[..self.n]
            .iter()
            .find(|o| o.id == id)
            .map(|o| o.kind)
    }

    /// How many objects are live (a test convenience).
    pub fn object_count(&self) -> usize {
        self.n
    }

    fn add(&mut self, id: u32, kind: Kind) -> Result<(), DispatchError> {
        self.add_obj(Obj::new(id, kind))
    }

    /// Add a fully-populated object (for the kinds that carry extra state).
    fn add_obj(&mut self, obj: Obj) -> Result<(), DispatchError> {
        if obj.id == 0 || self.kind(obj.id).is_some() {
            // A client reusing a live id is a protocol error, but not one that
            // ends the connection; report it as a duplicate by refusing.
            return Err(DispatchError::NoSpace);
        }
        if self.n >= MAX_OBJECTS {
            return Err(DispatchError::NoSpace);
        }
        self.objs[self.n] = obj;
        self.n += 1;
        Ok(())
    }

    fn obj(&self, id: u32) -> Option<&Obj> {
        self.objs[..self.n].iter().find(|o| o.id == id)
    }

    fn obj_mut(&mut self, id: u32) -> Option<&mut Obj> {
        self.objs[..self.n].iter_mut().find(|o| o.id == id)
    }

    fn remove(&mut self, id: u32) {
        if let Some(i) = self.objs[..self.n].iter().position(|o| o.id == id) {
            for j in i..self.n - 1 {
                self.objs[j] = self.objs[j + 1];
            }
            self.n -= 1;
        }
    }

    /// Dispatch one request. Returns the number of bytes written to `out` (the
    /// events to send, each already framed and in order). A protocol error is
    /// written as a `wl_display.error` event and is *not* an `Err`; only a
    /// transport-level problem ends the connection.
    pub fn dispatch(
        &mut self,
        object_id: u32,
        opcode: u16,
        body: &[u8],
        out: &mut DispatchBuf<'_>,
    ) -> Result<Outcome, DispatchError> {
        let mut outcome = Outcome::default();
        let Some(kind) = self.kind(object_id) else {
            error(
                out,
                object_id,
                protocol::WL_DISPLAY_ERROR_INVALID_OBJECT,
                b"no such object",
            )?;
            return Ok(outcome);
        };
        let iface = kind.interface();
        let mut args = [Arg::Uint(0); MAX_ARGS];
        let n = protocol::decode_request(iface, opcode, body, &mut args)?;
        match (kind, opcode) {
            (Kind::Display, protocol::display_req::SYNC) => {
                self.create_callback(arg(n, &args, 0)?, out)
            }
            (Kind::Display, protocol::display_req::GET_REGISTRY) => {
                self.do_get_registry(arg(n, &args, 0)?, out)
            }
            (Kind::Registry, protocol::registry_req::BIND) => self.do_bind(
                arg(n, &args, 0)?,
                str_arg(&args, 1)?,
                arg(n, &args, 2)?,
                arg(n, &args, 3)?,
                out,
            ),
            (Kind::Compositor, protocol::compositor_req::CREATE_SURFACE) => {
                self.add(arg(n, &args, 0)?, Kind::Surface)?;
                Ok(())
            }
            (Kind::Compositor, protocol::compositor_req::CREATE_REGION) => {
                self.add(arg(n, &args, 0)?, Kind::Region)?;
                Ok(())
            }
            (Kind::Region, protocol::region_req::DESTROY)
            | (Kind::Surface, protocol::surface_req::DESTROY)
            | (Kind::ShmPool, protocol::shm_pool_req::DESTROY)
            | (Kind::Buffer, protocol::buffer_req::DESTROY)
            | (Kind::Pointer, protocol::pointer_req::RELEASE)
            | (Kind::Keyboard, protocol::keyboard_req::RELEASE)
            | (Kind::XdgWmBase, protocol::xdg_wm_base_req::DESTROY)
            | (Kind::XdgPositioner, protocol::xdg_positioner_req::DESTROY)
            | (Kind::XdgSurface, protocol::xdg_surface_req::DESTROY)
            | (Kind::XdgToplevel, protocol::xdg_toplevel_req::DESTROY)
            | (Kind::XdgPopup, protocol::xdg_popup_req::DESTROY)
            | (Kind::LayerShell, protocol::layer_shell_req::DESTROY)
            | (Kind::LayerSurface, protocol::layer_surface_req::DESTROY)
            | (Kind::DecorationManager, protocol::decoration_manager_req::DESTROY)
            | (Kind::ToplevelDecoration, protocol::toplevel_decoration_req::DESTROY) => {
                self.remove(object_id);
                Ok(())
            }
            (Kind::Pointer, protocol::pointer_req::SET_CURSOR) => {
                // `set_cursor(serial, surface, hotspot_x, hotspot_y)`; surface 0 unsets.
                // The serial is not checked against a recent `enter` (2d).
                let surface = arg(n, &args, 1)?;
                let hx = iarg(n, &args, 2)?;
                let hy = iarg(n, &args, 3)?;
                if surface == 0 {
                    self.cursor_surface = 0;
                    self.cursor = None;
                } else if self.kind(surface) == Some(Kind::Surface) {
                    self.cursor_surface = surface;
                    self.cursor_hotspot = (hx, hy);
                }
                Ok(())
            }
            (Kind::Seat, protocol::seat_req::GET_POINTER) => {
                self.add(arg(n, &args, 0)?, Kind::Pointer)?;
                // A pointer made after the surface committed is still owed its
                // `enter`, so offering one is what triggers it.
                self.ensure_entered(out)
            }
            (Kind::Seat, protocol::seat_req::GET_KEYBOARD) => {
                self.add(arg(n, &args, 0)?, Kind::Keyboard)?;
                // The keymap precedes any key event, so it goes out with the object
                // that will carry them.
                self.emit_keymap(out)?;
                self.ensure_entered(out)
            }
            (Kind::Shm, protocol::shm_req::CREATE_POOL) => {
                let id = arg(n, &args, 0)?;
                let fd_index = arg(n, &args, 1)?;
                let size = iarg(n, &args, 2)?;
                self.add_obj(Obj {
                    fd_index,
                    pool_size: size,
                    ..Obj::new(id, Kind::ShmPool)
                })?;
                outcome.new_pool = Some(id);
                Ok(())
            }
            (Kind::ShmPool, protocol::shm_pool_req::CREATE_BUFFER) => {
                let id = arg(n, &args, 0)?;
                let offset = iarg(n, &args, 1)?;
                let width = iarg(n, &args, 2)?;
                let height = iarg(n, &args, 3)?;
                let stride = iarg(n, &args, 4)?;
                let format = arg(n, &args, 5)?;
                let pool_size = self.obj(object_id).map_or(-1, |p| p.pool_size);
                let buffer = crate::shm::Buffer {
                    offset,
                    width,
                    height,
                    stride,
                    format,
                    pool_size,
                };
                match buffer.validate() {
                    Ok(()) => self.add_obj(Obj {
                        pool: object_id,
                        offset,
                        width,
                        height,
                        stride,
                        format,
                        ..Obj::new(id, Kind::Buffer)
                    }),
                    // A bad buffer is reported on the pool, under the `wl_shm`
                    // error codes, rather than dropped silently.
                    Err(e) => error(out, object_id, e.code(), e.message()),
                }
            }
            // A pool's declared size, which the client has already `ftruncate`d the fd
            // to: recording it is all that a later `create_buffer` validates against.
            (Kind::ShmPool, protocol::shm_pool_req::RESIZE) => {
                let size = iarg(n, &args, 0)?;
                if let Some(p) = self.obj_mut(object_id) {
                    p.pool_size = size;
                }
                Ok(())
            }
            (Kind::Surface, protocol::surface_req::ATTACH) => {
                // `buffer` is nullable; an id of 0 detaches.
                let buffer = arg(n, &args, 0)?;
                if let Some(o) = self.obj_mut(object_id) {
                    o.attached = buffer;
                }
                Ok(())
            }
            (Kind::Surface, protocol::surface_req::DAMAGE) => {
                let x = iarg(n, &args, 0)?;
                let y = iarg(n, &args, 1)?;
                let w = iarg(n, &args, 2)?;
                let h = iarg(n, &args, 3)?;
                if let Some(o) = self.obj_mut(object_id) {
                    grow_damage(&mut o.damage, x, y, w, h);
                }
                Ok(())
            }
            (Kind::Surface, protocol::surface_req::DAMAGE_BUFFER) => {
                // Buffer coordinates, and this port's buffers are scale 1 with no
                // transform, so they are the surface's own coordinates (2d).
                let x = iarg(n, &args, 0)?;
                let y = iarg(n, &args, 1)?;
                let w = iarg(n, &args, 2)?;
                let h = iarg(n, &args, 3)?;
                if let Some(o) = self.obj_mut(object_id) {
                    grow_damage(&mut o.damage, x, y, w, h);
                }
                Ok(())
            }
            (Kind::Surface, protocol::surface_req::FRAME) => {
                // A frame callback is answered at once with `done`.
                self.create_callback(arg(n, &args, 0)?, out)
            }
            (Kind::Surface, protocol::surface_req::COMMIT) => {
                let layer = self.obj(object_id).map_or(0, |o| o.layer);
                if object_id == self.cursor_surface && self.cursor_surface != 0 {
                    // A cursor surface's commit supplies the pointer image; it is not
                    // a window, so it is neither presented nor focused.
                    self.cursor = self.cursor_of(object_id);
                } else if layer != 0 {
                    // A layer surface (a panel) is configured like an `xdg_surface`,
                    // but it never takes focus: a panel is not where input goes (2e).
                    if !self.obj(layer).is_some_and(|o| o.configured) {
                        self.configure_layer(layer, out)?;
                    } else if self.obj(layer).is_some_and(|o| o.acked)
                        && self.obj(object_id).is_some_and(|o| o.attached != 0)
                    {
                        let c = self.commit_of(object_id);
                        self.pending = c;
                    }
                } else {
                    let xdg = self.obj(object_id).map_or(0, |o| o.xdg_surface);
                    // A popup is transient: configured like an `xdg_surface`, but it
                    // does not take focus either.
                    let transient = xdg != 0 && self.popup_of(xdg).is_some();
                    if xdg != 0 && !self.obj(xdg).is_some_and(|o| o.configured) {
                        // An `xdg_surface`'s first commit is the client saying "I am
                        // ready": the server answers with the configuration it wants, and
                        // the buffer is not shown until the client has acked it.
                        self.configure(xdg, out)?;
                    } else if (xdg == 0 || self.obj(xdg).is_some_and(|o| o.acked))
                        && self.obj(object_id).is_some_and(|o| o.attached != 0)
                    {
                        let c = self.commit_of(object_id);
                        self.pending = c;
                    }
                    // A surface's *first* commit is a window appearing, and is how a
                    // client asks for focus; later commits are redraws and must move
                    // nothing, or a client that draws often would take focus from the one
                    // the user is looking at. The loop arbitrates between connections,
                    // which is what `focus_request` is for.
                    let first = !transient && self.obj(object_id).is_some_and(|o| !o.committed);
                    if let Some(o) = self.obj_mut(object_id) {
                        o.committed = true;
                    }
                    if first {
                        self.set_focus(object_id, out)?;
                        self.focus_request = Some(object_id);
                    }
                }
                // The damage belongs to the commit that carried it, shown or not.
                if let Some(o) = self.obj_mut(object_id) {
                    o.damage = None;
                }
                Ok(())
            }
            (Kind::XdgWmBase, protocol::xdg_wm_base_req::GET_XDG_SURFACE) => {
                let id = arg(n, &args, 0)?;
                let surface = arg(n, &args, 1)?;
                if self.kind(surface) != Some(Kind::Surface) {
                    error(
                        out,
                        id,
                        protocol::WL_DISPLAY_ERROR_INVALID_OBJECT,
                        b"xdg_surface needs a wl_surface",
                    )?;
                    return Ok(outcome);
                }
                self.add_obj(Obj {
                    surface,
                    ..Obj::new(id, Kind::XdgSurface)
                })?;
                if let Some(s) = self.obj_mut(surface) {
                    s.xdg_surface = id;
                }
                Ok(())
            }
            (Kind::XdgWmBase, protocol::xdg_wm_base_req::CREATE_POSITIONER) => {
                self.add(arg(n, &args, 0)?, Kind::XdgPositioner)
            }
            // The positioner's size is the one part of it a popup's configure needs.
            (Kind::XdgPositioner, protocol::xdg_positioner_req::SET_SIZE) => {
                let w = iarg(n, &args, 0)?;
                let h = iarg(n, &args, 1)?;
                if let Some(o) = self.obj_mut(object_id) {
                    o.width = w;
                    o.height = h;
                }
                Ok(())
            }
            // The client's answer to a `ping`; nothing waits on it yet.
            (Kind::XdgWmBase, protocol::xdg_wm_base_req::PONG) => Ok(()),
            (Kind::XdgSurface, protocol::xdg_surface_req::GET_TOPLEVEL) => {
                let id = arg(n, &args, 0)?;
                self.add_obj(Obj {
                    xdg_surface: object_id,
                    ..Obj::new(id, Kind::XdgToplevel)
                })
            }
            (Kind::XdgSurface, protocol::xdg_surface_req::GET_POPUP) => {
                let id = arg(n, &args, 0)?;
                let parent = arg(n, &args, 1)?;
                let positioner = arg(n, &args, 2)?;
                if self.kind(positioner) != Some(Kind::XdgPositioner) {
                    error(
                        out,
                        id,
                        protocol::WL_DISPLAY_ERROR_INVALID_OBJECT,
                        b"popup needs a positioner",
                    )?;
                    return Ok(outcome);
                }
                let (pw, ph) = self.obj(positioner).map_or((0, 0), |o| (o.width, o.height));
                self.add_obj(Obj {
                    xdg_surface: object_id,
                    surface: parent,
                    width: pw,
                    height: ph,
                    ..Obj::new(id, Kind::XdgPopup)
                })
            }
            (Kind::XdgSurface, protocol::xdg_surface_req::ACK_CONFIGURE)
            | (Kind::LayerSurface, protocol::layer_surface_req::ACK_CONFIGURE) => {
                let serial = arg(n, &args, 0)?;
                if let Some(o) = self.obj_mut(object_id) {
                    // A serial that does not name the latest configure is a client
                    // bug; the surface then stays unacked and is never shown.
                    if serial == o.configure_serial {
                        o.acked = true;
                    }
                }
                Ok(())
            }
            // Geometry is unused while a toplevel fills the output, and a popup's grab
            // is accepted without dismissing it (2e).
            (Kind::XdgSurface, protocol::xdg_surface_req::SET_WINDOW_GEOMETRY)
            | (Kind::XdgPopup, protocol::xdg_popup_req::GRAB)
            | (Kind::XdgPopup, protocol::xdg_popup_req::REPOSITION) => Ok(()),
            // The toplevel's window-management requests have nothing to act on until
            // there is window management, and title/app_id are the client's business
            // under the client-side decorations this phase asks for: accepted, so a
            // toolkit's mapping sequence runs to completion.
            (Kind::XdgToplevel, _) => Ok(()),
            (Kind::XdgPositioner, _) => Ok(()),
            (Kind::LayerShell, protocol::layer_shell_req::GET_LAYER_SURFACE) => {
                let id = arg(n, &args, 0)?;
                let surface = arg(n, &args, 1)?;
                let _output = arg(n, &args, 2)?;
                let layer = arg(n, &args, 3)?;
                let _namespace = str_arg(&args, 4)?;
                if self.kind(surface) != Some(Kind::Surface) {
                    error(
                        out,
                        id,
                        protocol::WL_DISPLAY_ERROR_INVALID_OBJECT,
                        b"layer surface needs a wl_surface",
                    )?;
                    return Ok(outcome);
                }
                self.add_obj(Obj {
                    surface,
                    format: layer,
                    ..Obj::new(id, Kind::LayerSurface)
                })?;
                if let Some(s) = self.obj_mut(surface) {
                    s.layer = id;
                }
                Ok(())
            }
            (Kind::LayerSurface, protocol::layer_surface_req::SET_SIZE) => {
                let w = arg(n, &args, 0)? as i32;
                let h = arg(n, &args, 1)? as i32;
                if let Some(o) = self.obj_mut(object_id) {
                    o.width = w;
                    o.height = h;
                }
                Ok(())
            }
            // Anchors, margins, exclusive zones and keyboard interactivity have
            // nothing to act on while a layer surface is placed at the output's
            // origin: accepted and ignored (2e).
            (Kind::LayerSurface, _) => Ok(()),
            (
                Kind::DecorationManager,
                protocol::decoration_manager_req::GET_TOPLEVEL_DECORATION,
            ) => {
                let id = arg(n, &args, 0)?;
                let toplevel = arg(n, &args, 1)?;
                self.add_obj(Obj {
                    surface: toplevel,
                    ..Obj::new(id, Kind::ToplevelDecoration)
                })?;
                // This port draws no decorations, so the only honest answer is
                // client-side, and it goes out as soon as the object exists.
                out.event(id, protocol::toplevel_decoration_ev::CONFIGURE, |w| {
                    w.uint(protocol::DECORATION_MODE_CLIENT_SIDE)
                })?;
                Ok(())
            }
            // A mode the client asks for is not honoured: the compositor decides, and
            // it has already said so.
            (Kind::ToplevelDecoration, protocol::toplevel_decoration_req::SET_MODE)
            | (Kind::ToplevelDecoration, protocol::toplevel_decoration_req::UNSET_MODE) => Ok(()),
            // 1c (input) and later; a client that reaches them is answered the
            // same way an unknown opcode is.
            _ => error(
                out,
                object_id,
                protocol::WL_DISPLAY_ERROR_INVALID_METHOD,
                b"unimplemented",
            ),
        }?;
        Ok(outcome)
    }

    /// Build the commit for a surface from its attached buffer, if it has one.
    fn commit_of(&self, surface: u32) -> Option<Commit> {
        let s = self.obj(surface)?;
        let b = self.obj(s.attached)?;
        let pool = self.obj(b.pool)?;
        Some(Commit {
            surface,
            buffer: b.id,
            pool: b.pool,
            fd_index: pool.fd_index,
            offset: b.offset,
            width: b.width,
            height: b.height,
            stride: b.stride,
            format: b.format,
            pool_size: pool.pool_size,
            damage: s.damage,
        })
    }

    /// The cursor image a surface supplies, from its attached buffer.
    fn cursor_of(&self, surface: u32) -> Option<CursorImage> {
        let s = self.obj(surface)?;
        let b = self.obj(s.attached)?;
        let pool = self.obj(b.pool)?;
        Some(CursorImage {
            buffer: crate::shm::Buffer {
                offset: b.offset,
                width: b.width,
                height: b.height,
                stride: b.stride,
                format: b.format,
                pool_size: pool.pool_size,
            },
            pool: b.pool,
            fd_index: pool.fd_index,
            hotspot_x: self.cursor_hotspot.0,
            hotspot_y: self.cursor_hotspot.1,
        })
    }

    /// The pointer's cursor image, if a client set one and committed it (2d). The
    /// caller composites it; nothing here touches pixels.
    pub fn cursor(&self) -> Option<CursorImage> {
        self.cursor
    }

    /// The next surface commit awaiting presentation, cleared by the call.
    pub fn take_commit(&mut self) -> Option<Commit> {
        self.pending.take()
    }

    /// Emit `wl_buffer.release`: the server is done with a buffer it presented,
    /// so the client may reuse it.
    pub fn release_buffer(
        &mut self,
        id: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        out.event(id, protocol::buffer_ev::RELEASE, |_w| Ok(()))?;
        Ok(())
    }

    /// The client's keyboard object, if it created one.
    pub fn keyboard(&self) -> Option<u32> {
        self.objs[..self.n]
            .iter()
            .find(|o| o.kind == Kind::Keyboard)
            .map(|o| o.id)
    }

    /// The client's pointer object, if it created one.
    pub fn pointer(&self) -> Option<u32> {
        self.objs[..self.n]
            .iter()
            .find(|o| o.kind == Kind::Pointer)
            .map(|o| o.id)
    }

    /// Tell the server a keymap of `size` bytes is available to serve. The bytes
    /// live with the caller, which resolves the fd; a size of 0 disables the event.
    pub fn set_keymap(&mut self, size: u32) {
        self.keymap_size = size;
    }

    /// Whether a `wl_keyboard.keymap` was emitted and its fd must go out of band
    /// with the events of this batch. Clears the flag.
    pub fn take_keymap(&mut self) -> bool {
        core::mem::take(&mut self.keymap_owed)
    }

    /// The surface whose first commit should take focus, if one happened since the
    /// last call. Clears the request; the loop between connections reads it.
    pub fn take_focus_request(&mut self) -> Option<u32> {
        self.focus_request.take()
    }

    /// Emit `wl_keyboard.keymap` for the client's keyboard, if there is both a
    /// keyboard and a keymap. The `fd` argument is the index 0 the caller resolves.
    fn emit_keymap(&mut self, out: &mut DispatchBuf<'_>) -> Result<(), DispatchError> {
        if self.keymap_size == 0 {
            return Ok(());
        }
        let Some(kb) = self.keyboard() else {
            return Ok(());
        };
        let size = self.keymap_size;
        out.event(kb, protocol::keyboard_ev::KEYMAP, |w| {
            w.uint(protocol::WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1)?;
            w.fd(0)?;
            w.uint(size)
        })?;
        self.keymap_owed = true;
        Ok(())
    }

    /// The surface input is focused on (0 = none).
    pub fn focus(&self) -> u32 {
        self.focus
    }

    /// The client's `xdg_wm_base`, if it bound one.
    fn wm_base(&self) -> Option<u32> {
        self.objs[..self.n]
            .iter()
            .find(|o| o.kind == Kind::XdgWmBase)
            .map(|o| o.id)
    }

    /// The `xdg_toplevel` belonging to an `xdg_surface`, if it made one.
    fn toplevel_of(&self, xdg: u32) -> Option<u32> {
        self.objs[..self.n]
            .iter()
            .find(|o| o.kind == Kind::XdgToplevel && o.xdg_surface == xdg)
            .map(|o| o.id)
    }

    /// The `xdg_popup` belonging to an `xdg_surface`, if there is one. A popup is
    /// transient, and this is what tells the commit path not to focus it (2e).
    fn popup_of(&self, xdg: u32) -> Option<u32> {
        self.objs[..self.n]
            .iter()
            .find(|o| o.kind == Kind::XdgPopup && o.xdg_surface == xdg)
            .map(|o| o.id)
    }

    /// Answer an `xdg_surface`'s first commit: the surface's `configure` (whose
    /// serial the client acks), the toplevel's size, and a `ping` so the client can
    /// prove it is still reading.
    ///
    /// With no window management the toplevel fills the output, so this is the one
    /// size a client is ever offered (2b).
    fn configure(&mut self, xdg: u32, out: &mut DispatchBuf<'_>) -> Result<(), DispatchError> {
        self.serial += 1;
        let serial = self.serial;
        let (width, height) = (self.width, self.height);
        out.event(xdg, protocol::xdg_surface_ev::CONFIGURE, |w| w.uint(serial))?;
        if let Some(top) = self.toplevel_of(xdg) {
            out.event(top, protocol::xdg_toplevel_ev::CONFIGURE, |w| {
                w.int(width)?;
                w.int(height)?;
                w.array(&[])
            })?;
        }
        if let Some(wm) = self.wm_base() {
            out.event(wm, protocol::xdg_wm_base_ev::PING, |w| w.uint(serial))?;
        }
        if let Some(o) = self.obj_mut(xdg) {
            o.configured = true;
            o.configure_serial = serial;
        }
        Ok(())
    }

    /// Answer a layer surface's first commit. It has no toplevel and no ping, so the
    /// one event owed is `zwlr_layer_surface_v1.configure`; a zero size is the
    /// compositor deferring to the client, which is what the surface asked for.
    fn configure_layer(
        &mut self,
        layer: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        let (width, height) = self.obj(layer).map_or((0, 0), |o| (o.width, o.height));
        self.serial += 1;
        let serial = self.serial;
        out.event(layer, protocol::layer_surface_ev::CONFIGURE, |w| {
            w.uint(serial)?;
            w.uint(width as u32)?;
            w.uint(height as u32)
        })?;
        if let Some(o) = self.obj_mut(layer) {
            o.configured = true;
            o.configure_serial = serial;
        }
        Ok(())
    }

    /// Move this connection's focus to `surface` (0 = none), emitting the `leave`
    /// and `enter` events the change owes.
    ///
    /// This is the only place `leave` is produced. Within a connection a surface's
    /// first commit calls it; between connections the loop calls it —
    /// `set_focus(0, …)` on the connection that lost the focus, `set_focus(surface,
    /// …)` on the one that took it.
    pub fn set_focus(
        &mut self,
        surface: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        if self.focus == surface && self.entered {
            return Ok(());
        }
        let old = self.focus;
        if old != 0 && self.entered {
            self.serial += 1;
            let serial = self.serial;
            if let Some(kb) = self.keyboard() {
                out.event(kb, protocol::keyboard_ev::LEAVE, |w| {
                    w.uint(serial)?;
                    w.object(old)
                })?;
            }
            if let Some(pt) = self.pointer() {
                out.event(pt, protocol::pointer_ev::LEAVE, |w| {
                    w.uint(serial)?;
                    w.object(old)
                })?;
            }
        }
        self.focus = surface;
        self.entered = false;
        self.ensure_entered(out)
    }

    /// Emit the `enter` events the focused surface is owed, once per focus change.
    ///
    /// Until `xdg_shell` exists there is one surface and it is always the focused
    /// one, so this is where "the keyboard and pointer are on your surface" is said.
    fn ensure_entered(&mut self, out: &mut DispatchBuf<'_>) -> Result<(), DispatchError> {
        if self.entered || self.focus == 0 {
            return Ok(());
        }
        let surface = self.focus;
        self.serial += 1;
        let serial = self.serial;
        if let Some(kb) = self.keyboard() {
            // The keys held at focus time: Phase 1 keeps no pressed-key set, so the
            // `enter` array is empty.
            out.event(kb, protocol::keyboard_ev::ENTER, |w| {
                w.uint(serial)?;
                w.object(surface)?;
                w.array(&[])
            })?;
        }
        if let Some(pt) = self.pointer() {
            out.event(pt, protocol::pointer_ev::ENTER, |w| {
                w.uint(serial)?;
                w.object(surface)?;
                w.fixed(0)?;
                w.fixed(0)
            })?;
        }
        self.entered = true;
        Ok(())
    }

    /// Emit `wl_keyboard.key`. `key` is an evdev keycode, `state` a
    /// [`crate::input`] state.
    pub fn send_key(
        &mut self,
        time: u32,
        key: u32,
        state: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        let Some(kb) = self.keyboard() else {
            return Ok(());
        };
        if !self.entered {
            return Ok(());
        }
        self.serial += 1;
        let serial = self.serial;
        out.event(kb, protocol::keyboard_ev::KEY, |w| {
            w.uint(serial)?;
            w.uint(time)?;
            w.uint(key)?;
            w.uint(state)
        })?;
        Ok(())
    }

    /// Emit `wl_keyboard.modifiers`.
    pub fn send_modifiers(
        &mut self,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        let Some(kb) = self.keyboard() else {
            return Ok(());
        };
        self.serial += 1;
        let serial = self.serial;
        out.event(kb, protocol::keyboard_ev::MODIFIERS, |w| {
            w.uint(serial)?;
            w.uint(depressed)?;
            w.uint(latched)?;
            w.uint(locked)?;
            w.uint(group)
        })?;
        Ok(())
    }

    /// Emit `wl_pointer.motion`. Coordinates are 24.8 fixed point, surface-relative.
    pub fn send_pointer_motion(
        &mut self,
        time: u32,
        x: i32,
        y: i32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        let Some(pt) = self.pointer() else {
            return Ok(());
        };
        if !self.entered {
            return Ok(());
        }
        out.event(pt, protocol::pointer_ev::MOTION, |w| {
            w.uint(time)?;
            w.fixed(x)?;
            w.fixed(y)
        })?;
        Ok(())
    }

    /// Emit `wl_pointer.button`.
    pub fn send_pointer_button(
        &mut self,
        time: u32,
        button: u32,
        state: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        let Some(pt) = self.pointer() else {
            return Ok(());
        };
        if !self.entered {
            return Ok(());
        }
        self.serial += 1;
        let serial = self.serial;
        out.event(pt, protocol::pointer_ev::BUTTON, |w| {
            w.uint(serial)?;
            w.uint(time)?;
            w.uint(button)?;
            w.uint(state)
        })?;
        Ok(())
    }

    /// Create a one-shot callback, send its `done`, and retire it. A `wl_surface.frame`
    /// and a `wl_display.sync` differ only in which object carries them.
    fn create_callback(&mut self, id: u32, out: &mut DispatchBuf<'_>) -> Result<(), DispatchError> {
        self.add(id, Kind::Callback)?;
        self.serial += 1;
        let serial = self.serial;
        out.event(id, protocol::callback_ev::DONE, |w| w.uint(serial))?;
        // The callback is spent; tell the client it may reuse the id.
        out.event(DISPLAY_ID, protocol::display_ev::DELETE_ID, |w| w.uint(id))?;
        self.remove(id);
        Ok(())
    }

    fn do_get_registry(&mut self, id: u32, out: &mut DispatchBuf<'_>) -> Result<(), DispatchError> {
        self.add(id, Kind::Registry)?;
        for g in protocol::GLOBALS {
            out.event(id, protocol::registry_ev::GLOBAL, |w| {
                w.uint(g.name)?;
                w.string(g.interface.as_bytes())?;
                w.uint(g.version)
            })?;
        }
        Ok(())
    }

    fn do_bind(
        &mut self,
        name: u32,
        interface: &[u8],
        _version: u32,
        id: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        let Some(g) = protocol::GLOBALS.iter().find(|g| g.name == name) else {
            return error(
                out,
                id,
                protocol::WL_DISPLAY_ERROR_INVALID_OBJECT,
                b"no such global",
            );
        };
        if g.interface.as_bytes() != interface {
            return error(
                out,
                id,
                protocol::WL_DISPLAY_ERROR_INVALID_OBJECT,
                b"global interface mismatch",
            );
        }
        let Some(kind) = Kind::from_name(interface) else {
            return error(
                out,
                id,
                protocol::WL_DISPLAY_ERROR_INVALID_OBJECT,
                b"unsupported interface",
            );
        };
        self.add(id, kind)?;
        self.initial_events(kind, id, out)
    }

    /// The events a freshly bound global starts with.
    fn initial_events(
        &self,
        kind: Kind,
        id: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), DispatchError> {
        match kind {
            Kind::Shm => {
                out.event(id, protocol::shm_ev::FORMAT, |w| {
                    w.uint(protocol::WL_SHM_FORMAT_ARGB8888)
                })?;
                out.event(id, protocol::shm_ev::FORMAT, |w| {
                    w.uint(protocol::WL_SHM_FORMAT_XRGB8888)
                })?;
                Ok(())
            }
            Kind::Output => {
                out.event(id, protocol::output_ev::GEOMETRY, |w| {
                    w.int(0)?; // x
                    w.int(0)?; // y
                    w.int(self.width)?; // physical width (mm — uninformative)
                    w.int(self.height)?; // physical height
                    w.int(protocol::WL_OUTPUT_SUBPIXEL_UNKNOWN)?;
                    w.string(b"minixrs")?;
                    w.string(b"fb")?;
                    w.int(protocol::WL_OUTPUT_TRANSFORM_NORMAL)
                })?;
                out.event(id, protocol::output_ev::MODE, |w| {
                    w.uint(protocol::WL_OUTPUT_MODE_CURRENT | protocol::WL_OUTPUT_MODE_PREFERRED)?;
                    w.int(self.width)?;
                    w.int(self.height)?;
                    w.int(60_000) // 60 Hz
                })?;
                Ok(())
            }
            Kind::Seat => {
                // Pointer and keyboard since 1c; touch has no backend here.
                out.event(id, protocol::seat_ev::CAPABILITIES, |w| {
                    w.uint(
                        protocol::WL_SEAT_CAPABILITY_POINTER
                            | protocol::WL_SEAT_CAPABILITY_KEYBOARD,
                    )
                })?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// The `n`-th argument as a `uint`, or a protocol error if the count is short.
fn arg(count: usize, args: &[Arg<'_>], i: usize) -> Result<u32, DispatchError> {
    if i >= count {
        return Err(DispatchError::Wire(WireError::TooManyArgs));
    }
    args[i]
        .as_uint()
        .ok_or(DispatchError::Wire(WireError::BadSignature))
}

fn str_arg<'a>(args: &[Arg<'a>], i: usize) -> Result<&'a [u8], DispatchError> {
    args[i]
        .as_str()
        .ok_or(DispatchError::Wire(WireError::BadSignature))
}

/// The `n`-th argument as an `int`.
fn iarg(count: usize, args: &[Arg<'_>], i: usize) -> Result<i32, DispatchError> {
    if i >= count {
        return Err(DispatchError::Wire(WireError::TooManyArgs));
    }
    args[i]
        .as_int()
        .ok_or(DispatchError::Wire(WireError::BadSignature))
}

/// Grow a damage rectangle to cover another, which is how a surface's damage
/// accumulates between two commits (2d).
fn grow_damage(slot: &mut Option<crate::shm::Rect>, x: i32, y: i32, w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    let r = crate::shm::Rect { x, y, w, h };
    *slot = Some(match *slot {
        None => r,
        Some(p) => {
            let x0 = p.x.min(r.x);
            let y0 = p.y.min(r.y);
            let x1 = (p.x + p.w).max(r.x + r.w);
            let y1 = (p.y + p.h).max(r.y + r.h);
            crate::shm::Rect {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            }
        }
    });
}

/// Write a `wl_display.error` event.
fn error(
    out: &mut DispatchBuf<'_>,
    object_id: u32,
    code: u32,
    message: &[u8],
) -> Result<(), DispatchError> {
    out.event(DISPLAY_ID, protocol::display_ev::ERROR, |w| {
        w.uint(object_id)?;
        w.uint(code)?;
        w.string(message)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Header, MessageBuffer, Writer};

    /// Run the server over a buffer of request messages and collect the events.
    fn run(s: &mut Server, requests: &[u8]) -> [u8; 2048] {
        let mut out = [0u8; 2048];
        let mut mb = MessageBuffer::new();
        mb.push(requests).unwrap();
        let mut total = 0usize;
        while let Some(msg) = mb.next().unwrap() {
            let h = Header::parse(msg).unwrap();
            let mut buf = DispatchBuf::new(&mut out[total..]);
            s.dispatch(h.object_id, h.opcode, &msg[8..], &mut buf)
                .unwrap();
            total += buf.len();
            mb.consume(h.size as usize);
        }
        out
    }

    fn event(events: &[u8], i: usize) -> (Header, &[u8]) {
        let mut off = 0usize;
        for _ in 0..i {
            let h = Header::parse(&events[off..]).unwrap();
            off += h.size as usize;
        }
        let h = Header::parse(&events[off..]).unwrap();
        (h, &events[off + 8..off + h.size as usize])
    }

    fn req<F: Fn(&mut Writer<'_>) -> Result<(), WireError>>(
        id: u32,
        opcode: u16,
        f: F,
    ) -> ([u8; 64], usize) {
        let mut buf = [0u8; 64];
        let size = {
            let mut w = Writer::new(&mut buf).unwrap();
            f(&mut w).unwrap();
            w.finish(id, opcode).unwrap().len()
        };
        (buf, size)
    }

    /// Dispatch a run of exact messages and collect their events.
    fn run_msgs(s: &mut Server, msgs: &[&[u8]]) -> [u8; 2048] {
        let mut stream = [0u8; 512];
        let mut off = 0usize;
        for m in msgs {
            stream[off..off + m.len()].copy_from_slice(m);
            off += m.len();
        }
        run(s, &stream[..off])
    }

    #[test]
    fn sync_creates_a_callback_and_completes_it() {
        let mut s = Server::new(1280, 1024);
        let (m, mn) = req(DISPLAY_ID, protocol::display_req::SYNC, |w| w.new_id(2));
        let out = run_msgs(&mut s, &[&m[..mn]]);
        // First event: wl_callback.done on id 2.
        let (h0, b0) = event(&out, 0);
        assert_eq!(h0.object_id, 2);
        assert_eq!(h0.opcode, protocol::callback_ev::DONE);
        assert_eq!(u32::from_le_bytes(b0[0..4].try_into().unwrap()), 1);
        // Second: wl_display.delete_id on id 1.
        let (h1, _) = event(&out, 1);
        assert_eq!(h1.object_id, DISPLAY_ID);
        assert_eq!(h1.opcode, protocol::display_ev::DELETE_ID);
        // The callback is gone.
        assert_eq!(s.kind(2), None);
    }

    #[test]
    fn get_registry_advertises_every_global() {
        let mut s = Server::new(1280, 1024);
        let (m, mn) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let out = run_msgs(&mut s, &[&m[..mn]]);
        assert_eq!(s.kind(2), Some(Kind::Registry));
        for (i, g) in protocol::GLOBALS.iter().enumerate() {
            let (h, b) = event(&out, i);
            assert_eq!(h.object_id, 2);
            assert_eq!(h.opcode, protocol::registry_ev::GLOBAL);
            assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), g.name);
        }
    }

    #[test]
    fn bind_creates_the_object_and_sends_initial_events() {
        let mut s = Server::new(1280, 1024);
        // get_registry, then bind wl_shm (global 2) to id 3.
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(2)?; // name
            w.string(b"wl_shm")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let out = run_msgs(&mut s, &[&a[..an], &b[..bn]]);
        assert_eq!(s.kind(3), Some(Kind::Shm));
        // The shm initial events are the two formats, after the registry globals.
        let (_, b0) = event(&out, protocol::GLOBALS.len());
        assert_eq!(
            u32::from_le_bytes(b0[0..4].try_into().unwrap()),
            protocol::WL_SHM_FORMAT_ARGB8888
        );
    }

    #[test]
    fn unknown_object_is_a_protocol_error_not_a_panic() {
        let mut s = Server::new(1280, 1024);
        let (m, mn) = req(99, 0, |_w| Ok(()));
        let out = run_msgs(&mut s, &[&m[..mn]]);
        let (h, b) = event(&out, 0);
        assert_eq!(h.object_id, DISPLAY_ID);
        assert_eq!(h.opcode, protocol::display_ev::ERROR);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 99);
    }

    #[test]
    fn create_surface_registers_the_object() {
        let mut s = Server::new(1280, 1024);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(3, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(4));
        run_msgs(&mut s, &[&a[..an], &b[..bn], &c[..cn]]);
        assert_eq!(s.kind(4), Some(Kind::Surface));
    }

    #[test]
    fn a_buffer_that_does_not_fit_its_pool_is_a_wl_shm_error() {
        let mut s = Server::new(64, 64);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(2)?;
            w.string(b"wl_shm")?;
            w.uint(1)?;
            w.new_id(3)
        });
        // A 16-byte pool.
        let (c, cn) = req(3, protocol::shm_req::CREATE_POOL, |w| {
            w.new_id(4)?;
            w.fd(0)?;
            w.int(16)
        });
        // A 2x2 image is 16 bytes, but at offset 8 it runs to 24.
        let (d, dn) = req(4, protocol::shm_pool_req::CREATE_BUFFER, |w| {
            w.new_id(5)?;
            w.int(8)?;
            w.int(2)?;
            w.int(2)?;
            w.int(8)?;
            w.uint(protocol::WL_SHM_FORMAT_ARGB8888)
        });
        let out = run_msgs(&mut s, &[&a[..an], &b[..bn], &c[..cn], &d[..dn]]);
        // Events: four globals, two shm formats, then the error on the pool.
        let (h, body) = event(&out, protocol::GLOBALS.len() + 2);
        assert_eq!(h.object_id, DISPLAY_ID);
        assert_eq!(h.opcode, protocol::display_ev::ERROR);
        assert_eq!(u32::from_le_bytes(body[0..4].try_into().unwrap()), 4);
        assert_eq!(
            u32::from_le_bytes(body[4..8].try_into().unwrap()),
            protocol::WL_SHM_ERROR_INVALID_STRIDE
        );
        // The rejected buffer was never created.
        assert_eq!(s.kind(5), None);
    }

    /// `wl_shm_pool.resize` is part of v1 and is how a client that outgrows its pool
    /// grows it. A buffer is validated against the size the pool was last told, not
    /// the one it was created with.
    #[test]
    fn a_pool_resize_sets_the_size_buffers_are_validated_against() {
        let mut s = Server::new(64, 64);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(2)?;
            w.string(b"wl_shm")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(3, protocol::shm_req::CREATE_POOL, |w| {
            w.new_id(4)?;
            w.fd(0)?;
            w.int(16)
        });
        // 16 bytes is too small for a 2x2 image at offset 8 (24 bytes); 32 is not.
        let (grow, grow_n) = req(4, protocol::shm_pool_req::RESIZE, |w| w.int(32));
        let (d, dn) = req(4, protocol::shm_pool_req::CREATE_BUFFER, |w| {
            w.new_id(5)?;
            w.int(8)?;
            w.int(2)?;
            w.int(2)?;
            w.int(8)?;
            w.uint(protocol::WL_SHM_FORMAT_ARGB8888)
        });
        run_msgs(
            &mut s,
            &[&a[..an], &b[..bn], &c[..cn], &grow[..grow_n], &d[..dn]],
        );
        assert_eq!(s.kind(5), Some(Kind::Buffer));

        // Shrinking it again puts the same buffer back out of bounds.
        let (shrink, shrink_n) = req(4, protocol::shm_pool_req::RESIZE, |w| w.int(8));
        let (e, e_n) = req(4, protocol::shm_pool_req::CREATE_BUFFER, |w| {
            w.new_id(6)?;
            w.int(8)?;
            w.int(2)?;
            w.int(2)?;
            w.int(8)?;
            w.uint(protocol::WL_SHM_FORMAT_ARGB8888)
        });
        let out = run_msgs(&mut s, &[&shrink[..shrink_n], &e[..e_n]]);
        let (h, body) = event(&out, 0);
        assert_eq!(h.opcode, protocol::display_ev::ERROR);
        assert_eq!(u32::from_le_bytes(body[0..4].try_into().unwrap()), 4);
        assert_eq!(s.kind(6), None);
    }

    #[test]
    fn commit_carries_the_attached_buffers_geometry() {
        let mut s = Server::new(64, 64);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(2)?;
            w.string(b"wl_shm")?;
            w.uint(1)?;
            w.new_id(4)
        });
        let (d, dn) = req(3, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(5));
        let (e, en) = req(4, protocol::shm_req::CREATE_POOL, |w| {
            w.new_id(6)?;
            w.fd(0)?;
            w.int(64)
        });
        let (f, fs) = req(6, protocol::shm_pool_req::CREATE_BUFFER, |w| {
            w.new_id(7)?;
            w.int(4)?;
            w.int(2)?;
            w.int(2)?;
            w.int(8)?;
            w.uint(protocol::WL_SHM_FORMAT_XRGB8888)
        });
        let (g, gs) = req(5, protocol::surface_req::ATTACH, |w| {
            w.object(7)?;
            w.int(0)?;
            w.int(0)
        });
        let (h, hs) = req(5, protocol::surface_req::COMMIT, |_w| Ok(()));
        run_msgs(
            &mut s,
            &[
                &a[..an],
                &b[..bn],
                &c[..cn],
                &d[..dn],
                &e[..en],
                &f[..fs],
                &g[..gs],
                &h[..hs],
            ],
        );
        let commit = s.take_commit().expect("a commit");
        assert_eq!(commit.surface, 5);
        assert_eq!(commit.buffer, 7);
        assert_eq!(commit.pool, 6);
        assert_eq!(commit.fd_index, 0);
        assert_eq!(commit.offset, 4);
        assert_eq!(commit.width, 2);
        assert_eq!(commit.height, 2);
        assert_eq!(commit.stride, 8);
        assert_eq!(commit.format, protocol::WL_SHM_FORMAT_XRGB8888);
        assert_eq!(commit.pool_size, 64);
        assert_eq!(commit.buffer().validate(), Ok(()));
        // A commit is taken once.
        assert!(s.take_commit().is_none());
    }

    #[test]
    fn seat_advertises_pointer_and_keyboard() {
        let mut s = Server::new(1280, 1024);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(4)?; // wl_seat's registry name
            w.string(b"wl_seat")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let out = run_msgs(&mut s, &[&a[..an], &b[..bn]]);
        let (h, body) = event(&out, protocol::GLOBALS.len());
        assert_eq!(h.object_id, 3);
        assert_eq!(h.opcode, protocol::seat_ev::CAPABILITIES);
        let caps = u32::from_le_bytes(body[0..4].try_into().unwrap());
        assert_eq!(
            caps & protocol::WL_SEAT_CAPABILITY_POINTER,
            protocol::WL_SEAT_CAPABILITY_POINTER
        );
        assert_eq!(
            caps & protocol::WL_SEAT_CAPABILITY_KEYBOARD,
            protocol::WL_SEAT_CAPABILITY_KEYBOARD
        );
    }

    /// The whole Phase 1c path in the pure layer: a client makes a seat's keyboard
    /// and pointer, commits a surface, and gets `enter` for it; then the server's
    /// input API produces the events that surface is owed.
    #[test]
    fn a_committed_surface_is_entered_and_then_gets_input() {
        let mut s = Server::new(1280, 1024);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(4)?;
            w.string(b"wl_seat")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(3, protocol::seat_req::GET_KEYBOARD, |w| w.new_id(4));
        let (d, dn) = req(3, protocol::seat_req::GET_POINTER, |w| w.new_id(5));
        let (e, en) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(1)?;
            w.new_id(6)
        });
        let (f, fs) = req(6, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(7));
        let (g, gs) = req(7, protocol::surface_req::COMMIT, |_w| Ok(()));
        let out = run_msgs(
            &mut s,
            &[
                &a[..an],
                &b[..bn],
                &c[..cn],
                &d[..dn],
                &e[..en],
                &f[..fs],
                &g[..gs],
            ],
        );

        // Four globals and the seat capabilities, then the two `enter` events the
        // commit owed. Making the keyboard/pointer before the commit sent nothing.
        assert_eq!(s.focus(), 7);
        let (hk, kb) = event(&out, protocol::GLOBALS.len() + 1);
        assert_eq!(hk.object_id, 4, "keyboard enter");
        assert_eq!(hk.opcode, protocol::keyboard_ev::ENTER);
        assert_eq!(
            u32::from_le_bytes(kb[4..8].try_into().unwrap()),
            7,
            "surface"
        );
        assert_eq!(
            u32::from_le_bytes(kb[8..12].try_into().unwrap()),
            0,
            "no keys held"
        );
        let (hp, _) = event(&out, protocol::GLOBALS.len() + 2);
        assert_eq!(hp.object_id, 5, "pointer enter");
        assert_eq!(hp.opcode, protocol::pointer_ev::ENTER);

        // A commit that does not change focus must not re-enter.
        let (g2, g2s) = req(7, protocol::surface_req::COMMIT, |_w| Ok(()));
        let out = run_msgs(&mut s, &[&g2[..g2s]]);
        assert!(out[..8].iter().all(|&b| b == 0), "no duplicate enter");
    }

    #[test]
    fn input_events_are_framed_for_the_focused_client() {
        let mut s = Server::new(1280, 1024);
        // Handshake: seat with keyboard + pointer, a surface, and a commit.
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(4)?;
            w.string(b"wl_seat")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(3, protocol::seat_req::GET_KEYBOARD, |w| w.new_id(4));
        let (d, dn) = req(3, protocol::seat_req::GET_POINTER, |w| w.new_id(5));
        let (e, en) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(1)?;
            w.new_id(6)
        });
        let (f, fs) = req(6, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(7));
        let (g, gs) = req(7, protocol::surface_req::COMMIT, |_w| Ok(()));
        run_msgs(
            &mut s,
            &[
                &a[..an],
                &b[..bn],
                &c[..cn],
                &d[..dn],
                &e[..en],
                &f[..fs],
                &g[..gs],
            ],
        );

        let mut buf = [0u8; 512];
        let mut db = DispatchBuf::new(&mut buf);
        s.send_key(1234, 30, crate::input::STATE_PRESSED, &mut db)
            .unwrap();
        s.send_modifiers(crate::input::MOD_SHIFT, 0, 0, 0, &mut db)
            .unwrap();
        s.send_pointer_motion(1234, 64 << 8, 128 << 8, &mut db)
            .unwrap();
        s.send_pointer_button(
            1234,
            crate::input::BTN_LEFT,
            crate::input::STATE_PRESSED,
            &mut db,
        )
        .unwrap();

        let (h0, b0) = event(db.bytes(), 0);
        assert_eq!(h0.object_id, 4);
        assert_eq!(h0.opcode, protocol::keyboard_ev::KEY);
        assert_eq!(u32::from_le_bytes(b0[4..8].try_into().unwrap()), 1234); // time
        assert_eq!(u32::from_le_bytes(b0[8..12].try_into().unwrap()), 30); // evdev 'a'
        assert_eq!(
            u32::from_le_bytes(b0[12..16].try_into().unwrap()),
            crate::input::STATE_PRESSED
        );

        let (h1, b1) = event(db.bytes(), 1);
        assert_eq!(h1.object_id, 4);
        assert_eq!(h1.opcode, protocol::keyboard_ev::MODIFIERS);
        assert_eq!(
            u32::from_le_bytes(b1[4..8].try_into().unwrap()),
            crate::input::MOD_SHIFT
        );

        let (h2, b2) = event(db.bytes(), 2);
        assert_eq!(h2.object_id, 5);
        assert_eq!(h2.opcode, protocol::pointer_ev::MOTION);
        assert_eq!(i32::from_le_bytes(b2[4..8].try_into().unwrap()), 64 << 8);
        assert_eq!(i32::from_le_bytes(b2[8..12].try_into().unwrap()), 128 << 8);

        let (h3, b3) = event(db.bytes(), 3);
        assert_eq!(h3.object_id, 5);
        assert_eq!(h3.opcode, protocol::pointer_ev::BUTTON);
        assert_eq!(
            u32::from_le_bytes(b3[8..12].try_into().unwrap()),
            crate::input::BTN_LEFT
        );
    }

    /// Phase 2a in the pure layer: a keymap, once set, is served to the client's
    /// keyboard as `wl_keyboard.keymap` — XKB_V1, fd index 0 — and the caller is
    /// told the fd owes one out-of-band send.
    #[test]
    fn a_keymap_is_served_to_the_keyboard() {
        let mut s = Server::new(1280, 1024);
        s.set_keymap(64757);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(4)?;
            w.string(b"wl_seat")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(3, protocol::seat_req::GET_KEYBOARD, |w| w.new_id(4));
        let out = run_msgs(&mut s, &[&a[..an], &b[..bn], &c[..cn]]);

        // Four globals and the seat's capabilities, then the keymap.
        let (h, body) = event(&out, protocol::GLOBALS.len() + 1);
        assert_eq!(h.object_id, 4, "keyboard keymap");
        assert_eq!(h.opcode, protocol::keyboard_ev::KEYMAP);
        assert_eq!(
            u32::from_le_bytes(body[0..4].try_into().unwrap()),
            protocol::WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1
        );
        assert_eq!(
            u32::from_le_bytes(body[4..8].try_into().unwrap()),
            0,
            "the fd is at index 0"
        );
        assert_eq!(u32::from_le_bytes(body[8..12].try_into().unwrap()), 64757);

        assert!(s.take_keymap(), "the fd owes an out-of-band send");
        assert!(!s.take_keymap(), "and only once");
    }

    /// With no keymap set, the keyboard is created without a `keymap` event: a guest
    /// that cannot serve one still reaches a client (degrade, do not gate).
    #[test]
    fn no_keymap_means_no_keymap_event() {
        let mut s = Server::new(1280, 1024);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(4)?;
            w.string(b"wl_seat")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(3, protocol::seat_req::GET_KEYBOARD, |w| w.new_id(4));
        let out = run_msgs(&mut s, &[&a[..an], &b[..bn], &c[..cn]]);

        assert!(!s.take_keymap());
        // The trailing buffer is zeroed, so the walk stops at the capabilities event.
        let mut off = 0usize;
        let mut count = 0usize;
        while let Ok(h) = Header::parse(&out[off..]) {
            off += h.size as usize;
            count += 1;
        }
        assert_eq!(
            count,
            protocol::GLOBALS.len() + 1,
            "capabilities and no more"
        );
    }

    /// Phase 2b in the pure layer: an `xdg_surface`'s first commit is answered with a
    /// `configure` (surface serial, toplevel size, a ping), and a buffer is only
    /// presentable once the client has acked that serial.
    #[test]
    fn an_xdg_toplevel_configures_acks_and_then_presents() {
        let mut s = Server::new(1024, 768);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(5)?;
            w.string(b"xdg_wm_base")?;
            w.uint(1)?;
            w.new_id(4)
        });
        let (d, dn) = req(3, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(5));
        let (e, en) = req(4, protocol::xdg_wm_base_req::GET_XDG_SURFACE, |w| {
            w.new_id(6)?;
            w.object(5)
        });
        let (f, fs) = req(6, protocol::xdg_surface_req::GET_TOPLEVEL, |w| w.new_id(7));
        let (g, gs) = req(7, protocol::xdg_toplevel_req::SET_TITLE, |w| {
            w.string(b"wlx")
        });
        let (h, hs) = req(5, protocol::surface_req::COMMIT, |_w| Ok(()));
        let out = run_msgs(
            &mut s,
            &[
                &a[..an],
                &b[..bn],
                &c[..cn],
                &d[..dn],
                &e[..en],
                &f[..fs],
                &g[..gs],
                &h[..hs],
            ],
        );

        // Five globals, then the configure the initial commit earned.
        let (h0, b0) = event(&out, protocol::GLOBALS.len());
        assert_eq!(h0.object_id, 6, "xdg_surface configure");
        assert_eq!(h0.opcode, protocol::xdg_surface_ev::CONFIGURE);
        let serial = u32::from_le_bytes(b0[0..4].try_into().unwrap());
        let (h1, b1) = event(&out, protocol::GLOBALS.len() + 1);
        assert_eq!(h1.object_id, 7, "xdg_toplevel configure");
        assert_eq!(h1.opcode, protocol::xdg_toplevel_ev::CONFIGURE);
        assert_eq!(i32::from_le_bytes(b1[0..4].try_into().unwrap()), 1024);
        assert_eq!(i32::from_le_bytes(b1[4..8].try_into().unwrap()), 768);
        assert_eq!(
            i32::from_le_bytes(b1[8..12].try_into().unwrap()),
            0,
            "no states"
        );
        let (h2, b2) = event(&out, protocol::GLOBALS.len() + 2);
        assert_eq!(h2.object_id, 4, "wm_base ping");
        assert_eq!(h2.opcode, protocol::xdg_wm_base_ev::PING);
        assert_eq!(u32::from_le_bytes(b2[0..4].try_into().unwrap()), serial);

        // A pool, a buffer and an attach, so a commit has pixels to offer.
        let (i, i_n) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(2)?;
            w.string(b"wl_shm")?;
            w.uint(1)?;
            w.new_id(8)
        });
        let (j, j_n) = req(8, protocol::shm_req::CREATE_POOL, |w| {
            w.new_id(9)?;
            w.fd(0)?;
            w.int(1024 * 768 * 4)
        });
        let (k, k_n) = req(9, protocol::shm_pool_req::CREATE_BUFFER, |w| {
            w.new_id(10)?;
            w.int(0)?;
            w.int(1024)?;
            w.int(768)?;
            w.int(4096)?;
            w.uint(protocol::WL_SHM_FORMAT_XRGB8888)
        });
        let (l, l_n) = req(5, protocol::surface_req::ATTACH, |w| {
            w.object(10)?;
            w.int(0)?;
            w.int(0)
        });
        let (m, m_n) = req(5, protocol::surface_req::COMMIT, |_w| Ok(()));
        run_msgs(
            &mut s,
            &[&i[..i_n], &j[..j_n], &k[..k_n], &l[..l_n], &m[..m_n]],
        );
        assert!(
            s.take_commit().is_none(),
            "a buffer committed before ack is not presentable"
        );

        // Ack the configure; the same commit is now presentable, at the size the
        // toplevel was configured with.
        let (n_, n_n) = req(6, protocol::xdg_surface_req::ACK_CONFIGURE, |w| {
            w.uint(serial)
        });
        let (o, o_n) = req(5, protocol::surface_req::COMMIT, |_w| Ok(()));
        run_msgs(&mut s, &[&n_[..n_n], &o[..o_n]]);
        let c = s.take_commit().expect("an acked commit is presentable");
        assert_eq!(c.surface, 5);
        assert_eq!(c.buffer, 10);
        assert_eq!(c.width, 1024);
        assert_eq!(c.height, 768);
    }

    /// Focus moving between a client's own surfaces says `leave` for the surface it
    /// left and `enter` for the one it moved to (2c) — which is the machinery the loop
    /// uses when the move is between *connections* instead.
    #[test]
    fn focus_moves_emit_leave_then_enter() {
        let mut s = Server::new(1024, 768);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(4)?;
            w.string(b"wl_seat")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(3, protocol::seat_req::GET_KEYBOARD, |w| w.new_id(4));
        let (d, dn) = req(3, protocol::seat_req::GET_POINTER, |w| w.new_id(5));
        let (e, en) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(1)?;
            w.new_id(6)
        });
        let (f, fs) = req(6, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(7));
        let (g, gs) = req(6, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(8));
        let (h, hs) = req(7, protocol::surface_req::COMMIT, |_w| Ok(()));
        let (i, i_n) = req(8, protocol::surface_req::COMMIT, |_w| Ok(()));
        let out = run_msgs(
            &mut s,
            &[
                &a[..an],
                &b[..bn],
                &c[..cn],
                &d[..dn],
                &e[..en],
                &f[..fs],
                &g[..gs],
                &h[..hs],
                &i[..i_n],
            ],
        );

        // Globals, the seat's capabilities, then enter(7) on both, then leave(7) on
        // both and enter(8) on both.
        assert_eq!(s.focus(), 8);
        let (l0, b0) = event(&out, protocol::GLOBALS.len() + 3);
        assert_eq!(l0.object_id, 4, "keyboard leave");
        assert_eq!(l0.opcode, protocol::keyboard_ev::LEAVE);
        assert_eq!(u32::from_le_bytes(b0[4..8].try_into().unwrap()), 7);
        let (l1, _) = event(&out, protocol::GLOBALS.len() + 4);
        assert_eq!(l1.object_id, 5, "pointer leave");
        assert_eq!(l1.opcode, protocol::pointer_ev::LEAVE);
        let (e0, e0b) = event(&out, protocol::GLOBALS.len() + 5);
        assert_eq!(e0.object_id, 4, "keyboard enter");
        assert_eq!(e0.opcode, protocol::keyboard_ev::ENTER);
        assert_eq!(u32::from_le_bytes(e0b[4..8].try_into().unwrap()), 8);
        let (e1, _) = event(&out, protocol::GLOBALS.len() + 6);
        assert_eq!(e1.object_id, 5, "pointer enter");
        assert_eq!(e1.opcode, protocol::pointer_ev::ENTER);
    }

    /// Phase 2d in the pure layer: damage accumulates into the bounding rectangle of
    /// everything marked since the last commit, and is consumed by that commit; and a
    /// `set_cursor` surface's commit is taken as the pointer image instead of being
    /// presented as a window.
    #[test]
    fn damage_accumulates_and_a_cursor_surface_is_not_a_window() {
        let mut s = Server::new(1024, 768);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(2)?;
            w.string(b"wl_shm")?;
            w.uint(1)?;
            w.new_id(4)
        });
        let (d, dn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(4)?;
            w.string(b"wl_seat")?;
            w.uint(1)?;
            w.new_id(5)
        });
        let (e, en) = req(5, protocol::seat_req::GET_POINTER, |w| w.new_id(6));
        let (f, fs) = req(3, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(7));
        let (g, gs) = req(3, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(8));
        let (h, hs) = req(4, protocol::shm_req::CREATE_POOL, |w| {
            w.new_id(9)?;
            w.fd(0)?;
            w.int(4096)
        });
        let (i, i_n) = req(9, protocol::shm_pool_req::CREATE_BUFFER, |w| {
            w.new_id(10)?;
            w.int(0)?;
            w.int(4)?;
            w.int(4)?;
            w.int(16)?;
            w.uint(protocol::WL_SHM_FORMAT_XRGB8888)
        });
        let (j, j_n) = req(9, protocol::shm_pool_req::CREATE_BUFFER, |w| {
            w.new_id(11)?;
            w.int(64)?;
            w.int(8)?;
            w.int(8)?;
            w.int(32)?;
            w.uint(protocol::WL_SHM_FORMAT_ARGB8888)
        });
        let (k, k_n) = req(7, protocol::surface_req::ATTACH, |w| {
            w.object(10)?;
            w.int(0)?;
            w.int(0)
        });
        let (l, l_n) = req(7, protocol::surface_req::DAMAGE, |w| {
            w.int(0)?;
            w.int(0)?;
            w.int(4)?;
            w.int(4)
        });
        let (m, m_n) = req(7, protocol::surface_req::DAMAGE_BUFFER, |w| {
            w.int(10)?;
            w.int(10)?;
            w.int(4)?;
            w.int(4)
        });
        let (n_, n_n) = req(7, protocol::surface_req::COMMIT, |_w| Ok(()));
        run_msgs(
            &mut s,
            &[
                &a[..an],
                &b[..bn],
                &c[..cn],
                &d[..dn],
                &e[..en],
                &f[..fs],
                &g[..gs],
                &h[..hs],
                &i[..i_n],
                &j[..j_n],
                &k[..k_n],
                &l[..l_n],
                &m[..m_n],
                &n_[..n_n],
            ],
        );
        // (0,0,4,4) grown by (10,10,4,4) is the box between them.
        let commit = s.take_commit().expect("the window commit is presentable");
        assert_eq!(
            commit.damage,
            Some(crate::shm::Rect {
                x: 0,
                y: 0,
                w: 14,
                h: 14
            })
        );
        // And the damage is spent: a second commit carries none of it.
        let (o, o_n) = req(7, protocol::surface_req::COMMIT, |_w| Ok(()));
        run_msgs(&mut s, &[&o[..o_n]]);
        assert_eq!(s.take_commit().unwrap().damage, None);

        // A cursor surface: `set_cursor` names it, and its commit is the image.
        let (p, p_n) = req(6, protocol::pointer_req::SET_CURSOR, |w| {
            w.uint(0)?;
            w.object(8)?;
            w.int(1)?;
            w.int(1)
        });
        let (q, q_n) = req(8, protocol::surface_req::ATTACH, |w| {
            w.object(11)?;
            w.int(0)?;
            w.int(0)
        });
        let (r, r_n) = req(8, protocol::surface_req::COMMIT, |_w| Ok(()));
        run_msgs(&mut s, &[&p[..p_n], &q[..q_n], &r[..r_n]]);
        assert!(
            s.take_commit().is_none(),
            "a cursor surface is not presented as a window"
        );
        let cur = s.cursor().expect("the cursor image was taken");
        assert_eq!(cur.pool, 9);
        assert_eq!((cur.hotspot_x, cur.hotspot_y), (1, 1));
        assert_eq!((cur.buffer.width, cur.buffer.height), (8, 8));
    }

    /// Phase 2e in the pure layer: a layer surface's first commit is answered with
    /// `zwlr_layer_surface_v1.configure`, a popup's with `xdg_surface.configure`,
    /// neither takes focus, and a decoration manager answers client-side as soon as
    /// a decoration is asked for.
    #[test]
    fn layer_popup_and_decoration_configure_without_focus() {
        let mut s = Server::new(1024, 768);
        let (a, an) = req(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(2)
        });
        let (b, bn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(1)?;
            w.new_id(3)
        });
        let (c, cn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(5)?;
            w.string(b"xdg_wm_base")?;
            w.uint(1)?;
            w.new_id(4)
        });
        let (d, dn) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(6)?;
            w.string(b"zwlr_layer_shell_v1")?;
            w.uint(1)?;
            w.new_id(5)
        });
        let (e, en) = req(2, protocol::registry_req::BIND, |w| {
            w.uint(7)?;
            w.string(b"zxdg_decoration_manager_v1")?;
            w.uint(1)?;
            w.new_id(6)
        });
        // The toplevel the popup will hang off.
        let (f, fs) = req(3, protocol::compositor_req::CREATE_SURFACE, |w| w.new_id(7));
        let (g, gs) = req(4, protocol::xdg_wm_base_req::GET_XDG_SURFACE, |w| {
            w.new_id(8)?;
            w.object(7)
        });
        let (h, hs) = req(8, protocol::xdg_surface_req::GET_TOPLEVEL, |w| w.new_id(9));
        let (i, i_n) = req(
            6,
            protocol::decoration_manager_req::GET_TOPLEVEL_DECORATION,
            |w| {
                w.new_id(10)?;
                w.object(9)
            },
        );
        let out = run_msgs(
            &mut s,
            &[
                &a[..an],
                &b[..bn],
                &c[..cn],
                &d[..dn],
                &e[..en],
                &f[..fs],
                &g[..gs],
                &h[..hs],
                &i[..i_n],
            ],
        );
        // Seven globals, then the decoration answered at once: this port draws none,
        // so the only honest answer is client-side.
        let (d0, d0b) = event(&out, protocol::GLOBALS.len());
        assert_eq!(d0.object_id, 10, "toplevel decoration");
        assert_eq!(d0.opcode, protocol::toplevel_decoration_ev::CONFIGURE);
        assert_eq!(
            u32::from_le_bytes(d0b[0..4].try_into().unwrap()),
            protocol::DECORATION_MODE_CLIENT_SIDE
        );

        // A layer surface: its first commit configures it at the size it asked for,
        // and a panel is not where input goes, so it must not take focus (2e).
        let (j, j_n) = req(3, protocol::compositor_req::CREATE_SURFACE, |w| {
            w.new_id(11)
        });
        let (k, k_n) = req(5, protocol::layer_shell_req::GET_LAYER_SURFACE, |w| {
            w.new_id(12)?;
            w.object(11)?;
            w.object(0)?;
            w.uint(protocol::LAYER_TOP)?;
            w.string(b"panel")
        });
        let (l, l_n) = req(12, protocol::layer_surface_req::SET_SIZE, |w| {
            w.uint(200)?;
            w.uint(32)
        });
        let (m, m_n) = req(11, protocol::surface_req::COMMIT, |_w| Ok(()));
        let out = run_msgs(&mut s, &[&j[..j_n], &k[..k_n], &l[..l_n], &m[..m_n]]);
        let (l0, l0b) = event(&out, 0);
        assert_eq!(l0.object_id, 12, "layer surface configure");
        assert_eq!(l0.opcode, protocol::layer_surface_ev::CONFIGURE);
        let serial = u32::from_le_bytes(l0b[0..4].try_into().unwrap());
        assert_eq!(u32::from_le_bytes(l0b[4..8].try_into().unwrap()), 200);
        assert_eq!(u32::from_le_bytes(l0b[8..12].try_into().unwrap()), 32);
        assert!(
            s.take_focus_request().is_none(),
            "a panel does not take focus"
        );

        // A popup over the toplevel: a positioner sizes it, and its first commit is
        // answered with the `xdg_surface.configure` a transient surface gets — again
        // without taking focus.
        let (n_, n_n) = req(3, protocol::compositor_req::CREATE_SURFACE, |w| {
            w.new_id(13)
        });
        let (o, o_n) = req(4, protocol::xdg_wm_base_req::GET_XDG_SURFACE, |w| {
            w.new_id(14)?;
            w.object(13)
        });
        let (p, p_n) = req(4, protocol::xdg_wm_base_req::CREATE_POSITIONER, |w| {
            w.new_id(15)
        });
        let (q, q_n) = req(15, protocol::xdg_positioner_req::SET_SIZE, |w| {
            w.int(8)?;
            w.int(8)
        });
        let (r, r_n) = req(14, protocol::xdg_surface_req::GET_POPUP, |w| {
            w.new_id(16)?;
            w.object(8)?;
            w.object(15)
        });
        let (t, t_n) = req(13, protocol::surface_req::COMMIT, |_w| Ok(()));
        let out = run_msgs(
            &mut s,
            &[
                &n_[..n_n],
                &o[..o_n],
                &p[..p_n],
                &q[..q_n],
                &r[..r_n],
                &t[..t_n],
            ],
        );
        let (p0, p0b) = event(&out, 0);
        assert_eq!(p0.object_id, 14, "popup xdg_surface configure");
        assert_eq!(p0.opcode, protocol::xdg_surface_ev::CONFIGURE);
        let popup_serial = u32::from_le_bytes(p0b[0..4].try_into().unwrap());
        assert!(popup_serial > serial, "serials move forward");
        let (p1, _) = event(&out, 1);
        assert_eq!(p1.object_id, 4, "wm_base ping follows the configure");
        assert_eq!(p1.opcode, protocol::xdg_wm_base_ev::PING);
        assert!(
            s.take_focus_request().is_none(),
            "a popup does not take focus"
        );
    }
}
