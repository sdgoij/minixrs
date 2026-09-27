//! The client side of the protocol: an object table, request constructors over
//! the wire `Writer`, and event decoding.
//!
//! Pure — it builds and parses bytes; the caller owns the socket. `/bin/wlclient`
//! and the host tests share it, so the protocol the server answers is exercised
//! by a client that speaks only the published interfaces.

use crate::protocol::{self, Kind};
use crate::wire::{Arg, DispatchBuf, HEADER_LEN, Header, MAX_ARGS, WireError};

/// Most objects one client may hold open.
pub const MAX_OBJECTS: usize = 64;

/// The id `wl_display` always has.
pub const DISPLAY_ID: u32 = 1;

/// A failure on the client side of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientError {
    /// A message did not frame or decode.
    Wire(WireError),
    /// An event named an object this client does not hold.
    UnknownObject(u32),
}

impl From<WireError> for ClientError {
    fn from(e: WireError) -> Self {
        ClientError::Wire(e)
    }
}

impl core::fmt::Display for ClientError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ClientError::Wire(e) => write!(f, "{e}"),
            ClientError::UnknownObject(id) => write!(f, "event for unknown object {id}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Obj {
    id: u32,
    kind: Kind,
}

/// A client's view of one connection: which object ids it holds and of what
/// interface.
///
/// Ids are client-allocated — the protocol's `new_id` argument names the id the
/// client picks — so this table is both the allocator and the index the event
/// decoder uses. Ids increase monotonically; a `wl_display.delete_id` frees an
/// entry with [`Client::free`], after which an event on it is an error.
pub struct Client {
    objs: [Obj; MAX_OBJECTS],
    n: usize,
    next_id: u32,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// A client that holds only `wl_display`.
    pub fn new() -> Self {
        let mut objs = [Obj {
            id: 0,
            kind: Kind::Display,
        }; MAX_OBJECTS];
        objs[0] = Obj {
            id: DISPLAY_ID,
            kind: Kind::Display,
        };
        Self {
            objs,
            n: 1,
            next_id: DISPLAY_ID + 1,
        }
    }

    /// The interface of an object this client holds.
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

    /// Claim the next id for a new object of `kind`.
    pub fn alloc(&mut self, kind: Kind) -> Result<u32, WireError> {
        if self.n >= MAX_OBJECTS {
            return Err(WireError::NoSpace);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.objs[self.n] = Obj { id, kind };
        self.n += 1;
        Ok(id)
    }

    /// Forget an object, on `wl_display.delete_id`. The id is not reused; the
    /// allocator stays monotonic, which is all a Phase 1 client needs.
    pub fn free(&mut self, id: u32) {
        if let Some(i) = self.objs[..self.n].iter().position(|o| o.id == id) {
            for j in i..self.n - 1 {
                self.objs[j] = self.objs[j + 1];
            }
            self.n -= 1;
        }
    }

    /// `wl_display.get_registry`. Returns the registry id.
    pub fn get_registry(&mut self, out: &mut DispatchBuf<'_>) -> Result<u32, WireError> {
        let id = self.alloc(Kind::Registry)?;
        out.event(DISPLAY_ID, protocol::display_req::GET_REGISTRY, |w| {
            w.new_id(id)
        })?;
        Ok(id)
    }

    /// `wl_display.sync`. Returns the callback id.
    pub fn sync(&mut self, out: &mut DispatchBuf<'_>) -> Result<u32, WireError> {
        let id = self.alloc(Kind::Callback)?;
        out.event(DISPLAY_ID, protocol::display_req::SYNC, |w| w.new_id(id))?;
        Ok(id)
    }

    /// `wl_registry.bind`. `kind` is the interface the client expects `interface`
    /// to name; returns the new object's id.
    pub fn bind(
        &mut self,
        registry: u32,
        name: u32,
        interface: &[u8],
        version: u32,
        kind: Kind,
        out: &mut DispatchBuf<'_>,
    ) -> Result<u32, WireError> {
        let id = self.alloc(kind)?;
        out.event(registry, protocol::registry_req::BIND, |w| {
            w.uint(name)?;
            w.string(interface)?;
            w.uint(version)?;
            w.new_id(id)
        })?;
        Ok(id)
    }

    /// `wl_compositor.create_surface`.
    pub fn create_surface(
        &mut self,
        compositor: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<u32, WireError> {
        let id = self.alloc(Kind::Surface)?;
        out.event(compositor, protocol::compositor_req::CREATE_SURFACE, |w| {
            w.new_id(id)
        })?;
        Ok(id)
    }

    /// `wl_compositor.create_region`.
    pub fn create_region(
        &mut self,
        compositor: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<u32, WireError> {
        let id = self.alloc(Kind::Region)?;
        out.event(compositor, protocol::compositor_req::CREATE_REGION, |w| {
            w.new_id(id)
        })?;
        Ok(id)
    }

    /// `wl_seat.get_keyboard`. Returns the keyboard object's id.
    pub fn get_keyboard(&mut self, seat: u32, out: &mut DispatchBuf<'_>) -> Result<u32, WireError> {
        let id = self.alloc(Kind::Keyboard)?;
        out.event(seat, protocol::seat_req::GET_KEYBOARD, |w| w.new_id(id))?;
        Ok(id)
    }

    /// `wl_seat.get_pointer`. Returns the pointer object's id.
    pub fn get_pointer(&mut self, seat: u32, out: &mut DispatchBuf<'_>) -> Result<u32, WireError> {
        let id = self.alloc(Kind::Pointer)?;
        out.event(seat, protocol::seat_req::GET_POINTER, |w| w.new_id(id))?;
        Ok(id)
    }

    /// `wl_shm.create_pool`. `fd_index` refers to a descriptor sent with this
    /// message by `SCM_RIGHTS`, not to a descriptor the caller already holds.
    pub fn create_pool(
        &mut self,
        shm: u32,
        fd_index: u32,
        size: i32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<u32, WireError> {
        let id = self.alloc(Kind::ShmPool)?;
        out.event(shm, protocol::shm_req::CREATE_POOL, |w| {
            w.new_id(id)?;
            w.fd(fd_index)?;
            w.int(size)
        })?;
        Ok(id)
    }

    /// `wl_shm_pool.create_buffer`.
    #[allow(clippy::too_many_arguments)]
    pub fn create_buffer(
        &mut self,
        pool: u32,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<u32, WireError> {
        let id = self.alloc(Kind::Buffer)?;
        out.event(pool, protocol::shm_pool_req::CREATE_BUFFER, |w| {
            w.new_id(id)?;
            w.int(offset)?;
            w.int(width)?;
            w.int(height)?;
            w.int(stride)?;
            w.uint(format)
        })?;
        Ok(id)
    }

    /// `wl_surface.attach`. `buffer` of 0 detaches.
    pub fn attach(
        &self,
        surface: u32,
        buffer: u32,
        x: i32,
        y: i32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), WireError> {
        out.event(surface, protocol::surface_req::ATTACH, |w| {
            w.object(buffer)?;
            w.int(x)?;
            w.int(y)
        })
    }

    /// `wl_surface.damage`.
    pub fn damage(
        &self,
        surface: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), WireError> {
        out.event(surface, protocol::surface_req::DAMAGE, |w| {
            w.int(x)?;
            w.int(y)?;
            w.int(width)?;
            w.int(height)
        })
    }

    /// `wl_surface.frame`. Returns the callback id that will be answered `done`.
    pub fn frame(&mut self, surface: u32, out: &mut DispatchBuf<'_>) -> Result<u32, WireError> {
        let id = self.alloc(Kind::Callback)?;
        out.event(surface, protocol::surface_req::FRAME, |w| w.new_id(id))?;
        Ok(id)
    }

    /// `wl_surface.commit`.
    pub fn commit(&self, surface: u32, out: &mut DispatchBuf<'_>) -> Result<(), WireError> {
        out.event(surface, protocol::surface_req::COMMIT, |_w| Ok(()))
    }

    /// `wl_surface.destroy`. The id is freed when the server's
    /// `wl_display.delete_id` arrives, not here.
    pub fn destroy_surface(
        &self,
        surface: u32,
        out: &mut DispatchBuf<'_>,
    ) -> Result<(), WireError> {
        out.event(surface, protocol::surface_req::DESTROY, |_w| Ok(()))
    }

    /// `wl_buffer.destroy`.
    pub fn destroy_buffer(&self, buffer: u32, out: &mut DispatchBuf<'_>) -> Result<(), WireError> {
        out.event(buffer, protocol::buffer_req::DESTROY, |_w| Ok(()))
    }

    /// `wl_shm_pool.destroy`.
    pub fn destroy_pool(&self, pool: u32, out: &mut DispatchBuf<'_>) -> Result<(), WireError> {
        out.event(pool, protocol::shm_pool_req::DESTROY, |_w| Ok(()))
    }

    /// Decode one event message (header and body). The object's interface drives
    /// the signature; the arguments land in `args` and their count is returned.
    pub fn decode<'a>(
        &self,
        msg: &'a [u8],
        args: &mut [Arg<'a>; MAX_ARGS],
    ) -> Result<(Header, Kind, usize), ClientError> {
        let header = Header::parse(msg)?;
        if msg.len() < header.size as usize {
            return Err(WireError::Truncated.into());
        }
        let kind = self
            .kind(header.object_id)
            .ok_or(ClientError::UnknownObject(header.object_id))?;
        let body = &msg[HEADER_LEN..header.size as usize];
        let n = protocol::decode_event(kind.interface(), header.opcode, body, args)?;
        Ok((header, kind, n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::Server;
    use crate::wire::{MessageBuffer, Writer};

    /// One request, built by the client and held until it is sent.
    struct Wire {
        req: [u8; 512],
        len: usize,
    }

    impl Wire {
        fn new() -> Self {
            Self {
                req: [0u8; 512],
                len: 0,
            }
        }

        /// Build one request with `f`, returning what it produced.
        fn build<R>(&mut self, f: impl FnOnce(&mut DispatchBuf<'_>) -> Result<R, WireError>) -> R {
            let mut q = DispatchBuf::new(&mut self.req);
            let r = f(&mut q).unwrap();
            self.len = q.len();
            r
        }

        fn bytes(&self) -> &[u8] {
            &self.req[..self.len]
        }
    }

    /// Dispatch every message in `req` to `server`, collecting the events.
    fn to_server(server: &mut Server, req: &[u8], events: &mut [u8]) -> usize {
        let mut mb = MessageBuffer::new();
        mb.push(req).unwrap();
        let mut total = 0usize;
        while let Some(msg) = mb.next().unwrap() {
            let h = Header::parse(msg).unwrap();
            let mut b = DispatchBuf::new(&mut events[total..]);
            server
                .dispatch(h.object_id, h.opcode, &msg[HEADER_LEN..], &mut b)
                .unwrap();
            total += b.len();
            mb.consume(h.size as usize);
        }
        total
    }

    #[test]
    fn get_registry_allocates_id_two_and_frames_the_request() {
        let mut c = Client::new();
        let mut w = Wire::new();
        let id = w.build(|o| c.get_registry(o));
        assert_eq!(id, 2);
        assert_eq!(c.kind(2), Some(Kind::Registry));
        // Header: object 1, opcode 1, size 12; body: new_id 2.
        assert_eq!(&w.bytes()[0..4], &DISPLAY_ID.to_le_bytes());
        assert_eq!(&w.bytes()[4..8], &((12u32 << 16) | 1).to_le_bytes());
        assert_eq!(&w.bytes()[8..12], &2u32.to_le_bytes());
    }

    #[test]
    fn bind_writes_the_registry_signature() {
        let mut c = Client::new();
        c.alloc(Kind::Registry).unwrap();
        let mut w = Wire::new();
        let id = w.build(|o| c.bind(2, 7, b"wl_shm", 1, Kind::Shm, o));
        assert_eq!(id, 3);
        assert_eq!(c.kind(3), Some(Kind::Shm));
        // object 2, opcode 0, then name, name-length, "wl_shm", version, new id.
        assert_eq!(&w.bytes()[0..4], &2u32.to_le_bytes());
        assert_eq!(&w.bytes()[4..8], &(32u32 << 16).to_le_bytes());
        assert_eq!(&w.bytes()[8..12], &7u32.to_le_bytes());
        assert_eq!(u32::from_le_bytes(w.bytes()[12..16].try_into().unwrap()), 7);
        assert_eq!(&w.bytes()[16..22], b"wl_shm");
        assert_eq!(&w.bytes()[24..28], &1u32.to_le_bytes());
        assert_eq!(&w.bytes()[28..32], &3u32.to_le_bytes());
    }

    #[test]
    fn decoding_an_event_for_an_unknown_object_fails() {
        let c = Client::new();
        // An event addressed to object 99, which this client never held.
        let mut buf = [0u8; 12];
        {
            let mut w = Writer::new(&mut buf).unwrap();
            w.uint(1).unwrap();
            w.finish(99, protocol::display_ev::DELETE_ID).unwrap();
        }
        let mut args = [Arg::Uint(0); MAX_ARGS];
        assert_eq!(
            c.decode(&buf, &mut args),
            Err(ClientError::UnknownObject(99))
        );
    }

    #[test]
    fn decode_reads_a_registry_global() {
        let mut c = Client::new();
        c.alloc(Kind::Registry).unwrap();
        // wl_registry.global on id 2: name 5, "wl_shm", version 1.
        let mut buf = [0u8; 32];
        let size = {
            let mut w = Writer::new(&mut buf).unwrap();
            w.uint(5).unwrap();
            w.string(b"wl_shm").unwrap();
            w.uint(1).unwrap();
            w.finish(2, protocol::registry_ev::GLOBAL).unwrap().len()
        };
        let mut args = [Arg::Uint(0); MAX_ARGS];
        let (h, kind, n) = c.decode(&buf[..size], &mut args).unwrap();
        assert_eq!(h.object_id, 2);
        assert_eq!(kind, Kind::Registry);
        assert_eq!(n, 3);
        assert_eq!(args[0].as_uint(), Some(5));
        assert_eq!(args[1].as_str(), Some(&b"wl_shm"[..]));
        assert_eq!(args[2].as_uint(), Some(1));
    }

    #[test]
    fn free_forgets_an_object() {
        let mut c = Client::new();
        let id = c.alloc(Kind::Callback).unwrap();
        assert_eq!(c.kind(id), Some(Kind::Callback));
        c.free(id);
        assert_eq!(c.kind(id), None);
        assert_eq!(c.object_count(), 1);
    }

    #[test]
    fn a_full_shm_frame_round_trips_through_both_halves() {
        let mut server = Server::new(64, 64);
        let mut client = Client::new();
        let mut w = Wire::new();
        let mut events = [0u8; 8192];

        // The registry handshake, and the globals a client learns.
        let registry = w.build(|o| client.get_registry(o));
        let en = to_server(&mut server, w.bytes(), &mut events);
        let (shm_name, comp_name) = {
            let mut mb = MessageBuffer::new();
            mb.push(&events[..en]).unwrap();
            let mut shm = None;
            let mut comp = None;
            while let Some(msg) = mb.next().unwrap() {
                let h = Header::parse(msg).unwrap();
                let mut args = [Arg::Uint(0); MAX_ARGS];
                let (_, kind, argc) = client.decode(msg, &mut args).unwrap();
                assert_eq!(kind, Kind::Registry);
                assert_eq!(h.opcode, protocol::registry_ev::GLOBAL);
                assert_eq!(argc, 3);
                let name = args[0].as_uint().unwrap();
                let interface = args[1].as_str().unwrap();
                if interface == b"wl_shm" {
                    shm = Some(name);
                }
                if interface == b"wl_compositor" {
                    comp = Some(name);
                }
                mb.consume(h.size as usize);
            }
            (shm.unwrap(), comp.unwrap())
        };

        // Bind the two factories.
        let compositor = w.build(|o| {
            client.bind(
                registry,
                comp_name,
                b"wl_compositor",
                1,
                Kind::Compositor,
                o,
            )
        });
        to_server(&mut server, w.bytes(), &mut events);
        assert_eq!(server.kind(compositor), Some(Kind::Compositor));
        let shm = w.build(|o| client.bind(registry, shm_name, b"wl_shm", 1, Kind::Shm, o));
        to_server(&mut server, w.bytes(), &mut events);
        assert_eq!(server.kind(shm), Some(Kind::Shm));

        // A surface and a pool.
        let surface = w.build(|o| client.create_surface(compositor, o));
        to_server(&mut server, w.bytes(), &mut events);
        let pool = w.build(|o| client.create_pool(shm, 0, 16384, o));
        to_server(&mut server, w.bytes(), &mut events);
        let buffer = w.build(|o| {
            client.create_buffer(pool, 0, 64, 64, 256, protocol::WL_SHM_FORMAT_ARGB8888, o)
        });
        to_server(&mut server, w.bytes(), &mut events);

        // Present: attach, damage, commit.
        w.build(|o| client.attach(surface, buffer, 0, 0, o));
        to_server(&mut server, w.bytes(), &mut events);
        w.build(|o| client.damage(surface, 0, 0, 64, 64, o));
        to_server(&mut server, w.bytes(), &mut events);
        w.build(|o| client.commit(surface, o));
        to_server(&mut server, w.bytes(), &mut events);

        let commit = server
            .take_commit()
            .expect("the surface committed a buffer");
        assert_eq!(commit.surface, surface);
        assert_eq!(commit.buffer, buffer);
        assert_eq!(commit.width, 64);
        assert_eq!(commit.height, 64);
        assert_eq!(commit.stride, 256);

        // The server releases it; the client understands the event.
        {
            let mut q = DispatchBuf::new(&mut events);
            server.release_buffer(buffer, &mut q).unwrap();
            let mut mb = MessageBuffer::new();
            mb.push(q.bytes()).unwrap();
            let msg = mb.next().unwrap().unwrap();
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let (h, kind, argc) = client.decode(msg, &mut args).unwrap();
            assert_eq!(kind, Kind::Buffer);
            assert_eq!(h.opcode, protocol::buffer_ev::RELEASE);
            assert_eq!(argc, 0);
        }
    }

    #[test]
    fn frame_allocates_a_callback_the_server_completes() {
        let mut server = Server::new(64, 64);
        let mut client = Client::new();
        let mut w = Wire::new();
        let mut events = [0u8; 4096];

        // A surface is needed first; build the handshake the quick way.
        let registry = w.build(|o| client.get_registry(o));
        to_server(&mut server, w.bytes(), &mut events);
        let compositor =
            w.build(|o| client.bind(registry, 1, b"wl_compositor", 1, Kind::Compositor, o));
        to_server(&mut server, w.bytes(), &mut events);
        let surface = w.build(|o| client.create_surface(compositor, o));
        to_server(&mut server, w.bytes(), &mut events);

        let callback = w.build(|o| client.frame(surface, o));
        assert_eq!(client.kind(callback), Some(Kind::Callback));
        let en = to_server(&mut server, w.bytes(), &mut events);

        // wl_callback.done then wl_display.delete_id.
        let mut mb = MessageBuffer::new();
        mb.push(&events[..en]).unwrap();
        let msg = mb.next().unwrap().unwrap();
        let mut args = [Arg::Uint(0); MAX_ARGS];
        let (h, kind, _) = client.decode(msg, &mut args).unwrap();
        assert_eq!(kind, Kind::Callback);
        assert_eq!(h.object_id, callback);
        assert_eq!(h.opcode, protocol::callback_ev::DONE);
        mb.consume(h.size as usize);

        let msg = mb.next().unwrap().unwrap();
        let mut args = [Arg::Uint(0); MAX_ARGS];
        let (h, kind, _) = client.decode(msg, &mut args).unwrap();
        assert_eq!(kind, Kind::Display);
        assert_eq!(h.opcode, protocol::display_ev::DELETE_ID);
        assert_eq!(args[0].as_uint(), Some(callback));
        mb.consume(h.size as usize);
        assert!(mb.next().unwrap().is_none());

        // The client frees the spent callback.
        client.free(callback);
        assert_eq!(client.kind(callback), None);
    }
}
