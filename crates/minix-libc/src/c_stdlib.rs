//! C stdlib helpers — `div`/`abs`/`atoi`, the narrow `strto*` parsers,
//! and `strtod`. Ported from the old `tools/c-libc.c`, plus the integer
//! parsers the header always declared but never implemented.

use core::ffi::{c_char, c_int, c_long, c_longlong};
#[cfg(target_os = "minix")]
use core::ffi::{c_ulong, c_void};

/// `div_t` — `{ int quot, rem }`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DivT {
    pub quot: c_int,
    pub rem: c_int,
}

/// `ldiv_t` — `{ long quot, rem }`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LDivT {
    pub quot: c_long,
    pub rem: c_long,
}

/// `lldiv_t` — `{ long long quot, rem }`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LLDivT {
    pub quot: c_longlong,
    pub rem: c_longlong,
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn abs(x: c_int) -> c_int {
    x.abs()
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn labs(x: c_long) -> c_long {
    x.abs()
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn llabs(x: c_longlong) -> c_longlong {
    x.abs()
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn div(x: c_int, y: c_int) -> DivT {
    DivT {
        quot: x / y,
        rem: x % y,
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn ldiv(x: c_long, y: c_long) -> LDivT {
    LDivT {
        quot: x / y,
        rem: x % y,
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn lldiv(x: c_longlong, y: c_longlong) -> LLDivT {
    LLDivT {
        quot: x / y,
        rem: x % y,
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'\x0b')
}

fn digit_val(c: u8) -> i32 {
    match c {
        b'0'..=b'9' => (c - b'0') as i32,
        b'a'..=b'z' => (c - b'a') as i32 + 10,
        b'A'..=b'Z' => (c - b'A') as i32 + 10,
        _ => -1,
    }
}

pub(crate) fn strtoull_impl(s: *const u8, endptr: *mut *mut u8, base: i32) -> u64 {
    let start = s;
    let mut p = s;
    while is_space(unsafe { *p }) {
        p = unsafe { p.add(1) };
    }
    if matches!(unsafe { *p }, b'+' | b'-') {
        p = unsafe { p.add(1) };
    }
    let mut base = base;
    if base == 0 {
        if unsafe { *p } == b'0' && matches!(unsafe { *p.add(1) }, b'x' | b'X') {
            base = 16;
            p = unsafe { p.add(2) };
        } else if unsafe { *p } == b'0' {
            base = 8;
        } else {
            base = 10;
        }
    }
    let mut v: u64 = 0;
    let mut any = false;
    loop {
        let d = digit_val(unsafe { *p });
        if d < 0 || d >= base {
            break;
        }
        v = v.wrapping_mul(base as u64).wrapping_add(d as u64);
        any = true;
        p = unsafe { p.add(1) };
    }
    if !endptr.is_null() {
        unsafe { *endptr = (if any { p } else { start }) as *mut u8 };
    }
    v
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtoull(s: *const c_char, endptr: *mut *mut c_char, base: c_int) -> u64 {
    strtoull_impl(s as *const u8, endptr as *mut *mut u8, base)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtoll(s: *const c_char, endptr: *mut *mut c_char, base: c_int) -> i64 {
    let start = s;
    let mut p = s;
    while is_space(unsafe { *p } as u8) {
        p = unsafe { p.add(1) };
    }
    let mut neg = false;
    if unsafe { *p } == b'-' as c_char {
        neg = true;
        p = unsafe { p.add(1) };
    } else if unsafe { *p } == b'+' as c_char {
        p = unsafe { p.add(1) };
    }
    let v = strtoull_impl(p as *const u8, endptr as *mut *mut u8, base);
    if !endptr.is_null() && unsafe { *endptr } == p as *mut c_char {
        unsafe { *endptr = start as *mut c_char };
    }
    if neg {
        (v as i64).wrapping_neg()
    } else {
        v as i64
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtoul(
    s: *const c_char,
    endptr: *mut *mut c_char,
    base: c_int,
) -> c_ulong {
    strtoull_impl(s as *const u8, endptr as *mut *mut u8, base) as c_ulong
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtol(s: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_long {
    unsafe { strtoll(s, endptr, base) as c_long }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn atoi(s: *const c_char) -> c_int {
    unsafe { strtol(s, core::ptr::null_mut(), 10) as c_int }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn atol(s: *const c_char) -> c_long {
    unsafe { strtol(s, core::ptr::null_mut(), 10) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn atoll(s: *const c_char) -> c_longlong {
    unsafe { strtoll(s, core::ptr::null_mut(), 10) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn atof(s: *const c_char) -> f64 {
    strtod_impl(s as *const u8, core::ptr::null_mut())
}

/// Shared `strtod` core: decimal floats with fraction and exponent —
/// enough for charconv/LLVM float parsing. Non-numeric input leaves
/// `*endptr` at the start of the input.
pub(crate) fn strtod_impl(s: *const u8, endptr: *mut *mut u8) -> f64 {
    let start = s;
    let mut p = s;
    while is_space(unsafe { *p }) {
        p = unsafe { p.add(1) };
    }
    let mut neg = false;
    if matches!(unsafe { *p }, b'+' | b'-') {
        neg = unsafe { *p } == b'-';
        p = unsafe { p.add(1) };
    }
    let mut v = 0.0;
    let mut any = false;
    while (unsafe { *p }).is_ascii_digit() {
        v = v * 10.0 + (unsafe { *p } - b'0') as f64;
        any = true;
        p = unsafe { p.add(1) };
    }
    if unsafe { *p } == b'.' {
        p = unsafe { p.add(1) };
        let mut f = 0.1;
        while (unsafe { *p }).is_ascii_digit() {
            v += (unsafe { *p } - b'0') as f64 * f;
            f *= 0.1;
            any = true;
            p = unsafe { p.add(1) };
        }
    }
    if any && matches!(unsafe { *p }, b'e' | b'E') {
        p = unsafe { p.add(1) };
        let mut eneg = false;
        if matches!(unsafe { *p }, b'+' | b'-') {
            eneg = unsafe { *p } == b'-';
            p = unsafe { p.add(1) };
        }
        let mut e: i32 = 0;
        while (unsafe { *p }).is_ascii_digit() {
            e = e * 10 + (unsafe { *p } - b'0') as i32;
            p = unsafe { p.add(1) };
        }
        let mut m = 1.0;
        while e > 0 {
            m *= 10.0;
            e -= 1;
        }
        v = if eneg { v / m } else { v * m };
    }
    if !endptr.is_null() {
        unsafe { *endptr = (if any { p } else { start }) as *mut u8 };
    }
    if neg { -v } else { v }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtod(s: *const c_char, endptr: *mut *mut c_char) -> f64 {
    strtod_impl(s as *const u8, endptr as *mut *mut u8)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtof(s: *const c_char, endptr: *mut *mut c_char) -> f32 {
    strtod_impl(s as *const u8, endptr as *mut *mut u8) as f32
}

#[cfg(all(target_os = "minix", target_arch = "x86_64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn strtold(s: *const c_char, endptr: *mut *mut c_char) -> f64 {
    // `strtold` must return an 80-bit long double in x87 ST0 (SysV ABI),
    // which Rust cannot name; compute the value as f64 via `strtod` and
    // widen it to 80-bit in ST0.
    core::arch::naked_asm!(
        "sub rsp, 24", // align to 16 for the call
        "call {strtod}",
        "movsd [rsp], xmm0",
        "fld qword ptr [rsp]",
        "add rsp, 24",
        "ret",
        strtod = sym crate::c_stdlib::strtod,
    )
}

/// On the 64-bit targets `long double` is `double`, so this *is* `strtod`; the
/// x86_64 form above exists only because that ABI returns the value in x87 ST0.
#[cfg(all(target_os = "minix", not(target_arch = "x86_64")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtold(s: *const c_char, endptr: *mut *mut c_char) -> f64 {
    unsafe { strtod(s, endptr) }
}

/// Locale-aware variant; the locale argument is ignored (same as `strtold`).
#[cfg(all(target_os = "minix", target_arch = "x86_64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn strtold_l(
    s: *const c_char,
    endptr: *mut *mut c_char,
    _loc: *const c_void,
) -> f64 {
    core::arch::naked_asm!(
        "sub rsp, 24",
        "call {strtod}",
        "movsd [rsp], xmm0",
        "fld qword ptr [rsp]",
        "add rsp, 24",
        "ret",
        strtod = sym crate::c_stdlib::strtod,
    )
}

/// Locale-aware variant; the locale argument is ignored (same as `strtold`).
#[cfg(all(target_os = "minix", not(target_arch = "x86_64")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtold_l(
    s: *const c_char,
    endptr: *mut *mut c_char,
    _loc: *const c_void,
) -> f64 {
    unsafe { strtod(s, endptr) }
}

/// Radix-independent exponent via IEEE bit extraction (no_std has no `ln`).
/// `logb(0) = -inf`, `logb(±inf) = +inf`, `logb(nan) = nan`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn logb(x: f64) -> f64 {
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    if exp == 0x7ff {
        // inf or nan
        return if frac == 0 { f64::INFINITY } else { f64::NAN };
    }
    if exp == 0 {
        // subnormal: value = frac * 2^-1074, msb position p (0..=51) gives
        // logb = p - 1074.
        let lz = frac.leading_zeros() as i64;
        return (63 - lz - 1074) as f64;
    }
    (exp - 1023) as f64
}

/// Sort `nmemb` elements of `size` bytes at `base` with the comparator
/// (insertion sort; fine for the small arrays LLVM sorts).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qsort(
    base: *mut c_void,
    nmemb: usize,
    size: usize,
    compar: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> c_int>,
) {
    if base.is_null() || nmemb < 2 || size == 0 {
        return;
    }
    let Some(compar) = compar else { return };
    let base = base as *mut u8;
    for i in 1..nmemb {
        let mut j = i;
        while j > 0 {
            let a = unsafe { base.add((j - 1) * size) };
            let b = unsafe { base.add(j * size) };
            if unsafe { compar(a as *const c_void, b as *const c_void) } <= 0 {
                break;
            }
            for k in 0..size {
                unsafe {
                    let t = *a.add(k);
                    *a.add(k) = *b.add(k);
                    *b.add(k) = t;
                }
            }
            j -= 1;
        }
    }
}

/// Binary search for `key` among `nmemb` elements of `size` bytes at `base`,
/// which `cmp` must already have sorted. `cmp` receives the key first and the
/// element second, as the C comparator does.
///
/// Split out from the export so the host suite can exercise the bounds it
/// computes without a C callback having to be called.
fn bsearch_impl(
    base: *const u8,
    nmemb: usize,
    size: usize,
    key: *const u8,
    cmp: impl Fn(*const u8, *const u8) -> i32,
) -> *const u8 {
    let mut lo = 0usize;
    let mut hi = nmemb;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let elem = unsafe { base.add(mid * size) };
        match cmp(key, elem) {
            0 => return elem,
            n if n < 0 => hi = mid,
            _ => lo = mid + 1,
        }
    }
    core::ptr::null()
}

/// POSIX `bsearch()`: a pointer to the element matching `key` in a sorted
/// array, or null when it is absent.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bsearch(
    key: *const c_void,
    base: *const c_void,
    nmemb: usize,
    size: usize,
    compar: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> c_int>,
) -> *mut c_void {
    let (Some(compar), false) = (compar, base.is_null() || size == 0) else {
        return core::ptr::null_mut();
    };
    let found = bsearch_impl(
        base as *const u8,
        nmemb,
        size,
        key as *const u8,
        |key, elem| unsafe { compar(key as *const c_void, elem as *const c_void) },
    );
    found as *mut c_void
}

/// Six characters for a `mktemp` suffix, from a caller-supplied seed. The
/// alphabet is the portable filename set, so no suffix needs escaping.
fn temp_suffix(seed: u32) -> [u8; 6] {
    const ALPHABET: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut out = [0u8; 6];
    let mut state = seed;
    for c in &mut out {
        state = state.wrapping_mul(1103515245).wrapping_add(12345);
        *c = ALPHABET[((state >> 16) & 0x3f) as usize % ALPHABET.len()];
    }
    out
}

/// `mktemp()`: replace `template`'s trailing `XXXXXX` in place and return the
/// template, or null when there is no `XXXXXX` to fill.
///
/// The name is *not* reserved: nothing stops another process from taking it
/// between this call and the caller's `open`, which is why POSIX deprecated
/// `mktemp` in favour of `mkstemp`. It exists here because bash's temporary-file
/// path calls it, not because it is a good interface.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mktemp(template: *mut c_char) -> *mut c_char {
    if template.is_null() {
        return core::ptr::null_mut();
    }
    let bytes = unsafe { core::ffi::CStr::from_ptr(template) }.to_bytes();
    if bytes.len() < 6 || &bytes[bytes.len() - 6..] != b"XXXXXX" {
        return core::ptr::null_mut();
    }
    // The pid and the clock are what differ between two callers that arrive in
    // the same tick; the counter separates two calls from the same process.
    // A clock that will not answer leaves the other two, which is a weaker
    // seed but not a failure this function can report.
    let ticks = match minix_std::time::clock_gettime(minix_std::time::CLOCK_MONOTONIC) {
        Ok(t) => t.tv_nsec as u32,
        Err(_) => 0,
    };
    let pid = crate::getpid() as u32;
    static mut SEQ: u32 = 0;
    let seq = unsafe {
        SEQ = SEQ.wrapping_add(1);
        SEQ
    };
    let suffix = temp_suffix(pid ^ ticks.rotate_left(7) ^ seq.wrapping_mul(0x9E37_79B9));
    for (i, c) in suffix.iter().enumerate() {
        unsafe { *template.add(bytes.len() - 6 + i) = *c as c_char };
    }
    template
}

// glibc-compatible 31-bit LCG; POSIX leaves the sequence unspecified.
static mut RAND_STATE: u32 = 1;

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn srand(seed: core::ffi::c_uint) {
    unsafe { RAND_STATE = seed.wrapping_add(1) | 1 };
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rand() -> c_int {
    unsafe {
        RAND_STATE = RAND_STATE.wrapping_mul(1103515245).wrapping_add(12345);
        ((RAND_STATE >> 16) & 0x7fff) as c_int
    }
}

#[cfg(target_os = "minix")]
/// The process environment, as C sees it: the NULL-terminated array the kernel
/// handed `main`, published by `crt0` through `__minix_set_environ`.
///
/// It points at an empty array until then, which is what makes an `environ`
/// dereference from an `.init_array` constructor safe rather than a null
/// pointer.
#[unsafe(no_mangle)]
pub static mut environ: *mut *mut c_char = core::ptr::addr_of_mut!(EMPTY_ENVIRON);

#[cfg(target_os = "minix")]
static mut EMPTY_ENVIRON: *mut c_char = core::ptr::null_mut();

/// Publish the environment block `crt0` was handed. Called before `main`, so
/// constructors and `getenv` see the real environment.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __minix_set_environ(envp: *mut *mut c_char) {
    unsafe { environ = envp };
}

/// Find `name` in the NULL-terminated `env` array, answering the value (the
/// bytes after the `=`) or null.
fn getenv_in(env: *const *const c_char, name: &[u8]) -> *mut c_char {
    if env.is_null() || name.is_empty() || name.contains(&b'=') {
        return core::ptr::null_mut();
    }
    let mut p = env;
    loop {
        let entry = unsafe { *p };
        if entry.is_null() {
            return core::ptr::null_mut();
        }
        let bytes = unsafe { core::ffi::CStr::from_ptr(entry) }.to_bytes();
        if let Some(eq) = bytes.iter().position(|b| *b == b'=')
            && &bytes[..eq] == name
        {
            return unsafe { entry.add(eq + 1) as *mut c_char };
        }
        p = unsafe { p.add(1) };
    }
}

/// C `getenv()`: the value of `name` in the environment, or null. The pointer
/// is into the environment block, as POSIX specifies — the caller must not free
/// it, and it must not be used after `setenv`/`putenv` moves that block.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getenv(name: *const c_char) -> *mut c_char {
    if name.is_null() {
        return core::ptr::null_mut();
    }
    let want = unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes();
    getenv_in(unsafe { environ } as *const *const c_char, want)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aligned_alloc(alignment: usize, size: usize) -> *mut c_void {
    // minix-libc's malloc returns 16-byte-aligned blocks; anything beyond
    // that needs a header that free() cannot recover, so it is not
    // supported yet (over-aligned new/delete would abort instead of
    // corrupting the heap).
    if alignment > 16 {
        return core::ptr::null_mut();
    }
    unsafe { crate::malloc(size) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "minix")]
    #[test]
    fn div_quot_rem() {
        let r = div(7, 2);
        assert_eq!((r.quot, r.rem), (3, 1));
    }

    #[test]
    fn getenv_in_reads_the_environment() {
        let mut path = *b"PATH=/bin:/sbin\0";
        let mut empty = *b"EMPTY=\0";
        let mut env: [*const c_char; 4] = [
            path.as_mut_ptr() as *const c_char,
            empty.as_mut_ptr() as *const c_char,
            core::ptr::null(),
            core::ptr::null(),
        ];
        let env = env.as_mut_ptr() as *const *const c_char;
        unsafe {
            let v = getenv_in(env, b"PATH");
            assert!(!v.is_null());
            assert_eq!(core::ffi::CStr::from_ptr(v).to_bytes(), b"/bin:/sbin");
            // A name with no value is found, with an empty value.
            let v = getenv_in(env, b"EMPTY");
            assert!(!v.is_null());
            assert_eq!(core::ffi::CStr::from_ptr(v).to_bytes(), b"");
            // Absent, invalid, and empty names answer null.
            assert!(getenv_in(env, b"HOME").is_null());
            assert!(getenv_in(env, b"PA=TH").is_null());
            assert!(getenv_in(env, b"").is_null());
            assert!(getenv_in(core::ptr::null(), b"PATH").is_null());
        }
    }

    #[test]
    fn temp_suffix_is_a_portable_name() {
        let a = temp_suffix(1);
        let b = temp_suffix(2);
        assert_ne!(a, b);
        for c in a.iter().chain(b.iter()) {
            assert!(
                c.is_ascii_alphanumeric(),
                "{} is not a filename byte",
                *c as char
            );
        }
        // The same seed gives the same name, which is what makes the suffix a
        // function of the seed rather than of hidden state.
        assert_eq!(a, temp_suffix(1));
    }

    #[test]
    fn bsearch_impl_finds_the_key() {
        let data: [u32; 5] = [1, 3, 5, 7, 9];
        let size = core::mem::size_of::<u32>();
        let base = data.as_ptr() as *const u8;
        let cmp = |key: *const u8, elem: *const u8| {
            let (k, e) = unsafe { (*(key as *const u32), *(elem as *const u32)) };
            if k < e { -1 } else { i32::from(k > e) }
        };
        for (i, value) in data.iter().enumerate() {
            let key = core::ptr::from_ref(value) as *const u8;
            let hit = bsearch_impl(base, data.len(), size, key, cmp);
            assert_eq!(hit, unsafe { base.add(i * size) });
        }

        let absent = 4u32;
        let key = core::ptr::from_ref(&absent) as *const u8;
        assert!(bsearch_impl(base, data.len(), size, key, cmp).is_null());
        assert!(bsearch_impl(base, 0, size, key, cmp).is_null());
    }

    #[test]
    fn strtoull_base_detect() {
        let s = b"0x1f 10";
        let mut end: *mut u8 = core::ptr::null_mut();
        let v = strtoull_impl(s.as_ptr(), &mut end, 0);
        assert_eq!(v, 31);
        assert_eq!(end, unsafe { s.as_ptr().add(4) } as *mut u8);
    }

    #[test]
    fn strtoull_no_digits() {
        let s = b"zzz";
        let mut end: *mut u8 = core::ptr::null_mut();
        let v = strtoull_impl(s.as_ptr(), &mut end, 10);
        assert_eq!(v, 0);
        assert_eq!(end, s.as_ptr() as *mut u8);
    }

    #[test]
    fn strtod_parses() {
        let mut end: *mut u8 = core::ptr::null_mut();
        let s = b"  -12.5e2xyz";
        let v = strtod_impl(s.as_ptr(), &mut end);
        assert_eq!(v, -1250.0);
        assert_eq!(end, unsafe { s.as_ptr().add(9) } as *mut u8);
    }

    #[test]
    fn strtod_empty() {
        let mut end: *mut u8 = core::ptr::null_mut();
        let s = b"abc";
        let v = strtod_impl(s.as_ptr(), &mut end);
        assert_eq!(v, 0.0);
        assert_eq!(end, s.as_ptr() as *mut u8);
    }
}
