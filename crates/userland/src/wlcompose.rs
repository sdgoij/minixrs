//! `/bin/wlcompose` — the compositor's GPU-composition helper (`.notes/wayland.md` §6.10, stage
//! 3d-b-5b).
//!
//! `/sbin/wlserver` is static and keeps Track A Mesa-free, so composition lives in a separate,
//! stateless process with its own EGL context and Mesa DSOs (D14): a client draws a surface on the
//! render node, hands over its dma-buf descriptor, and the helper imports it, draws it as a quad
//! into one frame, and reads that frame back once (D16, §5 row 27). The handoff is a UNIX-domain
//! socket with `SCM_RIGHTS`; the reply is the **frame's** descriptor, which the compositor presents
//! uncopied.
//!
//! Two modes, so the mechanism can be measured from the shell:
//!   * `serve` — bind the socket and, for each frame the peer describes (`begin`/`surface`/`end`),
//!     compose the surfaces and reply with the frame's descriptor;
//!   * `test` — the peer: make two GL surfaces, ask a `serve` **in another process** to compose
//!     them, import the frame it returns, and check the two rectangles and the background.
//!
//! The one thing this stage exists to measure is **cross-process sampling**: the peer's surface is
//! drawn by one Mesa context and imported by another in a different process. If virgl's resources
//! do not cross that boundary, the handoff shape changes, which is why `test` checks pixels rather
//! than merely reaching `end`.
//!
//! The frame is a full-output object on the render node, so the fb server can scan it out the same
//! way it scans a `zwp_linux_dmabuf_v1` client's (3d-b-1). A dynamic image is required (Mesa's
//! DSOs); `tools/build-wlcompose.py` links this crate and `tools/wlcompose_render.c` together.

#![cfg_attr(not(target_os = "minix"), allow(dead_code))]

use minix_std::fs;
#[cfg(target_os = "minix")]
use minix_std::fs::PollFd;
#[cfg(target_os = "minix")]
use minix_std::uds;
use minix_std::vmem;

/// The socket the helper binds. `/run` is where `/sbin/wlserver` binds its own, so it exists by
/// the time the helper runs.
const SOCK_PATH: &[u8] = b"/run/wlcompose";
/// The render node the frame and the imported surfaces live on.
const NODE: &[u8] = b"/dev/dri/renderD128";

/// The frame's geometry: a full-output frame is what the compositor can scan out (3d-b-1).
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const FRAME_LEN: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// The most surfaces one frame may name — the compositor's window list, bounded so the peer cannot
/// overrun the fixed array.
const MAX_SURFACES: usize = 8;

/// `VIRTIO_GPU_FORMAT_B8G8R8X8` and `VIRTIO_GPU_FORMAT_TEXTURE_2D` — the frame object's format and
/// target; `VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW`, the binds `/bin/drmprime` uses.
const FORMAT_B8G8R8X8: u32 = 2;
const TARGET_TEXTURE_2D: u32 = 2;
const BIND_TEXTURE: u32 = (1 << 1) | (1 << 3);

const POLLIN: i16 = 0x001;

const DRM_RDWR: u32 = 0o02;
const DRM_CLOEXEC: u32 = 0o200000;

/// `_IOWR` from `asm-generic/ioctl.h`, and the render node's request numbers — declared here rather
/// than imported, so the helper's agreement with the node is evidence and not a tautology.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}
const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_IOCTL_PRIME_HANDLE_TO_FD: u32 = iowr(DRM_IOCTL_BASE, 0x2d, 12);
const DRM_IOCTL_PRIME_FD_TO_HANDLE: u32 = iowr(DRM_IOCTL_BASE, 0x2e, 12);
const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x01, 16);
const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x04, 56);

/// `struct drm_prime_handle`: `u32 handle`, `u32 flags`, `s32 fd`.
#[repr(C)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

/// `struct drm_virtgpu_map`: `u64 offset`, `u32 handle`, `u32 pad`.
#[repr(C)]
struct Map {
    offset: u64,
    handle: u32,
    pad: u32,
}

/// `struct drm_virtgpu_resource_create`.
#[repr(C)]
struct ResourceCreate {
    target: u32,
    format: u32,
    bind: u32,
    width: u32,
    height: u32,
    depth: u32,
    array_size: u32,
    last_level: u32,
    nr_samples: u32,
    flags: u32,
    bo_handle: u32,
    res_handle: u32,
    size: u32,
    stride: u32,
}

/// `struct wlcompose_surface` (`tools/wlcompose_render.c`) — one surface in a frame: the plane's
/// descriptor and geometry, and where its top-left lands in the frame.
#[repr(C)]
#[derive(Clone, Copy)]
struct Surface {
    fd: i32,
    w: u32,
    h: u32,
    stride: u32,
    x: u32,
    y: u32,
}

const NO_SURFACE: Surface = Surface {
    fd: -1,
    w: 0,
    h: 0,
    stride: 0,
    x: 0,
    y: 0,
};

/// The composited frame: a full-output object on the render node, mapped for the readback and
/// exported so the compositor can present it.
struct Frame {
    fd: i32,
    /// The object's pages, which `wlcompose_render` fills.
    va: *mut u8,
    w: u32,
    h: u32,
}

// The C half of the helper (`tools/wlcompose_render.c`). The whole GL/GBM half is minix-only: a
// host build of this crate has no such symbols.
#[cfg(target_os = "minix")]
unsafe extern "C" {
    fn wlcompose_make_surface(w: u32, h: u32, r: u8, g: u8, b: u8, out_stride: *mut u32) -> i32;
    fn wlcompose_render(surfaces: *const Surface, n: i32, w: u32, h: u32, out: *mut u8) -> i32;
}

fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<i32, i32> {
    match unsafe { fs::ioctl(fd, request, arg) } {
        Ok(code) => Ok(code),
        Err(e) => Err(e.0),
    }
}

fn fail(msg: &[u8]) -> i32 {
    crate::write_err(msg);
    1
}

/// A `u32` in decimal; `0` when the token is not one.
fn parse_u32(b: &[u8]) -> Option<u32> {
    if b.is_empty() {
        return None;
    }
    let mut v = 0u32;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as u32)?;
    }
    Some(v)
}

/// The `idx`-th whitespace-separated token of a protocol line.
fn token(line: &[u8], idx: usize) -> Option<&[u8]> {
    line.split(|&c| c == b' ')
        .filter(|s| !s.is_empty())
        .nth(idx)
}

/// The `idx`-th token of a protocol line, as a `u32`.
fn field(line: &[u8], idx: usize) -> Option<u32> {
    parse_u32(token(line, idx)?)
}

fn push(out: &mut [u8], at: &mut usize, bytes: &[u8]) {
    out[*at..*at + bytes.len()].copy_from_slice(bytes);
    *at += bytes.len();
}

fn push_dec(out: &mut [u8], at: &mut usize, mut v: u32) {
    let mut tmp = [0u8; 10];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    push(out, at, &tmp[i..]);
}

/// A `0x`-prefixed `u32`, for the pixel values a failed check reports.
fn push_hex(out: &mut [u8], at: &mut usize, v: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    push(out, at, b"0x");
    for shift in (0..8).rev() {
        out[*at] = HEX[((v >> (shift * 4)) & 0xf) as usize];
        *at += 1;
    }
}

/// A full-output object on the render node, mapped and exported — the frame the compositor will
/// present. `None` if the node refuses any step.
fn alloc_frame(w: u32, h: u32) -> Option<Frame> {
    let node = match unsafe { fs::open(NODE, fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            crate::write_err(b"wlcompose: no render node at /dev/dri/renderD128\n");
            return None;
        }
    };
    let size = (w as usize) * (h as usize) * 4;
    let mut create = ResourceCreate {
        target: TARGET_TEXTURE_2D,
        format: FORMAT_B8G8R8X8,
        bind: BIND_TEXTURE,
        width: w,
        height: h,
        depth: 1,
        array_size: 1,
        last_level: 0,
        nr_samples: 0,
        flags: 0,
        bo_handle: 0,
        res_handle: 0,
        size: size as u32,
        stride: w * 4,
    };
    if ioctl(
        node,
        DRM_IOCTL_VIRTGPU_RESOURCE_CREATE,
        core::ptr::addr_of_mut!(create) as *mut u8,
    )
    .is_err()
    {
        crate::write_err(b"wlcompose: RESOURCE_CREATE failed\n");
        let _ = fs::close(node);
        return None;
    }
    let mut map = Map {
        offset: 0,
        handle: create.bo_handle,
        pad: 0,
    };
    if ioctl(
        node,
        DRM_IOCTL_VIRTGPU_MAP,
        core::ptr::addr_of_mut!(map) as *mut u8,
    )
    .is_err()
    {
        crate::write_err(b"wlcompose: VIRTGPU_MAP failed\n");
        let _ = fs::close(node);
        return None;
    }
    let va = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            size,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            node,
            map.offset as i64,
        )
    };
    if va == vmem::MAP_FAILED {
        crate::write_err(b"wlcompose: mmap of the frame failed\n");
        let _ = fs::close(node);
        return None;
    }
    let mut export = PrimeHandle {
        handle: create.bo_handle,
        flags: DRM_RDWR | DRM_CLOEXEC,
        fd: -1,
    };
    if ioctl(
        node,
        DRM_IOCTL_PRIME_HANDLE_TO_FD,
        core::ptr::addr_of_mut!(export) as *mut u8,
    )
    .is_err()
        || export.fd < 0
    {
        crate::write_err(b"wlcompose: PRIME_HANDLE_TO_FD failed\n");
        let _ = unsafe { vmem::munmap(va, size) };
        let _ = fs::close(node);
        return None;
    }
    // The object outlives this descriptor: it lives in the device's table, and the mapping stays
    // valid, so the node descriptor itself is not needed past the export.
    let _ = fs::close(node);
    Some(Frame {
        fd: export.fd,
        va,
        w,
        h,
    })
}

/// Import the frame's descriptor and map its pages, so the peer can read the composed pixels back.
fn import_frame(desc: i32, len: usize) -> Option<*mut u8> {
    let node = unsafe { fs::open(NODE, fs::O_RDWR, 0) }.ok()?;
    let mut import = PrimeHandle {
        handle: 0,
        flags: DRM_RDWR | DRM_CLOEXEC,
        fd: desc,
    };
    if ioctl(
        node,
        DRM_IOCTL_PRIME_FD_TO_HANDLE,
        core::ptr::addr_of_mut!(import) as *mut u8,
    )
    .is_err()
    {
        let _ = fs::close(node);
        return None;
    }
    let mut map = Map {
        offset: 0,
        handle: import.handle,
        pad: 0,
    };
    if ioctl(
        node,
        DRM_IOCTL_VIRTGPU_MAP,
        core::ptr::addr_of_mut!(map) as *mut u8,
    )
    .is_err()
    {
        let _ = fs::close(node);
        return None;
    }
    let va = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            len,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            node,
            map.offset as i64,
        )
    };
    let _ = fs::close(node);
    (va != vmem::MAP_FAILED).then_some(va)
}

/// One XRGB8888 pixel of a mapped frame. Little-endian bytes B,G,R,X.
fn read_px(va: *const u8, x: u32, y: u32) -> u32 {
    let off = ((y as usize) * (WIDTH as usize) + (x as usize)) * 4;
    unsafe { core::ptr::read_unaligned(va.add(off) as *const u32) }
}

/// The compositor helper: bind the socket and compose frames for whichever peer connects.
#[cfg(target_os = "minix")]
fn serve() -> i32 {
    // `/run` holds `wlserver`'s socket, so it exists by now; the mkdir is only for a host that
    // somehow reached here first.
    let _ = fs::mkdir(b"/run", 0o777);
    let listen = match uds::socket() {
        Ok(fd) => fd,
        Err(_) => return fail(b"wlcompose: socket failed\n"),
    };
    if uds::bind(listen, SOCK_PATH).is_err() || uds::listen(listen, 4).is_err() {
        let _ = uds::close_fd(listen);
        return fail(b"wlcompose: bind/listen failed\n");
    }
    crate::write_out(b"wlcompose: ready\n");
    loop {
        let mut pf = [PollFd {
            fd: listen,
            events: POLLIN,
            revents: 0,
        }];
        if fs::poll(&mut pf, 200).is_err() || pf[0].revents & POLLIN == 0 {
            continue;
        }
        if let Ok(conn) = uds::accept(listen) {
            client(conn);
            let _ = uds::close_fd(conn);
        }
    }
}

/// Wait until `fd` is readable, up to `timeout_ms`. The local sockets are non-blocking (`uds.rs`:
/// a caller retries rather than waits), and VFS is single-threaded, so a long `poll` would hold the
/// one worker and starve the other process — the helper's GL transfer is itself a VFS request. So
/// this polls in short slices, the way `/sbin/wlserver`'s loop uses a small tick.
#[cfg(target_os = "minix")]
fn wait_readable(fd: i32, timeout_ms: i32) -> bool {
    let mut waited = 0;
    loop {
        let mut pf = [PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        }];
        if matches!(fs::poll(&mut pf, 25), Ok(n) if n > 0) && pf[0].revents & POLLIN != 0 {
            return true;
        }
        waited += 25;
        if waited >= timeout_ms {
            return false;
        }
    }
}

/// Read one peer's frames until it disconnects. A frame is `begin <w> <h>`, then one `surface`
/// per surface (its plane descriptor travels by `SCM_RIGHTS`), then `end`; the reply is the
/// frame's descriptor and an `ok <w> <h> <stride>` line. Reads are poll-guarded, and `0` is the
/// only end-of-connection — an `EAGAIN` on a non-blocking socket is retried.
#[cfg(target_os = "minix")]
fn client(conn: i32) {
    // Greet on accept: a descriptor a peer sends before the helper accepts would be keyed to the
    // not-yet-accepted connection and lost, so the peer waits for this line before it sends.
    let _ = uds::send(conn, b"ready\n");
    let mut buf = [0u8; 1024];
    let mut len = 0usize;
    let mut frame: Option<Frame> = None;
    let mut surfaces = [NO_SURFACE; MAX_SURFACES];
    let mut n_surf = 0usize;
    loop {
        if !wait_readable(conn, 200) {
            continue;
        }
        let got = match uds::recv(conn, &mut buf[len..]) {
            Ok(0) => return,
            Ok(n) => n as usize,
            Err(_) => continue,
        };
        len += got;
        let mut start = 0usize;
        while let Some(rel) = buf[start..len].iter().position(|&c| c == b'\n') {
            let end = start + rel;
            handle_line(
                conn,
                &buf[start..end],
                &mut frame,
                &mut surfaces,
                &mut n_surf,
            );
            start = end + 1;
        }
        buf.copy_within(start..len, 0);
        len -= start;
    }
}

/// Act on one protocol line. The plane descriptors are drained once, at `end`: `recvmsg`
/// (`vfs::scm`) hands a receiver *every* descriptor pending on the socket in one call, and answers
/// `EOVERFLOW` if the receiver offered room for fewer — so a `recv_fds` per line would overflow on
/// the second surface, whose descriptor the peer queued before the first was read. Draining once
/// at `end` and pairing descriptors with `surface` lines in order is therefore the shape that
/// fits the transport.
#[cfg(target_os = "minix")]
fn handle_line(
    conn: i32,
    line: &[u8],
    frame: &mut Option<Frame>,
    surfaces: &mut [Surface; MAX_SURFACES],
    n_surf: &mut usize,
) {
    match token(line, 0) {
        Some(b"begin") => {
            if let (Some(w), Some(h)) = (field(line, 1), field(line, 2))
                && frame.is_none()
            {
                *frame = alloc_frame(w, h);
            }
        }
        Some(b"surface") => {
            if *n_surf >= MAX_SURFACES {
                return;
            }
            let s = &mut surfaces[*n_surf];
            s.fd = -1;
            s.w = field(line, 1).unwrap_or(0);
            s.h = field(line, 2).unwrap_or(0);
            s.stride = field(line, 3).unwrap_or(0);
            // Field 4 is the plane's DRM format; 3d-b-5 composition is XRGB8888 only, so it is
            // carried for the compositor and not yet read.
            s.x = field(line, 5).unwrap_or(0);
            s.y = field(line, 6).unwrap_or(0);
            *n_surf += 1;
        }
        Some(b"end") => {
            // Drain the planes the peer queued for this frame, in the order its `surface` lines
            // named them. `recvmsg` (`vfs::scm`) hands over everything pending on the socket, but
            // collect across calls too, so the shape does not depend on how many land together.
            let mut fds = [-1i32; MAX_SURFACES];
            let mut got = 0usize;
            loop {
                let mut one = [-1i32; MAX_SURFACES];
                match uds::recv_fds(conn, &mut one) {
                    Ok(k) if k > 0 => {
                        for &fd in &one[..k] {
                            if got < MAX_SURFACES {
                                fds[got] = fd;
                                got += 1;
                            }
                        }
                    }
                    _ => break,
                }
                if got >= *n_surf {
                    break;
                }
            }
            let mut matches = got == *n_surf;
            for (s, &fd) in surfaces.iter_mut().take(*n_surf).zip(fds.iter()) {
                s.fd = fd;
                matches &= fd >= 0;
            }
            if !matches {
                let mut msg = [0u8; 48];
                let mut at = 0usize;
                push(&mut msg, &mut at, b"wlcompose: got ");
                push_dec(&mut msg, &mut at, got as u32);
                push(&mut msg, &mut at, b" planes for ");
                push_dec(&mut msg, &mut at, *n_surf as u32);
                push(&mut msg, &mut at, b" surfaces\n");
                crate::write_err(&msg[..at]);
                *n_surf = 0;
                return;
            }
            match frame.as_ref() {
                Some(f) if compose(f, surfaces, *n_surf) == 0 => {
                    let _ = uds::send_fds(conn, &[f.fd]);
                    let mut reply = [0u8; 40];
                    let mut at = 0usize;
                    push(&mut reply, &mut at, b"ok ");
                    push_dec(&mut reply, &mut at, f.w);
                    push(&mut reply, &mut at, b" ");
                    push_dec(&mut reply, &mut at, f.h);
                    push(&mut reply, &mut at, b" ");
                    push_dec(&mut reply, &mut at, f.w * 4);
                    push(&mut reply, &mut at, b"\n");
                    let _ = uds::send(conn, &reply[..at]);
                }
                Some(_) => crate::write_err(b"wlcompose: the composition failed\n"),
                None => crate::write_err(b"wlcompose: no frame to compose into\n"),
            }
            for s in surfaces.iter().take(*n_surf) {
                let _ = uds::close_fd(s.fd);
            }
            *n_surf = 0;
        }
        _ => {}
    }
}

/// Draw the surface list into the frame (the C half reads the pixels and reads them back once).
#[cfg(target_os = "minix")]
fn compose(frame: &Frame, surfaces: &[Surface; MAX_SURFACES], n: usize) -> i32 {
    unsafe { wlcompose_render(surfaces.as_ptr(), n as i32, frame.w, frame.h, frame.va) }
}

/// Connect to a helper, retrying while it is still coming up — the shell starts it in the
/// background and this peer may run before it has bound.
#[cfg(target_os = "minix")]
fn connect() -> Result<i32, i32> {
    let fd = uds::socket().map_err(|_| fail(b"wlcompose: socket failed\n"))?;
    for _ in 0..100_000 {
        if uds::connect(fd, SOCK_PATH).is_ok() {
            return Ok(fd);
        }
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
    let _ = uds::close_fd(fd);
    Err(fail(b"wlcompose: connect to the helper failed\n"))
}

/// The test peer: two GL surfaces composed by a helper in another process, then read back. The
/// check is the cross-process measurement — if the helper sampled the peer's surface, the two
/// rectangles hold the colours the peer drew.
#[cfg(target_os = "minix")]
fn test() -> i32 {
    const SRC: u32 = 64;
    // The rectangles: green at (64,64), red at (256,64), each 64x64, in a 1024x768 frame.
    const GREEN: (u32, u32) = (64, 64);
    const RED: (u32, u32) = (256, 64);

    let (mut green_stride, mut red_stride) = (0u32, 0u32);
    let green = unsafe { wlcompose_make_surface(SRC, SRC, 0, 255, 0, &mut green_stride) };
    let red = unsafe { wlcompose_make_surface(SRC, SRC, 255, 0, 0, &mut red_stride) };
    if green < 0 || red < 0 {
        return fail(b"wlcompose: the peer could not make its surfaces\n");
    }

    let conn = match connect() {
        Ok(c) => c,
        Err(code) => return code,
    };

    // Wait for the helper's greeting, so the descriptor sends below happen after its `accept`.
    if !wait_readable(conn, 30_000) {
        return fail(b"wlcompose: the helper did not greet\n");
    }
    let mut hello = [0u8; 8];
    let _ = uds::recv(conn, &mut hello);

    let _ = uds::send(conn, b"begin 1024 768\n");
    for (fd, stride, (x, y)) in [(green, green_stride, GREEN), (red, red_stride, RED)] {
        let _ = uds::send_fds(conn, &[fd]);
        let mut line = [0u8; 64];
        let mut at = 0usize;
        push(&mut line, &mut at, b"surface ");
        push_dec(&mut line, &mut at, SRC);
        push(&mut line, &mut at, b" ");
        push_dec(&mut line, &mut at, SRC);
        push(&mut line, &mut at, b" ");
        push_dec(&mut line, &mut at, stride);
        push(&mut line, &mut at, b" 2 ");
        push_dec(&mut line, &mut at, x);
        push(&mut line, &mut at, b" ");
        push_dec(&mut line, &mut at, y);
        push(&mut line, &mut at, b"\n");
        let _ = uds::send(conn, &line[..at]);
    }
    let _ = uds::send(conn, b"end\n");

    // The helper's reply is the frame's descriptor as control data plus an `ok` line, and the
    // control is lifted *after* the data read — the order `/sbin/wlserver` reads a client's plane
    // in, and the order `await_keymap` reads a keymap fd. Both reads are poll-guarded: the sockets
    // are non-blocking, so an unguarded `recv` would race the helper's reply.
    let mut ack = [0u8; 40];
    if !wait_readable(conn, 30_000) {
        return fail(b"wlcompose: the helper did not answer\n");
    }
    if !matches!(uds::recv(conn, &mut ack), Ok(n) if n > 0) {
        return fail(b"wlcompose: the helper did not answer\n");
    }
    let mut fds = [-1i32; 1];
    if !matches!(uds::recv_fds(conn, &mut fds), Ok(1)) || fds[0] < 0 {
        return fail(b"wlcompose: the helper returned no frame\n");
    }

    let va = match import_frame(fds[0], FRAME_LEN) {
        Some(va) => va,
        None => return fail(b"wlcompose: the returned frame would not import\n"),
    };

    let got_green = read_px(va, GREEN.0 + SRC / 2, GREEN.1 + SRC / 2);
    let got_red = read_px(va, RED.0 + SRC / 2, RED.1 + SRC / 2);
    let got_bg = read_px(va, WIDTH / 2, HEIGHT / 2);
    let ok = got_green == 0x0000_FF00u32 && got_red == 0x00FF_0000u32 && got_bg == 0;

    if ok {
        crate::write_out(b"wlcompose: PASS\n");
        0
    } else {
        let mut line = [0u8; 96];
        let mut at = 0usize;
        push(&mut line, &mut at, b"wlcompose: FAIL green=");
        push_hex(&mut line, &mut at, got_green);
        push(&mut line, &mut at, b" red=");
        push_hex(&mut line, &mut at, got_red);
        push(&mut line, &mut at, b" bg=");
        push_hex(&mut line, &mut at, got_bg);
        push(&mut line, &mut at, b"\n");
        crate::write_err(&line[..at]);
        1
    }
}

/// `/bin/wlcompose`: `serve` in one process, `test` in another.
#[cfg(target_os = "minix")]
pub fn wlcompose(args: &[&str]) -> i32 {
    match args.get(1).copied() {
        Some("serve") => serve(),
        Some("test") => test(),
        _ => {
            crate::write_err(b"usage: wlcompose serve|test\n");
            2
        }
    }
}
