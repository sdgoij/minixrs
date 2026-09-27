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
//! `wl_shm` pools/buffers and input events are 1b/1c.

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
            | (Kind::Keyboard, protocol::keyboard_req::RELEASE) => {
                self.remove(object_id);
                Ok(())
            }
            (Kind::Pointer, protocol::pointer_req::SET_CURSOR) => {
                // No cursor surfaces in Phase 1, so nothing to draw; accepted and
                // dropped rather than answered with an error.
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
            (Kind::Surface, protocol::surface_req::ATTACH) => {
                // `buffer` is nullable; an id of 0 detaches.
                let buffer = arg(n, &args, 0)?;
                if let Some(o) = self.obj_mut(object_id) {
                    o.attached = buffer;
                }
                Ok(())
            }
            (Kind::Surface, protocol::surface_req::DAMAGE) => {
                // Accepted and ignored: a Phase 1b commit presents the whole
                // surface.
                Ok(())
            }
            (Kind::Surface, protocol::surface_req::FRAME) => {
                // A frame callback is answered at once with `done`.
                self.create_callback(arg(n, &args, 0)?, out)
            }
            (Kind::Surface, protocol::surface_req::COMMIT) => {
                if let Some(c) = self.commit_of(object_id) {
                    self.pending = Some(c);
                }
                // The first surface to commit is the one input goes to: there is no
                // window management to choose another yet, and a client that has not
                // drawn anything should not be given keys.
                if self.focus != object_id {
                    self.focus = object_id;
                    self.entered = false;
                }
                self.ensure_entered(out)
            }
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
        })
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

    /// The surface input is focused on (0 = none).
    pub fn focus(&self) -> u32 {
        self.focus
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
}
