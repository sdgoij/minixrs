//! `/bin/wlscene` — the two-surface `zwp_linux_dmabuf_v1` client (`.notes/wayland.md` §6.10,
//! stage 3d-b-5c/5d).
//!
//! One connection commits **two** objects on the render node as `linux-dmabuf` buffers: a
//! full-output background (red) on one surface, and a smaller overlay (green) on another. The
//! compositor holds both in its scene, and once there is more than one surface it asks the GPU
//! composition helper (`/bin/wlcompose`) to draw them into one frame, which it scans out
//! (`FBIO_SCANOUT_SRC`). The gate reads the display from the host (a QMP screendump) and the
//! compositor's `wlserver: composed N surfaces` line.
//!
//! The two objects are made and filled the way `/bin/wldmabuf` makes its single one — a render
//! node object, mapped and written, then exported to a dma-buf descriptor
//! (`PRIME_HANDLE_TO_FD`) and handed over as a `zwp_linux_dmabuf_v1` buffer. Like `/bin/drmprime`
//! and `/bin/wldmabuf` it declares the DRM ABI itself rather than importing the driver's, so its
//! agreement with the node is evidence and not a tautology.

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

/// The overlay's size. Its position is the compositor's to choose, and the port places every
/// surface at the output's origin (2e accepts anchors and geometry and ignores them), so the
/// overlay is the top-left square over the background.
const OVERLAY: u32 = 128;

/// The render node's 2D format, `VIRTIO_GPU_FORMAT_B8G8R8X8` — the same byte order as the
/// `DRM_FORMAT_XRGB8888` the linux-dmabuf buffer is created with.
const FORMAT_B8G8R8X8: u32 = 2;
const TARGET_TEXTURE_2D: u32 = 2;
/// `VIRGL_BIND_RENDER_TARGET | VIRGL_BIND_SAMPLER_VIEW`, what `/bin/wldmabuf` uses.
const BIND_TEXTURE: u32 = (1 << 1) | (1 << 3);

/// The two fills, as the memory holds them: B8G8R8X8 little-endian, bytes B,G,R,X.
const BG_COLOR: u32 = 0x00FF_0000; // red
const FG_COLOR: u32 = 0x0000_FF00; // green

const POLLIN: i16 = 0x001;

/// `_IOWR` from `asm-generic/ioctl.h`, and the render node's request numbers.
const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    (3u32 << 30) | (type_ << 8) | (size << 16) | nr
}
const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_IOCTL_PRIME_HANDLE_TO_FD: u32 = iowr(DRM_IOCTL_BASE, 0x2d, 12);
const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x01, 16);
const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x04, 56);
const DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST: u32 = iowr(DRM_IOCTL_BASE, 0x40 + 0x07, 44);

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

/// `struct drm_virtgpu_3d_transfer_to_host` — the object and a box, 44 bytes. A whole-object
/// transfer leaves every field but the resource and the box zero.
#[repr(C)]
struct Transfer {
    resource_id: u32,
    x: u32,
    y: u32,
    z: u32,
    w: u32,
    h: u32,
    d: u32,
    level: u32,
    offset: u32,
    stride: u32,
    layer_stride: u32,
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

/// One object a client hands the compositor: the descriptor, and its geometry.
struct Plane {
    fd: i32,
    w: u32,
    h: u32,
}

/// The server connection and the buffers one request/reply round trip needs.
struct Conn {
    fd: i32,
    client: Client,
    req: [u8; 512],
    inbuf: MessageBuffer,
    chunk: [u8; 512],
}

impl Conn {
    fn new(fd: i32) -> Self {
        Self {
            fd,
            client: Client::new(),
            req: [0u8; 512],
            inbuf: MessageBuffer::new(),
            chunk: [0u8; 512],
        }
    }

    /// Build one request with `f` and send it. The closure gets the client and the buffer, so a
    /// request method can take `&mut Client` without the caller borrowing `self` twice.
    fn send<R, F>(&mut self, f: F) -> Result<R, i32>
    where
        F: FnOnce(&mut Client, &mut DispatchBuf<'_>) -> Result<R, WireError>,
    {
        let mut q = DispatchBuf::new(&mut self.req);
        let r = f(&mut self.client, &mut q).map_err(|_| fail(2, b"wlscene: encode failed\n"))?;
        match uds::send(self.fd, q.bytes()) {
            Ok(n) if n as usize == q.len() => Ok(r),
            _ => Err(fail(3, b"wlscene: send failed\n")),
        }
    }

    /// Poll the socket for readability, then read one chunk into the inbound queue.
    fn fill(&mut self, timeout_ms: i32) -> bool {
        let mut pf = [PollFd {
            fd: self.fd,
            events: POLLIN,
            revents: 0,
        }];
        match fs::poll(&mut pf, timeout_ms) {
            Ok(n) if n > 0 && pf[0].revents & POLLIN != 0 => {}
            _ => return false,
        }
        match uds::recv(self.fd, &mut self.chunk) {
            Ok(n) if n > 0 => self.inbuf.push(&self.chunk[..n as usize]).is_ok(),
            _ => false,
        }
    }

    /// Read registry events until both globals this client needs are known.
    fn find_globals(&mut self) -> Result<(u32, u32), i32> {
        let (mut comp, mut dmabuf) = (None, None);
        for _ in 0..2000 {
            loop {
                let msg = match self.inbuf.next() {
                    Ok(Some(m)) => m,
                    Ok(None) => break,
                    Err(_) => return Err(fail(6, b"wlscene: wire error\n")),
                };
                let h = Header::parse(msg).map_err(|_| fail(7, b"wlscene: bad header\n"))?;
                let mut args = [Arg::Uint(0); MAX_ARGS];
                let (_, kind, n) = self
                    .client
                    .decode(msg, &mut args)
                    .map_err(|_| fail(8, b"wlscene: decode failed\n"))?;
                if kind == Kind::Registry && h.opcode == protocol::registry_ev::GLOBAL && n == 3 {
                    let name = args[0].as_uint().unwrap_or(0);
                    match args[1].as_str().unwrap_or(b"") {
                        b"wl_compositor" => comp = Some(name),
                        b"zwp_linux_dmabuf_v1" => dmabuf = Some(name),
                        _ => {}
                    }
                }
                self.inbuf.consume(h.size as usize);
            }
            if let (Some(c), Some(d)) = (comp, dmabuf) {
                return Ok((c, d));
            }
            if !self.fill(2000) {
                break;
            }
        }
        Err(fail(
            9,
            b"wlscene: the compositor or the dmabuf global is missing\n",
        ))
    }

    /// Read until `done` accepts an event, or nothing arrives for two minutes. The wait is
    /// patient on purpose: the compositor's loop is busy while the helper draws a multi-surface
    /// frame, so its reply can take seconds and a single quiet poll is not the connection going
    /// away.
    fn await_event<F>(&mut self, mut done: F) -> Result<(), i32>
    where
        F: FnMut(&Header, Kind, &[Arg<'_>]) -> bool,
    {
        let mut waited = 0;
        loop {
            loop {
                let msg = match self.inbuf.next() {
                    Ok(Some(m)) => m,
                    Ok(None) => break,
                    Err(_) => return Err(fail(10, b"wlscene: wire error\n")),
                };
                let h = Header::parse(msg).map_err(|_| fail(11, b"wlscene: bad header\n"))?;
                let mut args = [Arg::Uint(0); MAX_ARGS];
                let (_, kind, n) = self
                    .client
                    .decode(msg, &mut args)
                    .map_err(|_| fail(12, b"wlscene: decode failed\n"))?;
                let hit = done(&h, kind, &args[..n]);
                self.inbuf.consume(h.size as usize);
                if hit {
                    return Ok(());
                }
            }
            if self.fill(250) {
                waited = 0;
            } else {
                waited += 250;
                if waited >= 120_000 {
                    return Err(fail(13, b"wlscene: the server went quiet\n"));
                }
            }
        }
    }

    /// Hand one already-exported object to the server as a `linux-dmabuf` buffer on `surface` and
    /// commit it. Returns the buffer object id.
    fn commit(&mut self, dmabuf: u32, surface: u32, plane: &Plane) -> Result<u32, i32> {
        let params = self.send(|cl, q| cl.create_params(dmabuf, q))?;

        // Drain everything above before the descriptor travels: the fd goes with the `add`
        // message by `SCM_RIGHTS`, so the server must be reading that message when it arrives.
        let _ = self.send(|cl, q| cl.sync(q))?;
        self.await_event(|h, kind, _| {
            kind == Kind::Callback && h.opcode == protocol::callback_ev::DONE
        })?;

        uds::send_fds(self.fd, &[plane.fd]).map_err(|_| fail(25, b"wlscene: send_fds failed\n"))?;
        self.send(|cl, q| {
            cl.params_add(
                params,
                0,
                0,
                0,
                plane.w * 4,
                protocol::DRM_FORMAT_MOD_LINEAR,
                q,
            )
        })?;
        let buffer = self.send(|cl, q| {
            cl.params_create_immed(
                params,
                plane.w as i32,
                plane.h as i32,
                protocol::DRM_FORMAT_XRGB8888,
                0,
                q,
            )
        })?;

        self.send(|cl, q| cl.attach(surface, buffer, 0, 0, q))?;
        self.send(|cl, q| cl.damage(surface, 0, 0, plane.w as i32, plane.h as i32, q))?;
        self.send(|cl, q| cl.commit(surface, q))?;
        Ok(buffer)
    }
}

/// Connect to the Wayland server, retrying while it is still coming up.
fn connect() -> Result<i32, i32> {
    let fd = uds::socket().map_err(|_| fail(4, b"wlscene: socket failed\n"))?;
    for _ in 0..100_000 {
        if uds::connect(fd, SOCK_PATH).is_ok() {
            return Ok(fd);
        }
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
    let _ = uds::close_fd(fd);
    Err(fail(5, b"wlscene: connect failed\n"))
}

/// Make a `w`x`h` object on the render node, fill it with `color`, and export it to a descriptor.
fn make_object(w: u32, h: u32, color: u32) -> Result<Plane, i32> {
    let fd = match unsafe { fs::open(NODE, fs::O_RDWR, 0) } {
        Ok(fd) => fd,
        Err(_) => {
            return Err(fail(
                20,
                b"wlscene: no render node at /dev/dri/renderD128\n",
            ));
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
        fd,
        DRM_IOCTL_VIRTGPU_RESOURCE_CREATE,
        core::ptr::addr_of_mut!(create) as *mut u8,
    )
    .is_err()
    {
        return Err(fail(21, b"wlscene: RESOURCE_CREATE failed\n"));
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
        return Err(fail(22, b"wlscene: VIRTGPU_MAP failed\n"));
    }

    let va = unsafe {
        vmem::mmap(
            core::ptr::null_mut(),
            size,
            vmem::PROT_READ | vmem::PROT_WRITE,
            vmem::MAP_SHARED,
            fd,
            map.offset as i64,
        )
    };
    if va == vmem::MAP_FAILED {
        return Err(fail(23, b"wlscene: mmap of the object failed\n"));
    }
    // The fill, one pixel at a time. A partly-written object is still an object, so the fill is
    // checked by the screendump and not by the write's return.
    let px = va.cast::<u32>();
    for i in 0..(w * h) as usize {
        unsafe { core::ptr::write_volatile(px.add(i), color) };
    }
    // Read the fill back, so an object that did not take it is caught here rather than as a frame
    // with the wrong colours.
    if unsafe { core::ptr::read_volatile(px) } != color {
        return Err(fail(26, b"wlscene: the object did not take its fill\n"));
    }
    let _ = unsafe { vmem::munmap(va, size) };

    // Push the fill to the host. On a virgl device a `RESOURCE_CREATE` object is a *host-side*
    // texture as far as the GPU is concerned: the pages written here hold the fill, but the
    // compositor's helper samples the texture, so a surface the host has not been given is
    // composed black. This is a static client's half of §5 row 26's client-side transfer, where a
    // GL client renders into the texture instead.
    let mut xfer = Transfer {
        resource_id: create.bo_handle,
        x: 0,
        y: 0,
        z: 0,
        w,
        h,
        d: 1,
        level: 0,
        offset: 0,
        stride: 0,
        layer_stride: 0,
    };
    if ioctl(
        fd,
        DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST,
        core::ptr::addr_of_mut!(xfer) as *mut u8,
    )
    .is_err()
    {
        return Err(fail(27, b"wlscene: TRANSFER_TO_HOST failed\n"));
    }

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
        return Err(fail(24, b"wlscene: PRIME_HANDLE_TO_FD failed\n"));
    }

    let _ = fs::close(fd);
    Ok(Plane {
        fd: export.fd,
        w,
        h,
    })
}

/// The client's whole session: two objects committed on one connection, a background over the
/// whole output and a smaller overlay on top of it, and the compositor re-composing the frame.
pub fn wlscene(_args: &[&str]) -> i32 {
    match run() {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn run() -> Result<(), i32> {
    let bg = make_object(WIDTH, HEIGHT, BG_COLOR)?;
    let fg = make_object(OVERLAY, OVERLAY, FG_COLOR)?;

    let mut c = Conn::new(connect()?);

    let registry = c.send(|cl, q| cl.get_registry(q))?;
    let (comp_name, dmabuf_name) = c.find_globals()?;

    let compositor = c.send(|cl, q| {
        cl.bind(
            registry,
            comp_name,
            b"wl_compositor",
            1,
            Kind::Compositor,
            q,
        )
    })?;
    let dmabuf = c.send(|cl, q| {
        cl.bind(
            registry,
            dmabuf_name,
            b"zwp_linux_dmabuf_v1",
            3,
            Kind::LinuxDmabuf,
            q,
        )
    })?;
    let bg_surface = c.send(|cl, q| cl.create_surface(compositor, q))?;
    let fg_surface = c.send(|cl, q| cl.create_surface(compositor, q))?;

    // The background first, so it is under the overlay in the scene, and so the first commit
    // takes the single-surface fast path before the second makes the frame a composition. Its
    // release is awaited before the second commit: a `sync` in that commit drains the inbound
    // queue, and an unrelated event already in it would be lost.
    let bg_buffer = c.commit(dmabuf, bg_surface, &bg)?;
    if c.await_event(|h, kind, _| {
        kind == Kind::Buffer && h.opcode == protocol::buffer_ev::RELEASE && h.object_id == bg_buffer
    })
    .is_err()
    {
        return Err(fail(30, b"wlscene: the background was never released\n"));
    }

    // The overlay on top of it: with two surfaces the compositor asks the helper to compose.
    let fg_buffer = c.commit(dmabuf, fg_surface, &fg)?;
    if c.await_event(|h, kind, _| {
        kind == Kind::Buffer && h.opcode == protocol::buffer_ev::RELEASE && h.object_id == fg_buffer
    })
    .is_err()
    {
        return Err(fail(31, b"wlscene: the overlay was never released\n"));
    }

    crate::write_out(b"wlscene: PASS\n");

    let _ = uds::close_fd(bg.fd);
    let _ = uds::close_fd(fg.fd);
    let _ = uds::close_fd(c.fd);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    /// The argument structs against the sizes their request numbers carry — the same check
    /// `/bin/drmprime` and `/bin/wldmabuf` make. A struct that grew or lost a field would make
    /// every request a number the node does not serve.
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
        assert_eq!(
            arg_size(DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST),
            size_of::<Transfer>() as u32
        );
    }

    /// The two fills differ in their significant channels, so a pixel equal to one on the display
    /// is not a leftover of the other, and neither is black (the compositor's clear).
    #[test]
    fn the_fills_are_distinct_and_opaque() {
        assert_eq!(BG_COLOR & 0x00FF_FFFF, 0x00FF_0000);
        assert_eq!(FG_COLOR & 0x00FF_FFFF, 0x0000_FF00);
    }
}
