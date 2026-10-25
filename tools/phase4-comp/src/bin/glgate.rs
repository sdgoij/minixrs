//! Phase 4d-1's gate: smithay's GLES renderer on the port.
//!
//! 4d composes with smithay's `GlesRenderer` (`.notes/wayland.md` 6.13, D17), drawing into an
//! offscreen target and reading the frame back (D18 — the shape 3d-b-5 measured). This binary
//! does exactly that, and each step prints what it found, so a failure names the layer: the EGL
//! context, the renderer, the framebuffer, or the read-back.
//!
//! The EGL context comes from `egl_bootstrap.c` rather than from smithay, for a measured
//! reason: smithay's configless `EGLContext::new` pins `EGL_CONTEXT_CLIENT_VERSION = 2`, its
//! config'd path asks a surfaceless display for `WINDOW_BIT` configs (`NoAvailablePixelFormat`)
//! and its device-display path asks for `STREAM_BIT_KHR` (`BadAttribute`) — while `ExportMem`,
//! which is how a frame is read back, needs GLES 3 (`glReadBuffer`, `PIXEL_PACK_BUFFER`). So the
//! context is created in C with the port's proven surfaceless recipe and wrapped by
//! `EGLContext::from_raw`.
//!
//! It is a *dynamically linked* program on purpose, and it must take its C ABI symbols from
//! `libc.so` rather than from the `minix-libc` rlib: `GlesRenderer` loads EGL with
//! `libloading`, i.e. `dlopen`, and the rlib is the *static* image's C library, whose
//! `dlopen` answers "this program is statically linked". `libxkbcommon` is linked
//! statically, because a crate's native libraries land inside rustc's `-Bstatic` window.

use core::ffi::c_void;

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::EGLContext;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};

/// The offscreen target's side, and the region read back.
const SIDE: i32 = 64;

/// The handles `egl_bootstrap.c` hands back.
#[repr(C)]
struct GlBootstrap {
    display: *mut c_void,
    config: *mut c_void,
    context: *mut c_void,
    surface: *mut c_void,
    /// `eglGetError()` from the step that failed.
    error: i32,
    /// Whether `eglGetPlatformDisplayEXT` was found, and what each display path answered.
    have_platform_display: i32,
    platform_error: i32,
    env_error: i32,
    egl_major: i32,
    egl_minor: i32,
    /// `glGetString` of the context: the vendor, driver and GL version that ran.
    gl_vendor: [core::ffi::c_char; 64],
    gl_renderer: [core::ffi::c_char; 64],
    gl_version: [core::ffi::c_char; 64],
}

unsafe extern "C" {
    /// Creates the display, config, GLES 3 context and pbuffer, and makes the context current.
    /// Returns 0, or a negative code naming the step that failed.
    fn glgate_egl_bootstrap(out: *mut GlBootstrap, width: i32, height: i32) -> i32;
}

fn fail(message: &str) -> ! {
    println!("glgate: FAIL {message}");
    std::process::exit(1);
}

/// Reads one of the NUL-terminated strings `egl_bootstrap.c` copied out of `glGetString`.
fn c_string(bytes: &[core::ffi::c_char; 64]) -> std::borrow::Cow<'_, str> {
    // SAFETY: the C side NUL-terminates within the 64 bytes it was given.
    unsafe { core::ffi::CStr::from_ptr(bytes.as_ptr()) }.to_string_lossy()
}

fn main() {
    let mut bootstrap = GlBootstrap {
        display: core::ptr::null_mut(),
        config: core::ptr::null_mut(),
        context: core::ptr::null_mut(),
        surface: core::ptr::null_mut(),
        error: 0,
        have_platform_display: 0,
        platform_error: 0,
        env_error: 0,
        egl_major: 0,
        egl_minor: 0,
        gl_vendor: [0; 64],
        gl_renderer: [0; 64],
        gl_version: [0; 64],
    };
    // SAFETY: the struct is writable for the call's duration and every handle it returns stays
    // valid for the process (nothing terminates the display or destroys the context).
    let step = unsafe { glgate_egl_bootstrap(&mut bootstrap, SIDE, SIDE) };
    if step != 0 {
        println!(
            "glgate: egl platform-extension={} platform-error=0x{:04x} env-error=0x{:04x}              version={}.{}",
            if bootstrap.have_platform_display != 0 {
                "found"
            } else {
                "missing"
            },
            bootstrap.platform_error,
            bootstrap.env_error,
            bootstrap.egl_major,
            bootstrap.egl_minor
        );
        fail(&format!(
            "EGL bootstrap failed at step {} (eglGetError 0x{:04x})",
            -step, bootstrap.error
        ));
    }
    println!(
        "glgate: egl display, config, GLES 3 context and pbuffer ok (EGL {}.{})",
        bootstrap.egl_major, bootstrap.egl_minor
    );
    // The driver that actually ran: a `PASS` on its own cannot tell virgl from llvmpipe.
    println!(
        "glgate: GL_VENDOR={} GL_RENDERER={} GL_VERSION={}",
        c_string(&bootstrap.gl_vendor),
        c_string(&bootstrap.gl_renderer),
        c_string(&bootstrap.gl_version),
    );

    // The display is wrapped through the context below (`EGLContext::from_raw` registers it),
    // so nothing else is taken from it here.
    //
    // SAFETY: the handles are the ones the bootstrap created, and it keeps them alive for the
    // whole process; nothing else creates an EGL display with these parameters.
    let context = match unsafe {
        EGLContext::from_raw(bootstrap.display, bootstrap.config, bootstrap.context)
    } {
        Ok(context) => context,
        Err(e) => fail(&format!("EGL context wrap failed: {e:?}")),
    };
    println!("glgate: smithay wrapped the display and the context");

    // What the GL actually is. (`glGetString` is resolved by the C bootstrap while the
    // context is current; smithay keeps its own entry points to itself, so a proc-address
    // lookup taken from here answers null.)
    // SAFETY: the context is not current in any other thread.
    match unsafe { GlesRenderer::supported_capabilities(&context) } {
        Ok(capabilities) => println!("glgate: capabilities={capabilities:?}"),
        Err(e) => fail(&format!("GL capabilities query failed: {e:?}")),
    }

    // SAFETY: the context is not current in any other thread.
    let mut renderer = match unsafe { GlesRenderer::new(context) } {
        Ok(renderer) => renderer,
        Err(e) => fail(&format!("GLES renderer failed: {e:?}")),
    };
    println!("glgate: GLES renderer ok");

    let buffer_size = Size::<i32, Buffer>::from((SIDE, SIDE));
    let buffer_region = Rectangle::from_size(buffer_size);
    let physical_size = Size::<i32, Physical>::from((SIDE, SIDE));
    let physical_region = Rectangle::from_size(physical_size);
    let mut target =
        match Offscreen::<GlesTexture>::create_buffer(&mut renderer, Fourcc::Abgr8888, buffer_size)
        {
            Ok(target) => target,
            Err(e) => fail(&format!("offscreen buffer failed: {e:?}")),
        };
    println!("glgate: offscreen target {SIDE}x{SIDE} Abgr8888 ok");

    match renderer.bind(&mut target) {
        Ok(mut bound) => {
            // `render` starts the frame on the bound framebuffer; `clear` and `finish` belong
            // to the frame. It finishes (and unbinds) before the copy below.
            let rects = [physical_region];
            match renderer.render(&mut bound, physical_size, Transform::Normal) {
                Ok(mut frame) => {
                    if let Err(e) = frame.clear(Color32F::new(0.0, 1.0, 0.0, 1.0), &rects) {
                        fail(&format!("clear failed: {e:?}"));
                    }
                    if let Err(e) = frame.finish() {
                        fail(&format!("frame finish failed: {e:?}"));
                    }
                }
                Err(e) => fail(&format!("render failed: {e:?}")),
            }
            println!("glgate: cleared the target to green");
        }
        Err(e) => fail(&format!("bind failed: {e:?}")),
    }

    let mapping = match renderer.copy_texture(&target, buffer_region, Fourcc::Abgr8888) {
        Ok(mapping) => mapping,
        Err(e) => fail(&format!("copy_texture failed: {e:?}")),
    };
    let bytes = match renderer.map_texture(&mapping) {
        Ok(bytes) => bytes,
        Err(e) => fail(&format!("map_texture failed: {e:?}")),
    };
    if bytes.len() < 4 {
        fail(&format!("read back {} bytes, wanted a pixel", bytes.len()));
    }
    // `Abgr8888` in memory on a little-endian machine is R, G, B, A — the order
    // `GL_RGBA`/`GL_UNSIGNED_BYTE` reads back (`Argb8888` maps to `GL_BGRA`, an extension
    // virgl does not expose).
    let (r, g, b, a) = (bytes[0], bytes[1], bytes[2], bytes[3]);
    println!("glgate: pixel r={r} g={g} b={b} a={a}");
    if r == 0 && g >= 250 && b == 0 && a == 255 {
        println!("glgate: PASS clear=green");
    } else {
        fail("the pixel read back is not the clear colour");
    }
}

/// `getrandom`'s custom backend (`--cfg getrandom_backend="custom"`).
///
/// The port has no entropy source yet, and nothing on 4d's path needs cryptographic
/// randomness: smithay's `rand` use is identifiers and cookies. A counter mixed with the
/// stack address and the cycle counter is enough for that, and it is *not* enough for
/// anything else — which is why it lives in a gate and not in a library.
mod entropy {
    use core::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn splitmix64(x: u64) -> u64 {
        let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn fill(dest: &mut [u8]) -> Result<(), getrandom::Error> {
        let stack = core::ptr::addr_of!(dest) as u64;
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        #[cfg(target_arch = "x86_64")]
        let cycles = unsafe { core::arch::x86_64::_rdtsc() };
        #[cfg(not(target_arch = "x86_64"))]
        let cycles = 0u64;
        let mut next = splitmix64(stack ^ counter.rotate_left(32) ^ cycles);
        for chunk in dest.chunks_mut(8) {
            next = splitmix64(next);
            for (i, byte) in chunk.iter_mut().enumerate() {
                *byte = (next >> (i * 8)) as u8;
            }
        }
        Ok(())
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "Rust" fn __getrandom_v03_custom(
        dest: *mut u8,
        len: usize,
    ) -> Result<(), getrandom::Error> {
        // SAFETY: the caller guarantees `dest` is writable for `len` bytes, which is the
        // contract this symbol's cfg selects.
        let dest = unsafe { core::slice::from_raw_parts_mut(dest, len) };
        fill(dest)
    }
}
