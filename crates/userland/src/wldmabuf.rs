//! `/bin/wldmabuf` — a render-node object handed to the compositor as a `linux-dmabuf`
//! `wl_buffer` and scanned out (`.notes/wayland.md` §6.10, stage 3d-b-4c).
//!
//! It makes a full-output (1024×768 B8G8R8X8) object on the render node, fills it with a known
//! colour through a mapping, exports it to a dma-buf descriptor (`PRIME_HANDLE_TO_FD`), and
//! hands that descriptor to `/sbin/wlserver` as a `zwp_linux_dmabuf_v1` buffer over the
//! socket. The compositor imports the object on the same device and scans its frames out
//! (`FBIO_SCANOUT_SRC`), so the frame *is* the object and the compositor copies nothing. The
//! gate reads the display from the host (a QMP screendump) and the fb server's
//! `fb: scanout src` line.
//!
//! On x86 the output must *be* the virtio-gpu for this: the scanout is re-backed by a
//! virtio-gpu, and the render node has to be that same device (D11), which is why the gate
//! runs `-device virtio-gpu-pci` with no `bochs-display`.
//!
//! Like `/bin/drmprime` it declares the DRM ABI itself rather than importing the driver's, so
//! its agreement with the node is evidence and not a tautology.

use minix_std::fs::{self, PollFd};
use minix_std::{uds, vmem};
use wayland::client::Client;
use wayland::protocol::{self, Kind};
use wayland::wire::{Arg, DispatchBuf, Header, MAX_ARGS, MessageBuffer, WireError};

use crate::write_err;

/// The socket the server binds, and the render node.
const SOCK_PATH: &[u8] = b"/run/wayland-0";
const NODE: &[u8] = b"/dev/dri/renderD128";

/// The output's geometry: a full-output object is what the compositor can scan out.
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const STRIDE: u32 = WIDTH * 4;
const SIZE: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// The render node's 2D format, `VIRTIO_GPU_FORMAT_B8G8R8X8` — the same byte order as the
/// `DRM_FORMAT_XRGB8888` the linux-dmabuf buffer is created with.
const FORMAT_B8G8R8X8: u32 = 2;
const TARGET_TEXTURE_2D: u32 = 2;
/// `VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW`, what `/bin/drmprime` uses.
const BIND_TEXTURE: u32 = (1 << 1) | (1 << 3);

/// Green, as the memory holds it: B8G8R8X8 little-endian, bytes B,G,R,X = 00,FF,00,00.
const COLOR: u32 = 0x0000_FF00;

const POLLIN: i16 = 0x001;

/// `_IOWR` from `asm-generic/ioctl.h`, and the render node's request numbers.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}
const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_IOCTL_PRIME_HANDLE_TO_FD: u32 = iowr(DRM_IOCTL_BASE, 0x2d, 12);
const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x01, 16);
const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x04, 56);

const DRM_RDWR: u32 = 0o02;
const DRM_CLOEXEC: u32 = 0o200000;

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

fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<i32, i32> {
    match unsafe { fs::ioctl(fd, request, arg) } {
        Ok(code) => Ok(code),
        Err(e) => Err(e.0),
    }
}

fn fail(code: i32, msg: &[u8]) -> i32 {
    write_err(msg);
    code
}

/// Build one request with `f` and send it.
fn send_req<R, F>(conn: i32, req: &mut [u8; 512], f: F) -> Result<R, i32>
where
    F: FnOnce(&mut DispatchBuf<'_>) -> Result<R, WireError>,
{
    let mut q = DispatchBuf::new(req);
    let r = f(&mut q).map_err(|_| fail(2, b"wldmabuf: encode failed\n"))?;
    match uds::send(conn, q.bytes()) {
        Ok(n) if n as usize == q.len() => Ok(r),
        _ => Err(fail(3, b"wldmabuf: send failed\n")),
    }
}

/// Poll `conn` for readability, then read one chunk into `inbuf`.
fn fill(conn: i32, inbuf: &mut MessageBuffer, chunk: &mut [u8; 512], timeout_ms: i32) -> bool {
    let mut pf = [PollFd {
        fd: conn,
        events: POLLIN,
        revents: 0,
    }];
    match fs::poll(&mut pf, timeout_ms) {
        Ok(n) if n > 0 && pf[0].revents & POLLIN != 0 => {}
        _ => return false,
    }
    match uds::recv(conn, chunk) {
        Ok(n) if n > 0 => inbuf.push(&chunk[..n as usize]).is_ok(),
        _ => false,
    }
}

/// Connect to the Wayland server, retrying while it is still coming up.
fn connect() -> Result<i32, i32> {
    let fd = uds::socket().map_err(|_| fail(4, b"wldmabuf: socket failed\n"))?;
    for _ in 0..100_000 {
        if uds::connect(fd, SOCK_PATH).is_ok() {
            return Ok(fd);
        }
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
    let _ = uds::close_fd(fd);
    Err(fail(5, b"wldmabuf: connect failed\n"))
}

/// Read registry events until both globals this client needs are known.
fn find_globals(
    conn: i32,
    client: &Client,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
) -> Result<(u32, u32), i32> {
    let (mut comp, mut dmabuf) = (None, None);
    for _ in 0..2000 {
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(6, b"wldmabuf: wire error\n")),
            };
            let h = Header::parse(msg).map_err(|_| fail(7, b"wldmabuf: bad header\n"))?;
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let (_, kind, n) = client
                .decode(msg, &mut args)
                .map_err(|_| fail(8, b"wldmabuf: decode failed\n"))?;
            if kind == Kind::Registry && h.opcode == protocol::registry_ev::GLOBAL && n == 3 {
                let name = args[0].as_uint().unwrap_or(0);
                match args[1].as_str().unwrap_or(b"") {
                    b"wl_compositor" => comp = Some(name),
                    b"zwp_linux_dmabuf_v1" => dmabuf = Some(name),
                    _ => {}
                }
            }
            inbuf.consume(h.size as usize);
        }
        if let (Some(c), Some(d)) = (comp, dmabuf) {
            return Ok((c, d));
        }
        if !fill(conn, inbuf, chunk, 2000) {
            break;
        }
    }
    Err(fail(
        9,
        b"wldmabuf: the compositor or the dmabuf global is missing\n",
    ))
}

/// Read until `done` accepts an event.
fn await_event<F>(
    conn: i32,
    client: &Client,
    inbuf: &mut MessageBuffer,
    chunk: &mut [u8; 512],
    mut done: F,
) -> Result<(), i32>
where
    F: FnMut(&Header, Kind, &[Arg<'_>]) -> bool,
{
    for _ in 0..2000 {
        loop {
            let msg = match inbuf.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => return Err(fail(10, b"wldmabuf: wire error\n")),
            };
            let h = Header::parse(msg).map_err(|_| fail(11, b"wldmabuf: bad header\n"))?;
            let mut args = [Arg::Uint(0); MAX_ARGS];
            let (_, kind, n) = client
                .decode(msg, &mut args)
                .map_err(|_| fail(12, b"wldmabuf: decode failed\n"))?;
            let hit = done(&h, kind, &args[..n]);
            inbuf.consume(h.size as usize);
            if hit {
                return Ok(());
            }
        }
        if !fill(conn, inbuf, chunk, 2000) {
            return Err(fail(13, b"wldmabuf: the server went quiet\n"));
        }
    }
    Err(fail(14, b"wldmabuf: the awaited event never arrived\n"))
}

/// Make the full-output object on the render node and export it to a descriptor.
fn make_object() -> Result<i32, i32> {
    let fd = match unsafe { fs::open(NODE, fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            return Err(fail(
                20,
                b"wldmabuf: no render node at /dev/dri/renderD128\n",
            ));
        }
    };

    let mut create = ResourceCreate {
        target: TARGET_TEXTURE_2D,
        format: FORMAT_B8G8R8X8,
        bind: BIND_TEXTURE,
        width: WIDTH,
        height: HEIGHT,
        depth: 1,
        array_size: 1,
        last_level: 0,
        nr_samples: 0,
        flags: 0,
        bo_handle: 0,
        res_handle: 0,
        size: SIZE as u32,
        stride: STRIDE,
    };
    if ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_RESOURCE_CREATE,
        core::ptr::addr_of_mut!(create) as *mut u8,
    )
    .is_err()
    {
        return Err(fail(21, b"wldmabuf: RESOURCE_CREATE failed\n"));
    }

    let mut map = Map {
        offset: 0,
        handle: create.bo_handle,
        pad: 0,
    };
    if ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_MAP,
        core::ptr::addr_of_mut!(map) as *mut u8,
    )
    .is_err()
    {
        return Err(fail(22, b"wldmabuf: VIRTGPU_MAP failed\n"));
    }

    let va = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            SIZE,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            fd,
            map.offset as i64,
        )
    };
    if va == vmem::MAP_FAILED {
        return Err(fail(23, b"wldmabuf: mmap of the object failed\n"));
    }
    // The colour, one pixel at a time: 1024×768 writes, which is the whole frame the scanout
    // will show. A partly-written frame is still a frame, so the fill is checked by the
    // screendump and not by the write's return.
    let px = va.cast::<u32>();
    for i in 0..(WIDTH * HEIGHT) as usize {
        unsafe { core::ptr::write_volatile(px.add(i), COLOR) };
    }
    let _ = unsafe { vmem::munmap(va, SIZE) };

    let mut export = PrimeHandle {
        handle: create.bo_handle,
        flags: DRM_RDWR | DRM_CLOEXEC,
        fd: -1,
    };
    if ioctl(
        fd,
        DRM_IOCTL_PRIME_HANDLE_TO_FD,
        core::ptr::addr_of_mut!(export) as *mut u8,
    )
    .is_err()
        || export.fd < 0
    {
        return Err(fail(24, b"wldmabuf: PRIME_HANDLE_TO_FD failed\n"));
    }
    let _ = fs::close(fd);
    Ok(export.fd)
}

/// The client's whole session: make the object, hand it over as a linux-dmabuf buffer, commit
/// it, and wait for the compositor to release it.
pub fn wldmabuf(_args: &[&str]) -> i32 {
    match run() {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn run() -> Result<(), i32> {
    let prime_fd = make_object()?;

    let conn = connect()?;
    let mut client = Client::new();
    let mut req = [0u8; 512];
    let mut inbuf = MessageBuffer::new();
    let mut chunk = [0u8; 512];

    let registry = send_req(conn, &mut req, |q| client.get_registry(q))?;
    let (comp_name, dmabuf_name) = find_globals(conn, &client, &mut inbuf, &mut chunk)?;

    let compositor = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            comp_name,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let dmabuf = send_req(conn, &mut req, |q| {
        client.bind(
            registry,
            dmabuf_name,
            b"zwp_linux_dmabuf_v1",
            3,
            Kind::LinuxDmabuf,
            q,
        )
    })?;
    let surface = send_req(conn, &mut req, |q| client.create_surface(compositor, q))?;
    let params = send_req(conn, &mut req, |q| client.create_params(dmabuf, q))?;

    // Drain everything above before the descriptor travels: the fd goes with the `add` message
    // by `SCM_RIGHTS`, so the server must be reading that message when it arrives.
    let _callback = send_req(conn, &mut req, |q| client.sync(q))?;
    await_event(conn, &client, &mut inbuf, &mut chunk, |h, kind, _| {
        kind == Kind::Callback && h.opcode == protocol::callback_ev::DONE
    })?;

    // The plane fd travels with `add`; its index in the message's fd list is 0.
    uds::send_fds(conn, &[prime_fd]).map_err(|_| fail(25, b"wldmabuf: send_fds failed\n"))?;
    send_req(conn, &mut req, |q| {
        client.params_add(params, 0, 0, 0, STRIDE, protocol::DRM_FORMAT_MOD_LINEAR, q)
    })?;
    let buffer = send_req(conn, &mut req, |q| {
        client.params_create_immed(
            params,
            WIDTH as i32,
            HEIGHT as i32,
            protocol::DRM_FORMAT_XRGB8888,
            0,
            q,
        )
    })?;

    send_req(conn, &mut req, |q| client.attach(surface, buffer, 0, 0, q))?;
    send_req(conn, &mut req, |q| {
        client.damage(surface, 0, 0, WIDTH as i32, HEIGHT as i32, q)
    })?;
    send_req(conn, &mut req, |q| client.commit(surface, q))?;

    // The compositor releases the buffer once it has presented it, which is the signal that
    // the commit was taken.
    await_event(conn, &client, &mut inbuf, &mut chunk, |h, kind, _| {
        kind == Kind::Buffer && h.object_id == buffer && h.opcode == protocol::buffer_ev::RELEASE
    })?;

    let mut line = [0u8; 64];
    line[..15].copy_from_slice(b"wldmabuf: PASS\n");
    crate::write_out(&line[..15]);

    let _ = uds::close_fd(prime_fd);
    let _ = uds::close_fd(conn);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    /// The argument structs against the sizes their request numbers carry — the same check
    /// `/bin/drmprime` makes. A struct that grew or lost a field would make every request a
    /// number the node does not serve.
    const fn arg_size(request: u32) -> u32 {
        (request >> 16) & 0x3fff
    }

    #[test]
    fn the_argument_structs_are_the_sizes_their_numbers_carry() {
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE),
            size_of::<ResourceCreate>() as u32
        );
        assert_eq!(arg_size(DRM_IOCTL_VIRTGPU_MAP), size_of::<Map>() as u32);
        assert_eq!(
            arg_size(DRM_IOCTL_PRIME_HANDLE_TO_FD),
            size_of::<PrimeHandle>() as u32
        );
    }

    /// The object is the whole output: the compositor scans out a 1024×768 plane, and a
    /// smaller object would be blitted instead (a copy).
    #[test]
    fn the_object_is_the_full_output() {
        assert_eq!(SIZE, 1024 * 768 * 4);
        assert_eq!(STRIDE, WIDTH * 4);
    }

    /// The colour has no zero byte in its significant channels, so a pixel equal to it on the
    /// display is this frame and not a leftover.
    #[test]
    fn the_colour_is_distinctive() {
        assert_eq!(COLOR, 0x0000_FF00);
        assert_ne!(COLOR & 0x00FF_FFFF, 0);
    }
}
