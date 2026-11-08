//! The GLES backend: surfaceless EGL, a pbuffer, and the solid-rect program.
//!
//! This is the translation of `tools/wlgl_render.c` into the [`Backend`] shape,
//! with the triangle replaced by the rect and clear elements of the pass list.
//! EGL is surfaceless because there is no window system underneath us: the
//! compositor draws into its own buffer and presents that, exactly as the probe
//! does (`tools/wlgl_render.c` is the reference, `.notes/wayland.md` §6.10
//! stage 3d-a).
//!
//! The module is minix-only: it links `libEGL.so.1` and `libGLESv2.so.2`, which
//! exist only for the minix target. Everything with a decision in it lives in
//! the host-testable modules around it, so this file stays a mapping from the
//! pass to GL calls.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_int, c_void};
use core::ptr;

use crate::backend::Backend;
use crate::geom::Rect;
use crate::pass::{Color, Element, Pass};
use crate::pixels::rgba_bottom_up_to_xrgb8888;

type EglDisplay = *mut c_void;
type EglConfig = *mut c_void;
type EglContext = *mut c_void;
type EglSurface = *mut c_void;
type Eglint = i32;
type Gluint = u32;
type Glint = i32;
type Glenum = u32;
type Glsizei = i32;
type Glboolean = u8;

const EGL_OPENGL_ES_API: Eglint = 0x30A0;
const EGL_SURFACE_TYPE: Eglint = 0x3033;
const EGL_PBUFFER_BIT: Eglint = 0x0001;
const EGL_RENDERABLE_TYPE: Eglint = 0x3040;
const EGL_OPENGL_ES3_BIT: Eglint = 0x0040;
const EGL_RED_SIZE: Eglint = 0x3024;
const EGL_GREEN_SIZE: Eglint = 0x3023;
const EGL_BLUE_SIZE: Eglint = 0x3022;
const EGL_ALPHA_SIZE: Eglint = 0x3021;
const EGL_NONE: Eglint = 0x3038;
const EGL_CONTEXT_CLIENT_VERSION: Eglint = 0x3098;
const EGL_WIDTH: Eglint = 0x3057;
const EGL_HEIGHT: Eglint = 0x3056;

const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
const GL_RGBA: Glenum = 0x1908;
const GL_UNSIGNED_BYTE: Glenum = 0x1401;
const GL_FLOAT: Glenum = 0x1406;
const GL_VERTEX_SHADER: Glenum = 0x8B31;
const GL_FRAGMENT_SHADER: Glenum = 0x8B30;
const GL_COMPILE_STATUS: Glenum = 0x8B81;
const GL_LINK_STATUS: Glenum = 0x8B82;
const GL_ARRAY_BUFFER: Glenum = 0x8892;
const GL_STREAM_DRAW: Glenum = 0x88E0;
const GL_TRIANGLE_STRIP: Glenum = 0x0005;
const GL_BLEND: Glenum = 0x0BE2;
const GL_SRC_ALPHA: Glenum = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: Glenum = 0x0303;
const GL_SCISSOR_TEST: Glenum = 0x0C11;
const GL_RENDERER: Glenum = 0x1F01;

const GL_FALSE: Glboolean = 0;

const VERTEX_SRC: &[u8] = b"#version 300 es\n\
layout(location = 0) in vec2 a_pos;\n\
void main() { gl_Position = vec4(a_pos, 0.0, 1.0); }\n\0";

const FRAGMENT_SRC: &[u8] = b"#version 300 es\n\
precision mediump float;\n\
uniform vec4 u_color;\n\
out vec4 fragColor;\n\
void main() { fragColor = u_color; }\n\0";

unsafe extern "C" {
    fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;

    fn eglGetDisplay(display_id: *mut c_void) -> EglDisplay;
    fn eglInitialize(display: EglDisplay, major: *mut Eglint, minor: *mut Eglint) -> u32;
    fn eglBindAPI(api: u32) -> u32;
    fn eglChooseConfig(
        display: EglDisplay,
        attributes: *const Eglint,
        configs: *mut EglConfig,
        config_size: Eglint,
        num_config: *mut Eglint,
    ) -> u32;
    fn eglCreateContext(
        display: EglDisplay,
        config: EglConfig,
        share: EglContext,
        attributes: *const Eglint,
    ) -> EglContext;
    fn eglCreatePbufferSurface(
        display: EglDisplay,
        config: EglConfig,
        attributes: *const Eglint,
    ) -> EglSurface;
    fn eglMakeCurrent(
        display: EglDisplay,
        draw: EglSurface,
        read: EglSurface,
        context: EglContext,
    ) -> u32;
    fn eglDestroySurface(display: EglDisplay, surface: EglSurface) -> u32;
    fn eglDestroyContext(display: EglDisplay, context: EglContext) -> u32;
    fn eglTerminate(display: EglDisplay) -> u32;

    fn glViewport(x: Glint, y: Glint, width: Glsizei, height: Glsizei);
    fn glScissor(x: Glint, y: Glint, width: Glsizei, height: Glsizei);
    fn glEnable(capability: Glenum);
    fn glDisable(capability: Glenum);
    fn glBlendFunc(source: Glenum, destination: Glenum);
    fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32);
    fn glClear(mask: u32);
    fn glGetError() -> Glenum;
    fn glFinish();
    fn glReadPixels(
        x: Glint,
        y: Glint,
        width: Glsizei,
        height: Glsizei,
        format: Glenum,
        kind: Glenum,
        pixels: *mut c_void,
    );
    fn glGetString(name: Glenum) -> *const c_char;

    fn glCreateShader(kind: Glenum) -> Gluint;
    fn glShaderSource(
        shader: Gluint,
        count: Glsizei,
        source: *const *const c_char,
        length: *const Glint,
    );
    fn glCompileShader(shader: Gluint);
    fn glGetShaderiv(shader: Gluint, name: Glenum, value: *mut Glint);
    fn glGetShaderInfoLog(shader: Gluint, size: Glsizei, length: *mut Glsizei, log: *mut c_char);
    fn glDeleteShader(shader: Gluint);

    fn glCreateProgram() -> Gluint;
    fn glAttachShader(program: Gluint, shader: Gluint);
    fn glLinkProgram(program: Gluint);
    fn glGetProgramiv(program: Gluint, name: Glenum, value: *mut Glint);
    fn glGetProgramInfoLog(program: Gluint, size: Glsizei, length: *mut Glsizei, log: *mut c_char);
    fn glUseProgram(program: Gluint);
    fn glDeleteProgram(program: Gluint);
    fn glGetUniformLocation(program: Gluint, name: *const c_char) -> Glint;
    fn glUniform4f(location: Glint, x: f32, y: f32, z: f32, w: f32);

    fn glGenBuffers(count: Glsizei, buffers: *mut Gluint);
    fn glBindBuffer(target: Glenum, buffer: Gluint);
    fn glBufferData(target: Glenum, size: isize, data: *const c_void, usage: Glenum);
    fn glDeleteBuffers(count: Glsizei, buffers: *const Gluint);
    fn glEnableVertexAttribArray(index: Gluint);
    fn glVertexAttribPointer(
        index: Gluint,
        size: Glint,
        kind: Glenum,
        normalized: Glboolean,
        stride: Glsizei,
        pointer: *const c_void,
    );
    fn glDrawArrays(mode: Glenum, first: Glint, count: Glsizei);
}

/// Why a render call failed.
#[derive(Debug)]
pub enum GlError {
    /// An EGL entry point named by the string failed.
    Egl(&'static str),
    Shader(&'static str, String),
    Program(String),
    /// A damage rectangle did not intersect the output.
    Scissor,
    /// The pass was built for a different output size.
    OutputMismatch,
    /// The destination buffer is too small.
    OutputSize,
    /// The element kind has no path in this backend yet.
    Unsupported,
    /// `glGetError` returned nonzero.
    Gl(Glenum),
    /// A shader or program could not be created.
    Object,
}

impl core::fmt::Display for GlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Egl(call) => write!(f, "EGL failed in {call}"),
            Self::Shader(stage, log) => write!(f, "{stage} shader did not compile: {log}"),
            Self::Program(log) => write!(f, "program did not link: {log}"),
            Self::Scissor => f.write_str("damage rectangle is outside the output"),
            Self::OutputMismatch => f.write_str("pass output size differs from the renderer's"),
            Self::OutputSize => f.write_str("destination buffer is too small"),
            Self::Unsupported => f.write_str("element kind is not supported by the GL backend"),
            Self::Gl(code) => write!(f, "glGetError returned 0x{code:04x}"),
            Self::Object => f.write_str("GL refused to create an object"),
        }
    }
}

/// A surfaceless GLES renderer with one pbuffer of the output size.
pub struct Gl {
    display: EglDisplay,
    context: EglContext,
    surface: EglSurface,
    program: Gluint,
    vbo: Gluint,
    color: Glint,
    output: Rect,
    readback: Vec<u8>,
}

impl Gl {
    /// Create a context and a pbuffer of `width` by `height`.
    ///
    /// A failure part way through setup leaves the EGL objects to process exit:
    /// there is no frame yet whose resources could be lost, and unwinding them
    /// here would add an error path longer than the setup itself.
    pub fn new(width: i32, height: i32) -> Result<Self, GlError> {
        if width <= 0 || height <= 0 {
            return Err(GlError::OutputSize);
        }
        let output = Rect::new(0, 0, width, height);
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .and_then(|n| n.checked_mul(4))
            .ok_or(GlError::OutputSize)?;

        unsafe {
            setenv(c"EGL_PLATFORM".as_ptr(), c"surfaceless".as_ptr(), 1);

            let display = eglGetDisplay(ptr::null_mut());
            if display.is_null() {
                return Err(GlError::Egl("eglGetDisplay"));
            }
            let mut major = 0;
            let mut minor = 0;
            if eglInitialize(display, &mut major, &mut minor) == 0 {
                return Err(GlError::Egl("eglInitialize"));
            }
            if eglBindAPI(EGL_OPENGL_ES_API as u32) == 0 {
                return Err(GlError::Egl("eglBindAPI"));
            }

            let config_attributes = [
                EGL_SURFACE_TYPE,
                EGL_PBUFFER_BIT,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES3_BIT,
                EGL_RED_SIZE,
                8,
                EGL_GREEN_SIZE,
                8,
                EGL_BLUE_SIZE,
                8,
                EGL_ALPHA_SIZE,
                8,
                EGL_NONE,
            ];
            let mut config: EglConfig = ptr::null_mut();
            let mut configs = 0;
            if eglChooseConfig(
                display,
                config_attributes.as_ptr(),
                &mut config,
                1,
                &mut configs,
            ) == 0
                || configs < 1
            {
                return Err(GlError::Egl("eglChooseConfig"));
            }

            let context_attributes = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
            let context = eglCreateContext(
                display,
                config,
                ptr::null_mut(),
                context_attributes.as_ptr(),
            );
            if context.is_null() {
                return Err(GlError::Egl("eglCreateContext"));
            }

            let surface_attributes = [EGL_WIDTH, width, EGL_HEIGHT, height, EGL_NONE];
            let surface = eglCreatePbufferSurface(display, config, surface_attributes.as_ptr());
            if surface.is_null() {
                return Err(GlError::Egl("eglCreatePbufferSurface"));
            }

            if eglMakeCurrent(display, surface, surface, context) == 0 {
                return Err(GlError::Egl("eglMakeCurrent"));
            }

            let vertex = compile(GL_VERTEX_SHADER, VERTEX_SRC, "vertex")?;
            let fragment = compile(GL_FRAGMENT_SHADER, FRAGMENT_SRC, "fragment")?;
            let program = glCreateProgram();
            if program == 0 {
                return Err(GlError::Object);
            }
            glAttachShader(program, vertex);
            glAttachShader(program, fragment);
            glLinkProgram(program);
            glDeleteShader(vertex);
            glDeleteShader(fragment);

            let mut linked = 0;
            glGetProgramiv(program, GL_LINK_STATUS, &mut linked);
            if linked == 0 {
                return Err(GlError::Program(object_log(program, false)));
            }

            let color = glGetUniformLocation(program, c"u_color".as_ptr());
            if color < 0 {
                return Err(GlError::Object);
            }

            let mut vbo = 0;
            glGenBuffers(1, &mut vbo);
            if vbo == 0 {
                return Err(GlError::Object);
            }

            let error = glGetError();
            if error != 0 {
                return Err(GlError::Gl(error));
            }

            Ok(Self {
                display,
                context,
                surface,
                program,
                vbo,
                color,
                output,
                readback: vec![0; pixels],
            })
        }
    }

    pub fn output(&self) -> Rect {
        self.output
    }

    /// The `GL_RENDERER` string, which names the driver the frame came from.
    pub fn renderer_name(&self) -> Option<&'static str> {
        let name = unsafe { glGetString(GL_RENDERER) };
        if name.is_null() {
            return None;
        }
        // GL strings live for the context's lifetime, which is this object's.
        unsafe { CStr::from_ptr(name) }.to_str().ok()
    }

    /// Draw `pass` into the pbuffer.
    pub fn render(&mut self, pass: &Pass) -> Result<(), GlError> {
        if pass.output() != self.output {
            return Err(GlError::OutputMismatch);
        }

        unsafe {
            glViewport(0, 0, self.output.w, self.output.h);
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glDisable(GL_SCISSOR_TEST);
        }

        pass.submit(self)?;

        unsafe {
            glFinish();
            let error = glGetError();
            if error != 0 {
                return Err(GlError::Gl(error));
            }
        }
        Ok(())
    }

    /// Copy the pbuffer into `dst` as top-down `XRGB8888`.
    pub fn read_into(&mut self, dst: &mut [u8]) -> Result<(), GlError> {
        unsafe {
            glReadPixels(
                0,
                0,
                self.output.w,
                self.output.h,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                self.readback.as_mut_ptr().cast(),
            );
        }
        if !rgba_bottom_up_to_xrgb8888(
            &self.readback,
            dst,
            self.output.w as usize,
            self.output.h as usize,
        ) {
            return Err(GlError::OutputSize);
        }
        Ok(())
    }

    unsafe fn draw_rect(&mut self, rect: Rect, color: Color) -> Result<(), GlError> {
        let Some(rect) = rect.intersection(self.output) else {
            return Ok(());
        };

        let width = self.output.w as f32;
        let height = self.output.h as f32;
        let x0 = 2.0 * rect.x as f32 / width - 1.0;
        let x1 = 2.0 * rect.right() as f32 / width - 1.0;
        let y0 = 1.0 - 2.0 * rect.y as f32 / height;
        let y1 = 1.0 - 2.0 * rect.bottom() as f32 / height;

        let vertices: [f32; 8] = [x0, y0, x1, y0, x0, y1, x1, y1];

        unsafe {
            glUseProgram(self.program);
            glUniform4f(
                self.color,
                f32::from(color.r) / 255.0,
                f32::from(color.g) / 255.0,
                f32::from(color.b) / 255.0,
                f32::from(color.a) / 255.0,
            );
            glBindBuffer(GL_ARRAY_BUFFER, self.vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                core::mem::size_of_val(&vertices) as isize,
                vertices.as_ptr().cast(),
                GL_STREAM_DRAW,
            );
            glEnableVertexAttribArray(0);
            glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, ptr::null());
            glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        }
        Ok(())
    }
}

impl Backend for Gl {
    type Error = GlError;

    fn draw(&mut self, element: &Element, scissor: Rect) -> Result<(), Self::Error> {
        let scissor = scissor.intersection(self.output).ok_or(GlError::Scissor)?;

        // GL's scissor origin is the bottom-left corner.
        let y = self.output.h - scissor.bottom();
        unsafe {
            glEnable(GL_SCISSOR_TEST);
            glScissor(scissor.x, y, scissor.w, scissor.h);

            match *element {
                Element::Clear { color } if color.is_opaque() => {
                    glClearColor(
                        f32::from(color.r) / 255.0,
                        f32::from(color.g) / 255.0,
                        f32::from(color.b) / 255.0,
                        1.0,
                    );
                    glClear(GL_COLOR_BUFFER_BIT);
                }
                Element::Clear { color } => self.draw_rect(self.output, color)?,
                Element::Rect { rect, color } => self.draw_rect(rect, color)?,
                Element::Texture { .. } => return Err(GlError::Unsupported),
            }
        }
        Ok(())
    }
}

impl Drop for Gl {
    fn drop(&mut self) {
        // A drop cannot report an EGL failure anywhere useful, and the process is
        // usually exiting; the calls are still made so a compositor that replaces
        // its context does not leak the old one.
        unsafe {
            glDeleteBuffers(1, &self.vbo);
            glDeleteProgram(self.program);
            eglMakeCurrent(
                self.display,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            eglDestroySurface(self.display, self.surface);
            eglDestroyContext(self.display, self.context);
            eglTerminate(self.display);
        }
    }
}

unsafe fn compile(
    kind: Glenum,
    source: &'static [u8],
    stage: &'static str,
) -> Result<Gluint, GlError> {
    unsafe {
        let shader = glCreateShader(kind);
        if shader == 0 {
            return Err(GlError::Object);
        }
        let source_pointer = source.as_ptr().cast::<c_char>();
        glShaderSource(shader, 1, &source_pointer, ptr::null());
        glCompileShader(shader);

        let mut compiled = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &mut compiled);
        if compiled == 0 {
            let log = object_log(shader, true);
            glDeleteShader(shader);
            return Err(GlError::Shader(stage, log));
        }
        Ok(shader)
    }
}

unsafe fn object_log(object: Gluint, shader: bool) -> String {
    let mut buffer = [0u8; 512];
    let mut written: Glsizei = 0;
    unsafe {
        if shader {
            glGetShaderInfoLog(
                object,
                buffer.len() as Glsizei,
                &mut written,
                buffer.as_mut_ptr().cast(),
            );
        } else {
            glGetProgramInfoLog(
                object,
                buffer.len() as Glsizei,
                &mut written,
                buffer.as_mut_ptr().cast(),
            );
        }
    }

    let end = usize::try_from(written)
        .ok()
        .filter(|end| *end <= buffer.len())
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).trim().to_string()
}
