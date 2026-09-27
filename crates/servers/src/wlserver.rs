//! Wayland server (Phase 1) — boot proc 20, `/sbin/wlserver`.
//!
//! Speaks the core protocol plus `wl_shm` over a `/dev/uds` socket, and presents
//! a committed buffer to `/dev/fb`. The protocol itself is `crates/wayland`
//! (pure, host-tested); this is the loop around it: accept clients, poll them all
//! together, decode their requests, answer them, and composite a commit onto the
//! display. It arbitrates a single focus between them, so input goes to the surface
//! that last mapped a window (2c). It also serves
//! the seat's keymap — [`KEYMAP_XKB`] into a `memfd`, handed to a client's keyboard
//! by `SCM_RIGHTS` (`WAYLAND.md` §6.12).
//!
//! It maps the framebuffer directly and sends `FBIOFLUSH` **to the fb server**,
//! not through VFS, the way `wserver` does — VFS is single-worker and the shell's
//! blocking console read holds it, so a device ioctl through VFS would wait for a
//! keystroke (`WAYLAND.md` §6.11).

/// The US/evdev XKB keymap `wl_keyboard.keymap` serves (`WAYLAND.md` §6.12).
///
/// Compiled in rather than read from a boot data file: a boot process's read of a
/// regular file goes through VFS to MFS, and by the time `wlserver` runs the shell
/// is parked in a console read that holds VFS's single worker, so the read never
/// completes. The bytes are the `xkbcli` output the doc names, and `.gitattributes`
/// keeps them from being EOL-converted.
pub static KEYMAP_XKB: &[u8] = include_bytes!("../data/us-evdev.xkb");

#[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
mod imp {
    use arch_common::ipc::Message;
    use drivers::input::constants::{
        INPUT_BUTTON_1, INPUT_GD_X, INPUT_GD_Y, INPUT_PAGE_ABS, INPUT_PAGE_BUTTON, INPUT_PAGE_GD,
        INPUT_PAGE_KEY,
    };
    use minix_std::fs::{self, PollFd};
    use minix_std::time::{self, CLOCK_MONOTONIC};
    use minix_std::uds;
    use minix_std::vmem;
    use wayland::input;
    use wayland::server::{Commit, CursorImage, Server};
    use wayland::shm::{self, Destination};
    use wayland::wire::{DispatchBuf, HEADER_LEN, Header, MessageBuffer};

    /// The socket a Wayland client connects to.
    const SOCK_PATH: &[u8] = b"/run/wayland-0";

    /// Screen geometry: every fb backend this port has adopts 1024x768 XRGB8888
    /// (bochs's default mode, `virtio-gpu`'s 2D resource, the canvas), so a blit
    /// can be clipped against constants rather than a mode query.
    const XRES: usize = 1024;
    const YRES: usize = 768;
    const PITCH: usize = XRES * 4;
    /// The device BAR is larger than the mode's frame, as in `wserver`.
    const MAP_LEN: usize = 4 * 1024 * 1024;

    /// Most `wl_shm` pool fds one client may hold open.
    const MAX_POOLS: usize = 16;

    const POLLIN: i16 = 0x001;

    /// How long the serve loop waits on the socket before checking the input
    /// server.
    ///
    /// Input arrives by IPC notification, and an endpoint is not an fd, so `poll`
    /// cannot watch it: the loop wakes on a tick and fetches instead. 20 ms is a
    /// frame's worth of latency, and the fetch is a single SENDREC that returns
    /// EAGAIN when nothing is queued.
    const TICK_MS: i32 = 20;

    fn report(msg: &[u8]) {
        unsafe { minix_rt::write(2, msg.as_ptr(), msg.len()) };
    }

    /// The fds of the pools a client created, by pool object id.
    struct Pools {
        ids: [u32; MAX_POOLS],
        fds: [i32; MAX_POOLS],
        n: usize,
    }

    impl Pools {
        fn new() -> Self {
            Self {
                ids: [0; MAX_POOLS],
                fds: [-1; MAX_POOLS],
                n: 0,
            }
        }

        fn add(&mut self, id: u32, fd: i32) {
            if self.n < MAX_POOLS {
                self.ids[self.n] = id;
                self.fds[self.n] = fd;
                self.n += 1;
            }
        }

        fn fd_of(&self, id: u32) -> Option<i32> {
            self.ids[..self.n]
                .iter()
                .position(|&p| p == id)
                .map(|i| self.fds[i])
        }

        /// Close every pool fd. Called when a client's connection ends, so a
        /// client that goes away does not leak its pools.
        fn close_all(&mut self) {
            for i in 0..self.n {
                let _ = uds::close_fd(self.fds[i]);
            }
            self.n = 0;
        }
    }

    /// Copy the keymap into a `memfd`, NUL-terminated, and return the fd with the
    /// size `wl_keyboard.keymap` must advertise.
    ///
    /// The bytes are compiled in ([`super::KEYMAP_XKB`]) rather than read from a boot
    /// data file, so nothing here depends on MFS. The caller prepares it only once a
    /// client is waiting, never at boot (see `serve_forever`) — that timing is what
    /// keeps VFS's single worker free. The fd is what the event hands a client by
    /// `SCM_RIGHTS`; a failure answers `None`, and the seat is still served, just
    /// without a keymap.
    fn load_keymap() -> Option<(i32, u32)> {
        let bytes = super::KEYMAP_XKB;
        // The keymap string is NUL-terminated and `size` counts the NUL.
        let size = bytes.len() + 1;
        let fd = match fs::memfd_create(0) {
            Ok(fd) => fd,
            Err(_) => {
                report(b"wlserver: keymap memfd failed\n");
                return None;
            }
        };
        if fs::truncate(fd, size as i64).is_err() {
            report(b"wlserver: keymap ftruncate failed\n");
            let _ = fs::close(fd);
            return None;
        }
        // Copy through a shared mapping, the way the pool path does. `write(2)` on a
        // fresh memfd does not advance a file offset here, so a chunked write would
        // overwrite from the start each time and only the last chunk would survive.
        let dst = unsafe {
            vmem::mmap(
                core::ptr::null_mut(),
                size,
                vmem::PROT_READ | vmem::PROT_WRITE,
                vmem::MAP_SHARED,
                fd,
                0,
            )
        };
        if dst == vmem::MAP_FAILED {
            report(b"wlserver: keymap mmap failed\n");
            let _ = fs::close(fd);
            return None;
        }
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len()) };
        let _ = unsafe { vmem::munmap(dst, size) };
        Some((fd, size as u32))
    }

    /// Open and map `/dev/fb`, or report why not.
    fn attach_display() -> Option<*mut u8> {
        let fd = match unsafe { fs::open(b"/dev/fb", fs::O_RDWR, 0) } {
            Ok(fd) => fd,
            Err(_) => {
                report(b"wlserver: open /dev/fb failed\n");
                return None;
            }
        };
        let fb = unsafe {
            vmem::mmap(
                core::ptr::null_mut(),
                MAP_LEN,
                vmem::PROT_READ | vmem::PROT_WRITE,
                vmem::MAP_SHARED,
                fd,
                0,
            )
        };
        if fb == vmem::MAP_FAILED {
            report(b"wlserver: mmap /dev/fb failed\n");
            return None;
        }
        Some(fb)
    }

    /// Present the frame. The ioctl goes to the fb server directly (see the
    /// module doc); `FBIOFLUSH` carries no arg struct, so it travels with no
    /// grant.
    fn fb_flush() {
        let mut msg = Message {
            m_source: 0,
            m_type: arch_common::com::CDEV_IOCTL as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        msg.m_payload.m2.m2i1 = 0; // minor
        msg.m_payload.m2.m2i2 = fs::FBIOFLUSH as i32; // request
        let _ = unsafe {
            minix_rt::syscall2(
                minix_rt::SENDREC_CALL,
                arch_common::com::FB_PROC_NR as u64,
                &mut msg as *mut Message as u64,
            )
        };
    }

    /// Composite a commit onto the display and present it.
    ///
    /// Only the commit's damage is recomposited (2d): a client that names a small
    /// rectangle pays for that rectangle, and one that names none gets its whole
    /// buffer, so it is never left stale. The pointer's cursor image, if a client set
    /// one, is then drawn over the frame.
    ///
    /// Each pool is mapped read-only for as long as its copy takes: the client owns
    /// those frames, and this process only reads them.
    fn present(
        fb: *mut u8,
        pools: &Pools,
        c: &Commit,
        ptr: (i32, i32),
        cursor: Option<CursorImage>,
    ) {
        let Some(fd) = pools.fd_of(c.pool) else {
            report(b"wlserver: commit for a pool with no fd\n");
            return;
        };
        let len = c.pool_size.max(0) as usize;
        if len == 0 {
            return;
        }
        let src = unsafe {
            vmem::mmap(
                core::ptr::null_mut(),
                len,
                vmem::PROT_READ,
                vmem::MAP_SHARED,
                fd,
                0,
            )
        };
        if src == vmem::MAP_FAILED {
            report(b"wlserver: mmap of a pool failed\n");
            return;
        }
        let bytes = unsafe { core::slice::from_raw_parts(src, len) };
        let full = c.buffer();
        let mut rejected = false;
        match c.damage {
            // No damage named: take the whole buffer rather than risk staleness.
            None => {
                let dst = Destination {
                    x: 0,
                    y: 0,
                    width: XRES as i32,
                    height: YRES as i32,
                };
                if shm::blit(bytes, &full, dst, |x, y, px| put_px(fb, x, y, px)).is_err() {
                    rejected = true;
                }
            }
            // Just the part of it the client marked.
            Some(dmg) => {
                if let Some((part, x, y)) = full.damage_slice(dmg) {
                    let dst = Destination {
                        x,
                        y,
                        width: XRES as i32,
                        height: YRES as i32,
                    };
                    if shm::blit(bytes, &part, dst, |x, y, px| put_px(fb, x, y, px)).is_err() {
                        rejected = true;
                    }
                }
            }
        }
        if rejected {
            report(b"wlserver: blit rejected a buffer\n");
        }
        let _ = unsafe { vmem::munmap(src, len) };

        if let Some(cur) = cursor {
            blit_cursor(fb, pools, &cur, ptr);
        }
        fb_flush();
    }

    /// Write one pixel into the framebuffer.
    fn put_px(fb: *mut u8, x: i32, y: i32, px: u32) {
        let off = y as usize * PITCH + x as usize * 4;
        unsafe { core::ptr::write_volatile(fb.add(off).cast::<u32>(), px) };
    }

    /// Draw the pointer's cursor image at `ptr`, offset by its hotspot.
    ///
    /// A fully transparent pixel leaves what is under it alone, which is what a
    /// shaped cursor needs; a partly transparent one is copied as it stands (no
    /// blending — 2d).
    fn blit_cursor(fb: *mut u8, pools: &Pools, cur: &CursorImage, ptr: (i32, i32)) {
        let Some(fd) = pools.fd_of(cur.pool) else {
            return;
        };
        let len = cur.buffer.pool_size.max(0) as usize;
        if len == 0 {
            return;
        }
        let src = unsafe {
            vmem::mmap(
                core::ptr::null_mut(),
                len,
                vmem::PROT_READ,
                vmem::MAP_SHARED,
                fd,
                0,
            )
        };
        if src == vmem::MAP_FAILED {
            report(b"wlserver: mmap of a cursor pool failed\n");
            return;
        }
        let bytes = unsafe { core::slice::from_raw_parts(src, len) };
        let dst = Destination {
            x: ptr.0 - cur.hotspot_x,
            y: ptr.1 - cur.hotspot_y,
            width: XRES as i32,
            height: YRES as i32,
        };
        let skip_clear = cur.buffer.format == wayland::protocol::WL_SHM_FORMAT_ARGB8888;
        let _ = shm::blit(bytes, &cur.buffer, dst, |x, y, px| {
            if skip_clear && px >> 24 == 0 {
                return;
            }
            put_px(fb, x, y, px);
        });
        let _ = unsafe { vmem::munmap(src, len) };
    }

    /// The pointer and modifier state carried between input batches.
    struct InputState {
        mods: input::Modifiers,
        /// The pointer position in surface pixels, kept so a relative move has an
        /// absolute place to land.
        ptr: (i32, i32),
    }

    impl InputState {
        fn new() -> Self {
            Self {
                mods: input::Modifiers::default(),
                ptr: (XRES as i32 / 2, YRES as i32 / 2),
            }
        }
    }

    /// Monotonic milliseconds, for the `time` field input events carry.
    ///
    /// Read once per batch rather than per event: a client uses it for timing, not
    /// for identity, and one clock fetch per drain is enough. A failure answers 0
    /// rather than failing the input path.
    fn now_ms() -> u32 {
        match time::clock_gettime(CLOCK_MONOTONIC) {
            Ok(ts) => (ts.tv_sec as u32)
                .wrapping_mul(1000)
                .wrapping_add((ts.tv_nsec / 1_000_000) as u32),
            Err(_) => 0,
        }
    }

    /// Fetch one batch of HID records from the input server, or `None` when none
    /// are pending.
    ///
    /// A direct `CDEV_READ`, exactly as `wserver` does: VFS is single-worker and the
    /// shell's console read holds it, so a read through VFS would wait for a
    /// keystroke. The input server keeps a cursor per reader, so this advances only
    /// wlserver's.
    fn fetch_input(batch: &mut [u8; 48]) -> Option<usize> {
        let mut msg = Message {
            m_source: 0,
            m_type: arch_common::com::CDEV_READ as i32,
            m_payload: unsafe { core::mem::zeroed() },
        };
        msg.m_payload.m2.m2i1 = 0; // minor
        msg.m_payload.m2.m2l2 = 16; // count: two records is the inline reply's worth
        let _ = unsafe {
            minix_rt::syscall2(
                minix_rt::SENDREC_CALL,
                arch_common::com::INPUT_PROC_NR as u64,
                &mut msg as *mut Message as u64,
            )
        };
        // The reply status is the byte count in `m_type`, or EAGAIN (negative).
        let n = msg.m_type;
        if !(8..=48).contains(&n) {
            return None;
        }
        let n = n as usize;
        unsafe { batch[..n].copy_from_slice(&msg.m_payload.raw[..n]) };
        Some(n)
    }

    /// Turn every queued HID record into Wayland input events.
    ///
    /// Stops early if the reply buffer fills — the caller sends what there is and
    /// the rest arrives on the next tick, which is a dropped frame of input rather
    /// than a dropped connection.
    fn drain_input(server: &mut Server, st: &mut InputState, reply: &mut DispatchBuf<'_>) {
        let mut batch = [0u8; 48];
        while let Some(n) = fetch_input(&mut batch) {
            let time = now_ms();
            let mut off = 0;
            while off + 8 <= n {
                let page = u16::from_le_bytes([batch[off], batch[off + 1]]);
                let code = u16::from_le_bytes([batch[off + 2], batch[off + 3]]);
                let value = i32::from_le_bytes([
                    batch[off + 4],
                    batch[off + 5],
                    batch[off + 6],
                    batch[off + 7],
                ]);
                off += 8;
                let ok = match page {
                    INPUT_PAGE_KEY => {
                        let pressed = value != 0;
                        if st.mods.apply(code, pressed)
                            && server
                                .send_modifiers(st.mods.depressed(), 0, st.mods.locked(), 0, reply)
                                .is_err()
                        {
                            return;
                        }
                        match input::hid_to_evdev(code) {
                            Some(key) => {
                                let state = if pressed {
                                    input::STATE_PRESSED
                                } else {
                                    input::STATE_RELEASED
                                };
                                server.send_key(time, key as u32, state, reply).is_ok()
                            }
                            None => true,
                        }
                    }
                    INPUT_PAGE_ABS => {
                        // QEMU normalises an absolute tablet to 0..0x7FFF.
                        let v = value.clamp(0, 0x7FFF) as i64;
                        if code == INPUT_GD_X {
                            st.ptr.0 = (v * XRES as i64 / 0x8000) as i32;
                        } else if code == INPUT_GD_Y {
                            st.ptr.1 = (v * YRES as i64 / 0x8000) as i32;
                        } else {
                            continue;
                        }
                        motion(server, st, time, reply)
                    }
                    INPUT_PAGE_GD => {
                        // Relative movement (a PS/2 mouse): accumulate, then clamp.
                        if code == INPUT_GD_X {
                            st.ptr.0 += value;
                        } else if code == INPUT_GD_Y {
                            st.ptr.1 += value;
                        } else {
                            continue;
                        }
                        st.ptr.0 = st.ptr.0.clamp(0, XRES as i32 - 1);
                        st.ptr.1 = st.ptr.1.clamp(0, YRES as i32 - 1);
                        motion(server, st, time, reply)
                    }
                    INPUT_PAGE_BUTTON => {
                        let button = if code == INPUT_BUTTON_1 {
                            input::BTN_LEFT
                        } else if code == INPUT_BUTTON_1 + 1 {
                            input::BTN_RIGHT
                        } else if code == INPUT_BUTTON_1 + 2 {
                            input::BTN_MIDDLE
                        } else {
                            continue;
                        };
                        let state = if value != 0 {
                            input::STATE_PRESSED
                        } else {
                            input::STATE_RELEASED
                        };
                        server
                            .send_pointer_button(time, button, state, reply)
                            .is_ok()
                    }
                    _ => true,
                };
                if !ok {
                    return;
                }
            }
        }
    }

    /// Emit a pointer move at the tracked position.
    fn motion(
        server: &mut Server,
        st: &InputState,
        time: u32,
        reply: &mut DispatchBuf<'_>,
    ) -> bool {
        // 24.8 fixed point, which is what `wl_pointer.motion` carries.
        server
            .send_pointer_motion(time, st.ptr.0 << 8, st.ptr.1 << 8, reply)
            .is_ok()
    }

    /// Most clients served at once.
    const MAX_CLIENTS: usize = 8;

    /// One connection's state, so several can be served in a single loop.
    struct Client {
        conn: i32,
        server: Server,
        pools: Pools,
        inbuf: MessageBuffer,
        chunk: [u8; 512],
        out: [u8; 8192],
    }

    impl Client {
        fn new(conn: i32, keymap_size: u32) -> Self {
            let mut server = Server::new(XRES as i32, YRES as i32);
            server.set_keymap(keymap_size);
            Self {
                conn,
                server,
                pools: Pools::new(),
                inbuf: MessageBuffer::new(),
                chunk: [0u8; 512],
                out: [0u8; 8192],
            }
        }
    }

    /// Send a built reply, if there is one.
    fn flush(conn: i32, reply: &DispatchBuf<'_>) {
        if !reply.is_empty() {
            let _ = uds::send(conn, reply.bytes());
        }
    }

    /// Read and answer what one client has sent. `false` means the connection ended
    /// and the caller should drop it.
    fn pump(c: &mut Client, fb: *mut u8, keymap: Option<(i32, u32)>, ptr: (i32, i32)) -> bool {
        let n = match uds::recv(c.conn, &mut c.chunk) {
            Ok(0) => return false,
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        if c.inbuf.push(&c.chunk[..n]).is_err() {
            return false;
        }
        let mut reply = DispatchBuf::new(&mut c.out);
        let mut ok = true;
        loop {
            let msg = match c.inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => {
                    ok = false;
                    break;
                }
            };
            let h = match Header::parse(msg) {
                Ok(h) => h,
                Err(_) => {
                    ok = false;
                    break;
                }
            };
            let outcome =
                match c
                    .server
                    .dispatch(h.object_id, h.opcode, &msg[HEADER_LEN..], &mut reply)
                {
                    Ok(o) => o,
                    Err(_) => {
                        ok = false;
                        break;
                    }
                };
            // A `wl_shm.create_pool` named an fd index; the descriptor itself
            // travelled with this message by `SCM_RIGHTS`.
            if let Some(pool) = outcome.new_pool {
                let mut fds = [-1i32; 1];
                if let Ok(1) = uds::recv_fds(c.conn, &mut fds) {
                    c.pools.add(pool, fds[0]);
                }
            }
            if let Some(commit) = c.server.take_commit() {
                let cursor = c.server.cursor();
                present(fb, &c.pools, &commit, ptr, cursor);
                let _ = c.server.release_buffer(commit.buffer, &mut reply);
            }
            c.inbuf.consume(h.size as usize);
        }
        if !reply.is_empty() {
            // A keymap event's fd travels by `SCM_RIGHTS`, and the control message
            // must precede the data write it belongs to.
            let owes = c.server.take_keymap();
            if let Some(fd) = keymap.map(|(fd, _)| fd).filter(|_| owes) {
                let _ = uds::send_fds(c.conn, &[fd]);
            }
            let _ = uds::send(c.conn, reply.bytes());
        }
        ok
    }

    /// Bind and listen on the client socket.
    fn bind_listener() -> Option<i32> {
        let fd = match uds::socket() {
            Ok(fd) => fd,
            Err(_) => {
                report(b"wlserver: socket failed\n");
                return None;
            }
        };
        if uds::bind(fd, SOCK_PATH).is_err() {
            report(b"wlserver: bind failed\n");
            let _ = uds::close_fd(fd);
            return None;
        }
        if uds::listen(fd, 4).is_err() {
            report(b"wlserver: listen failed\n");
            let _ = uds::close_fd(fd);
            return None;
        }
        Some(fd)
    }

    fn serve_forever(listen: i32, fb: *mut u8) -> ! {
        // The keymap is prepared on the first client, not at boot. A boot process's
        // VFS call queues behind the shell's console read — parked in a `SENDREC` to
        // the tty, VFS's single worker will not look at another request until a
        // keystroke arrives — so the keymap's memfd would wait forever for input the
        // probe has not yet sent. Here the shell is blocked in `wait`, so VFS is free.
        let mut keymap: Option<(i32, u32)> = None;
        let mut clients: [Option<Client>; MAX_CLIENTS] = core::array::from_fn(|_| None);
        let mut istate = InputState::new();
        // Which connection's surface input goes to. Between connections this is the
        // loop's to arbitrate; within one, a surface's first commit sets it.
        let mut focus: Option<(usize, u32)> = None;
        loop {
            // Poll the listener and every live client at once, so one client that is
            // slow or idle does not stop the others (2c).
            let mut slots = [0usize; MAX_CLIENTS + 1];
            let mut pfds = [PollFd {
                fd: -1,
                events: POLLIN,
                revents: 0,
            }; MAX_CLIENTS + 1];
            pfds[0] = PollFd {
                fd: listen,
                events: POLLIN,
                revents: 0,
            };
            let mut np = 1;
            for (slot, c) in clients.iter().enumerate() {
                if let Some(c) = c {
                    slots[np] = slot;
                    pfds[np] = PollFd {
                        fd: c.conn,
                        events: POLLIN,
                        revents: 0,
                    };
                    np += 1;
                }
            }
            if fs::poll(&mut pfds[..np], TICK_MS).is_err() {
                continue;
            }

            if pfds[0].revents & POLLIN != 0
                && let Ok(conn) = uds::accept(listen)
            {
                if keymap.is_none() {
                    keymap = load_keymap();
                }
                let size = keymap.map_or(0, |(_, s)| s);
                match clients.iter().position(|c| c.is_none()) {
                    Some(slot) => clients[slot] = Some(Client::new(conn, size)),
                    // No room: refuse rather than leak the connection.
                    None => {
                        let _ = uds::close_fd(conn);
                    }
                }
            }

            for i in 1..np {
                if pfds[i].revents & POLLIN == 0 {
                    continue;
                }
                let slot = slots[i];
                let mut ended = false;
                if let Some(c) = clients[slot].as_mut()
                    && !pump(c, fb, keymap, istate.ptr)
                {
                    c.pools.close_all();
                    let _ = uds::close_fd(c.conn);
                    ended = true;
                }
                if ended {
                    if focus.is_some_and(|(s, _)| s == slot) {
                        focus = None;
                    }
                    clients[slot] = None;
                }
            }

            // A surface's first commit asks for focus. Hand it over — saying `leave`
            // to whichever connection held it — before input is routed.
            let mut want: Option<(usize, u32)> = None;
            for slot in 0..MAX_CLIENTS {
                if let Some(c) = clients[slot].as_mut()
                    && let Some(surface) = c.server.take_focus_request()
                {
                    want = Some((slot, surface));
                }
            }
            if let Some((slot, surface)) = want {
                if let Some(old) = focus.map(|(s, _)| s).filter(|s| *s != slot)
                    && let Some(c) = clients[old].as_mut()
                {
                    let mut reply = DispatchBuf::new(&mut c.out);
                    let _ = c.server.set_focus(0, &mut reply);
                    flush(c.conn, &reply);
                }
                if let Some(c) = clients[slot].as_mut() {
                    let mut reply = DispatchBuf::new(&mut c.out);
                    let _ = c.server.set_focus(surface, &mut reply);
                    flush(c.conn, &reply);
                }
                focus = Some((slot, surface));
            }

            pump_input(&mut clients, focus, &mut istate);
        }
    }

    /// Turn queued input into events for the focused connection — or, with nothing
    /// focused, drain and discard so the input ring does not back up.
    fn pump_input(
        clients: &mut [Option<Client>; MAX_CLIENTS],
        focus: Option<(usize, u32)>,
        st: &mut InputState,
    ) {
        match focus
            .and_then(|(slot, _)| clients.get_mut(slot))
            .and_then(|c| c.as_mut())
        {
            Some(c) => {
                let mut reply = DispatchBuf::new(&mut c.out);
                drain_input(&mut c.server, st, &mut reply);
                let n = reply.len();
                if n > 0 {
                    let _ = uds::send(c.conn, &c.out[..n]);
                }
            }
            None => {
                let mut batch = [0u8; 48];
                while fetch_input(&mut batch).is_some() {}
            }
        }
    }

    pub fn main_loop() {
        let Some(fb) = attach_display() else {
            return;
        };
        let Some(listen) = bind_listener() else {
            return;
        };
        unsafe { minix_rt::write(1, b"wlserver: ready\n".as_ptr(), 16) };
        serve_forever(listen, fb);
    }
}

/// Entry point, called from the `/sbin/wlserver` binary.
pub fn wlserver_main() {
    #[cfg(all(target_os = "minix", not(target_arch = "wasm32")))]
    imp::main_loop();
}

#[cfg(test)]
mod tests {
    use super::KEYMAP_XKB;

    #[test]
    fn the_keymap_is_the_generated_xkb_text() {
        assert!(
            KEYMAP_XKB.starts_with(b"xkb_keymap {"),
            "the artifact must be the XKB text, not a blob"
        );
        assert_eq!(
            KEYMAP_XKB.len(),
            64756,
            "regenerate with the `xkbcli` command in WAYLAND.md §6.12"
        );
    }
}
