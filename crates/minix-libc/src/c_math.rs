//! C `math.h` entry points, over the pure-Rust `libm` crate.
//!
//! The port had no libm, but Mesa's `src/util` and the softpipe driver need the
//! C99 double and float functions (sRGB conversion, rounding, min/max, sqrt),
//! so this exposes `libm` — a `no_std` port of musl's math — as C symbols.
//! `libm` omits the integer-returning rounding family, `nearbyint`, and `nan`;
//! those are built here on the functions that do exist.
//!
//! The integer rounding casts saturate rather than trap (Rust's `as` semantics),
//! which is the harmless half of C's undefined out-of-range result.

use core::ffi::{c_char, c_int, c_long, c_longlong};

#[unsafe(no_mangle)]
pub extern "C" fn fabs(x: f64) -> f64 {
    libm::fabs(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn fabsf(x: f32) -> f32 {
    libm::fabsf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn fmin(x: f64, y: f64) -> f64 {
    libm::fmin(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn fminf(x: f32, y: f32) -> f32 {
    libm::fminf(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn fmax(x: f64, y: f64) -> f64 {
    libm::fmax(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn fmaxf(x: f32, y: f32) -> f32 {
    libm::fmaxf(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn floor(x: f64) -> f64 {
    libm::floor(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn floorf(x: f32) -> f32 {
    libm::floorf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn ceil(x: f64) -> f64 {
    libm::ceil(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn ceilf(x: f32) -> f32 {
    libm::ceilf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn trunc(x: f64) -> f64 {
    libm::trunc(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn truncf(x: f32) -> f32 {
    libm::truncf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn round(x: f64) -> f64 {
    libm::round(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn roundf(x: f32) -> f32 {
    libm::roundf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn rint(x: f64) -> f64 {
    libm::rint(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn rintf(x: f32) -> f32 {
    libm::rintf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn nearbyint(x: f64) -> f64 {
    libm::rint(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn nearbyintf(x: f32) -> f32 {
    libm::rintf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn lrint(x: f64) -> c_long {
    libm::rint(x) as c_long
}

#[unsafe(no_mangle)]
pub extern "C" fn lrintf(x: f32) -> c_long {
    libm::rintf(x) as c_long
}

#[unsafe(no_mangle)]
pub extern "C" fn llrint(x: f64) -> c_longlong {
    libm::rint(x) as c_longlong
}

#[unsafe(no_mangle)]
pub extern "C" fn llrintf(x: f32) -> c_longlong {
    libm::rintf(x) as c_longlong
}

#[unsafe(no_mangle)]
pub extern "C" fn lround(x: f64) -> c_long {
    libm::round(x) as c_long
}

#[unsafe(no_mangle)]
pub extern "C" fn lroundf(x: f32) -> c_long {
    libm::roundf(x) as c_long
}

#[unsafe(no_mangle)]
pub extern "C" fn llround(x: f64) -> c_longlong {
    libm::round(x) as c_longlong
}

#[unsafe(no_mangle)]
pub extern "C" fn llroundf(x: f32) -> c_longlong {
    libm::roundf(x) as c_longlong
}

#[unsafe(no_mangle)]
pub extern "C" fn sqrt(x: f64) -> f64 {
    libm::sqrt(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn sqrtf(x: f32) -> f32 {
    libm::sqrtf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn cbrt(x: f64) -> f64 {
    libm::cbrt(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn cbrtf(x: f32) -> f32 {
    libm::cbrtf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn hypotf(x: f32, y: f32) -> f32 {
    libm::hypotf(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn exp(x: f64) -> f64 {
    libm::exp(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn expf(x: f32) -> f32 {
    libm::expf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn exp2(x: f64) -> f64 {
    libm::exp2(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn exp2f(x: f32) -> f32 {
    libm::exp2f(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn expm1(x: f64) -> f64 {
    libm::expm1(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn expm1f(x: f32) -> f32 {
    libm::expm1f(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log(x: f64) -> f64 {
    libm::log(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn logf(x: f32) -> f32 {
    libm::logf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log2(x: f64) -> f64 {
    libm::log2(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log2f(x: f32) -> f32 {
    libm::log2f(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log10(x: f64) -> f64 {
    libm::log10(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log10f(x: f32) -> f32 {
    libm::log10f(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log1p(x: f64) -> f64 {
    libm::log1p(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log1pf(x: f32) -> f32 {
    libm::log1pf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn sin(x: f64) -> f64 {
    libm::sin(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn sinf(x: f32) -> f32 {
    libm::sinf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn cos(x: f64) -> f64 {
    libm::cos(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn cosf(x: f32) -> f32 {
    libm::cosf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn tan(x: f64) -> f64 {
    libm::tan(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn tanf(x: f32) -> f32 {
    libm::tanf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn asin(x: f64) -> f64 {
    libm::asin(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn asinf(x: f32) -> f32 {
    libm::asinf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn acos(x: f64) -> f64 {
    libm::acos(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn acosf(x: f32) -> f32 {
    libm::acosf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn atan(x: f64) -> f64 {
    libm::atan(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn atanf(x: f32) -> f32 {
    libm::atanf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}

#[unsafe(no_mangle)]
pub extern "C" fn atan2f(y: f32, x: f32) -> f32 {
    libm::atan2f(y, x)
}

#[unsafe(no_mangle)]
pub extern "C" fn sinh(x: f64) -> f64 {
    libm::sinh(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn sinhf(x: f32) -> f32 {
    libm::sinhf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn cosh(x: f64) -> f64 {
    libm::cosh(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn coshf(x: f32) -> f32 {
    libm::coshf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn tanh(x: f64) -> f64 {
    libm::tanh(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn tanhf(x: f32) -> f32 {
    libm::tanhf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn pow(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn powf(x: f32, y: f32) -> f32 {
    libm::powf(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn fmod(x: f64, y: f64) -> f64 {
    libm::fmod(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn fmodf(x: f32, y: f32) -> f32 {
    libm::fmodf(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn fma(x: f64, y: f64, z: f64) -> f64 {
    libm::fma(x, y, z)
}

#[unsafe(no_mangle)]
pub extern "C" fn fmaf(x: f32, y: f32, z: f32) -> f32 {
    libm::fmaf(x, y, z)
}

#[unsafe(no_mangle)]
pub extern "C" fn fdim(x: f64, y: f64) -> f64 {
    libm::fdim(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn fdimf(x: f32, y: f32) -> f32 {
    libm::fdimf(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn copysign(x: f64, y: f64) -> f64 {
    libm::copysign(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn copysignf(x: f32, y: f32) -> f32 {
    libm::copysignf(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn nextafter(x: f64, y: f64) -> f64 {
    libm::nextafter(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn nextafterf(x: f32, y: f32) -> f32 {
    libm::nextafterf(x, y)
}

#[unsafe(no_mangle)]
pub extern "C" fn ldexp(x: f64, exp: c_int) -> f64 {
    libm::ldexp(x, exp)
}

#[unsafe(no_mangle)]
pub extern "C" fn ldexpf(x: f32, exp: c_int) -> f32 {
    libm::ldexpf(x, exp)
}

#[unsafe(no_mangle)]
pub extern "C" fn scalbn(x: f64, exp: c_int) -> f64 {
    libm::scalbn(x, exp)
}

#[unsafe(no_mangle)]
pub extern "C" fn scalbnf(x: f32, exp: c_int) -> f32 {
    libm::scalbnf(x, exp)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn frexp(x: f64, exp: *mut c_int) -> f64 {
    let (mantissa, e) = libm::frexp(x);
    if !exp.is_null() {
        unsafe { *exp = e };
    }
    mantissa
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn frexpf(x: f32, exp: *mut c_int) -> f32 {
    let (mantissa, e) = libm::frexpf(x);
    if !exp.is_null() {
        unsafe { *exp = e };
    }
    mantissa
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn modf(x: f64, iptr: *mut f64) -> f64 {
    let (frac, int) = libm::modf(x);
    if !iptr.is_null() {
        unsafe { *iptr = int };
    }
    frac
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn modff(x: f32, iptr: *mut f32) -> f32 {
    let (frac, int) = libm::modff(x);
    if !iptr.is_null() {
        unsafe { *iptr = int };
    }
    frac
}

/// C `nan`: a quiet NaN. The `tagp` string is ignored (only its presence has
/// ever been defined, and no caller reads the payload back).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nan(_tagp: *const c_char) -> f64 {
    f64::from_bits(0x7ff8_0000_0000_0000)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nanf(_tagp: *const c_char) -> f32 {
    f32::from_bits(0x7fc0_0000)
}

/// The floating-point environment. The port rounds to nearest and raises no
/// FP exceptions, so clearing/raising/testing flags is a no-op and the rounding
/// getter reports the fixed mode. LLVM libc's float parser (through libc++'s
/// `charconv.cpp`) is what calls these.
#[unsafe(no_mangle)]
pub extern "C" fn feclearexcept(_excepts: c_int) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn fetestexcept(_excepts: c_int) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn feraiseexcept(_excepts: c_int) -> c_int {
    0
}

/// `FE_TONEAREST`.
#[unsafe(no_mangle)]
pub extern "C" fn fegetround() -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn fesetround(_rounding_mode: c_int) -> c_int {
    0
}
