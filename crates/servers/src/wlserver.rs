//! Wayland server (Phase 1) — boot proc 20, `/sbin/wlserver`.
//!
//! Speaks the core protocol plus `wl_shm` over a `/dev/uds` socket, and presents
//! a committed buffer to `/dev/fb`. The protocol itself is `crates/wayland`
//! (pure, host-tested); this is the loop around it: accept a client, decode its
//! requests, answer them, and composite a commit onto the display.
//!
//! It maps the framebuffer directly and sends `FBIOFLUSH` **to the fb server**,
//! not through VFS, the way `wserver` does — VFS is single-worker and the shell's
//! blocking console read holds it, so a device ioctl through VFS would wait for a
//! keystroke (`WAYLAND.md` §6.11).

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
    use wayland::server::{Commit, Server};
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

    /// Composite a commit's buffer onto the display and present it.
    ///
    /// The pool is mapped read-only for as long as the copy takes: the client
    /// owns those frames, and this process only reads them.
    fn present(fb: *mut u8, pools: &Pools, c: &Commit) {
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
        let dst = Destination {
            x: 0,
            y: 0,
            width: XRES as i32,
            height: YRES as i32,
        };
        if shm::blit(bytes, &c.buffer(), dst, |x, y, px| {
            let off = y as usize * PITCH + x as usize * 4;
            unsafe { core::ptr::write_volatile(fb.add(off).cast::<u32>(), px) };
        })
        .is_err()
        {
            report(b"wlserver: blit rejected a buffer\n");
        }
        let _ = unsafe { vmem::munmap(src, len) };
        fb_flush();
    }

    fn wait_readable(fd: i32, timeout_ms: i32) -> bool {
        let mut pf = [PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        }];
        match fs::poll(&mut pf, timeout_ms) {
            Ok(n) if n > 0 => pf[0].revents & POLLIN != 0,
            _ => false,
        }
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

    /// Serve one connection until it closes or sends something unparseable.
    fn serve_client(conn: i32, fb: *mut u8) {
        let mut server = Server::new(XRES as i32, YRES as i32);
        let mut pools = Pools::new();
        let mut inbuf = MessageBuffer::new();
        let mut chunk = [0u8; 512];
        let mut out = [0u8; 8192];
        let mut istate = InputState::new();
        'conn: loop {
            // Wake on a client message or a tick; input is fetched either way, so a
            // key that arrives while no request is pending is still delivered.
            let ready = wait_readable(conn, TICK_MS);
            let mut reply = DispatchBuf::new(&mut out);
            drain_input(&mut server, &mut istate, &mut reply);
            if ready {
                let n = match uds::recv(conn, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => n as usize,
                    Err(_) => break,
                };
                if inbuf.push(&chunk[..n]).is_err() {
                    break;
                }
                loop {
                    let msg = match inbuf.next() {
                        Ok(Some(m)) => m,
                        Ok(None) => break,
                        Err(_) => break 'conn,
                    };
                    let h = match Header::parse(msg) {
                        Ok(h) => h,
                        Err(_) => break 'conn,
                    };
                    let outcome = match server.dispatch(
                        h.object_id,
                        h.opcode,
                        &msg[HEADER_LEN..],
                        &mut reply,
                    ) {
                        Ok(o) => o,
                        Err(_) => break 'conn,
                    };
                    // A `wl_shm.create_pool` named an fd index; the descriptor itself
                    // travelled with this message by `SCM_RIGHTS`.
                    if let Some(pool) = outcome.new_pool {
                        let mut fds = [-1i32; 1];
                        if let Ok(1) = uds::recv_fds(conn, &mut fds) {
                            pools.add(pool, fds[0]);
                        }
                    }
                    if let Some(commit) = server.take_commit() {
                        present(fb, &pools, &commit);
                        let _ = server.release_buffer(commit.buffer, &mut reply);
                    }
                    inbuf.consume(h.size as usize);
                }
            }
            if !reply.is_empty() {
                let _ = uds::send(conn, reply.bytes());
            }
        }
        pools.close_all();
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
        loop {
            if !wait_readable(listen, -1) {
                continue;
            }
            if let Ok(conn) = uds::accept(listen) {
                serve_client(conn, fb);
                let _ = uds::close_fd(conn);
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
