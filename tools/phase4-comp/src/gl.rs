//! The GL bootstrap 4d's binaries share: a surfaceless EGL display, a GLES 3 config, context and
//! pbuffer made in C (`egl_bootstrap.c`), wrapped for smithay with `EGLContext::from_raw`, plus
//! the `glGetString` facts a gate prints so a `PASS` line says what it passed on.
//!
//! Included by each binary rather than put in a library, because `getrandom`'s custom backend is
//! a `#[no_mangle]` symbol: exactly one definition per binary is what the linker wants.

use core::ffi::c_void;

use smithay::backend::egl::EGLContext;

/// The handles and diagnostics `egl_bootstrap.c` fills.
#[repr(C)]
pub struct GlBootstrap {
    pub display: *mut c_void,
    pub config: *mut c_void,
    pub context: *mut c_void,
    pub surface: *mut c_void,
    /// `eglGetError()` from the step that failed.
    pub error: i32,
    /// Whether `eglGetPlatformDisplayEXT` was found, and what each display path answered.
    pub have_platform_display: i32,
    pub platform_error: i32,
    pub env_error: i32,
    pub egl_major: i32,
    pub egl_minor: i32,
    /// `glGetString` of the current context: the vendor, driver and GL version that ran.
    pub gl_vendor: [core::ffi::c_char; 64],
    pub gl_renderer: [core::ffi::c_char; 64],
    pub gl_version: [core::ffi::c_char; 64],
}

impl GlBootstrap {
    fn empty() -> Self {
        Self {
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
        }
    }
}

unsafe extern "C" {
    /// Creates the display, config, GLES 3 context and pbuffer, and makes the context current.
    /// Returns 0, or a negative code naming the step that failed.
    fn glgate_egl_bootstrap(out: *mut GlBootstrap, width: i32, height: i32) -> i32;
}

/// Creates the context and wraps it for smithay; `Err` names the step and what each EGL display
/// path answered, because those are the two facts that decide whether this port can have a GL
/// renderer at all.
pub fn create_context(width: i32, height: i32) -> Result<(GlBootstrap, EGLContext), String> {
    let mut bootstrap = GlBootstrap::empty();
    // SAFETY: the struct is writable for the call's duration and every handle it returns stays
    // valid for the process (nothing terminates the display or destroys the context).
    let step = unsafe { glgate_egl_bootstrap(&mut bootstrap, width, height) };
    if step != 0 {
        return Err(format!(
            "EGL bootstrap failed at step {} (eglGetError 0x{:04x}): \
             platform-extension={} platform-error=0x{:04x} env-error=0x{:04x} version={}.{}",
            -step,
            bootstrap.error,
            if bootstrap.have_platform_display != 0 {
                "found"
            } else {
                "missing"
            },
            bootstrap.platform_error,
            bootstrap.env_error,
            bootstrap.egl_major,
            bootstrap.egl_minor
        ));
    }
    // SAFETY: the handles are the ones the bootstrap created, and it keeps them alive for the
    // whole process; nothing else creates an EGL display with these parameters.
    let context =
        unsafe { EGLContext::from_raw(bootstrap.display, bootstrap.config, bootstrap.context) }
            .map_err(|e| format!("EGL context wrap failed: {e:?}"))?;
    Ok((bootstrap, context))
}

/// What the GL actually is, as one line for a boot log.
pub fn describe(bootstrap: &GlBootstrap) -> String {
    format!(
        "GL_VENDOR={} GL_RENDERER={} GL_VERSION={} (EGL {}.{})",
        c_string(&bootstrap.gl_vendor),
        c_string(&bootstrap.gl_renderer),
        c_string(&bootstrap.gl_version),
        bootstrap.egl_major,
        bootstrap.egl_minor
    )
}

fn c_string(bytes: &[core::ffi::c_char; 64]) -> std::borrow::Cow<'_, str> {
    // SAFETY: the C side NUL-terminates within the 64 bytes it was given.
    unsafe { core::ffi::CStr::from_ptr(bytes.as_ptr()) }.to_string_lossy()
}

/// `getrandom`'s custom backend (`--cfg getrandom_backend="custom"`).
///
/// The port has no entropy source yet, and nothing on 4d's path needs cryptographic randomness:
/// smithay's `rand` use is identifiers and cookies. A counter mixed with the stack address and
/// the cycle counter is enough for that, and it is *not* enough for anything else — which is why
/// it lives in a gate and not in a library.
pub mod entropy {
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
