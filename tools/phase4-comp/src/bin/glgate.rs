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

#[path = "../gl.rs"]
mod gl;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};

/// The offscreen target's side, and the region read back.
const SIDE: i32 = 64;

fn fail(message: &str) -> ! {
    println!("glgate: FAIL {message}");
    std::process::exit(1);
}

fn main() {
    let (bootstrap, context) = match gl::create_context(SIDE, SIDE) {
        Ok(pair) => pair,
        Err(e) => fail(&e),
    };
    println!(
        "glgate: egl display, config, GLES 3 context and pbuffer ok (EGL {}.{})",
        bootstrap.egl_major, bootstrap.egl_minor
    );
    // The driver that actually ran: a `PASS` on its own cannot tell virgl from software.
    println!("glgate: {}", gl::describe(&bootstrap));
    println!("glgate: smithay wrapped the display and the context");

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
