//! C wide-character strings (`wchar.h`/`wctype.h`) — the functions libc++'s
//! `char_traits<wchar_t>` and wide I/O need, ported from the old
//! `tools/c-libc.c`. `wchar_t` is `int` (4 bytes) on this target.

#[cfg(target_os = "minix")]
use core::ffi::{VaList, c_long, c_ulong, c_void};
use core::ffi::{c_int, c_uint};

/// `wchar_t` (the compiler builtin on this target is a 32-bit int).
pub type WChar = c_int;
/// `wint_t`.
pub type WInt = c_uint;
/// `WEOF` (`wint_t` -1).
pub const WEOF: c_uint = c_uint::MAX;

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcslen(s: *const WChar) -> usize {
    if s.is_null() {
        return 0;
    }
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcscpy(dst: *mut WChar, src: *const WChar) -> *mut WChar {
    let mut d = 0usize;
    loop {
        let c = unsafe { *src.add(d) };
        unsafe { *dst.add(d) = c };
        if c == 0 {
            break;
        }
        d += 1;
    }
    dst
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsncpy(dst: *mut WChar, src: *const WChar, mut n: usize) -> *mut WChar {
    let mut d = 0usize;
    while n > 0 {
        let c = unsafe { *src.add(d) };
        if c == 0 {
            break;
        }
        unsafe { *dst.add(d) = c };
        d += 1;
        n -= 1;
    }
    while n > 0 {
        unsafe { *dst.add(d) = 0 };
        d += 1;
        n -= 1;
    }
    dst
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcscat(dst: *mut WChar, src: *const WChar) -> *mut WChar {
    let len = unsafe { wcslen(dst) };
    unsafe { wcscpy(dst.add(len), src) };
    dst
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsncat(dst: *mut WChar, src: *const WChar, mut n: usize) -> *mut WChar {
    let mut d = unsafe { wcslen(dst) };
    let mut s = 0usize;
    while n > 0 {
        let c = unsafe { *src.add(s) };
        if c == 0 {
            break;
        }
        unsafe { *dst.add(d) = c };
        d += 1;
        s += 1;
        n -= 1;
    }
    unsafe { *dst.add(d) = 0 };
    dst
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcscmp(a: *const WChar, b: *const WChar) -> c_int {
    let mut i = 0usize;
    loop {
        let ca = unsafe { *a.add(i) };
        let cb = unsafe { *b.add(i) };
        if ca == 0 || ca != cb {
            return ca - cb;
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsncmp(a: *const WChar, b: *const WChar, mut n: usize) -> c_int {
    let mut i = 0usize;
    while n > 0 {
        let ca = unsafe { *a.add(i) };
        let cb = unsafe { *b.add(i) };
        if ca == 0 || ca != cb {
            return ca - cb;
        }
        i += 1;
        n -= 1;
    }
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcschr(s: *const WChar, c: WChar) -> *mut WChar {
    let mut i = 0usize;
    loop {
        let sc = unsafe { *s.add(i) };
        if sc == c {
            return unsafe { s.add(i) } as *mut WChar;
        }
        if sc == 0 {
            return core::ptr::null_mut();
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsrchr(s: *const WChar, c: WChar) -> *mut WChar {
    let mut i = 0usize;
    let mut found: *mut WChar = core::ptr::null_mut();
    loop {
        let sc = unsafe { *s.add(i) };
        if sc == 0 {
            if c == 0 {
                return unsafe { s.add(i) } as *mut WChar;
            }
            return found;
        }
        if sc == c {
            found = unsafe { s.add(i) } as *mut WChar;
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsstr(haystack: *const WChar, needle: *const WChar) -> *mut WChar {
    if unsafe { *needle } == 0 {
        return haystack as *mut WChar;
    }
    let mut h = 0usize;
    loop {
        let mut hi = h;
        let mut ni = 0usize;
        loop {
            let hc = unsafe { *haystack.add(hi) };
            let nc = unsafe { *needle.add(ni) };
            if nc == 0 {
                return unsafe { haystack.add(h) } as *mut WChar;
            }
            if hc == 0 || hc != nc {
                break;
            }
            hi += 1;
            ni += 1;
        }
        if unsafe { *haystack.add(h) } == 0 {
            return core::ptr::null_mut();
        }
        h += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcspbrk(s: *const WChar, accept: *const WChar) -> *mut WChar {
    let mut i = 0usize;
    loop {
        let sc = unsafe { *s.add(i) };
        if sc == 0 {
            return core::ptr::null_mut();
        }
        let mut a = 0usize;
        loop {
            let ac = unsafe { *accept.add(a) };
            if ac == 0 {
                break;
            }
            if sc == ac {
                return unsafe { s.add(i) } as *mut WChar;
            }
            a += 1;
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsspn(s: *const WChar, accept: *const WChar) -> usize {
    let mut n = 0usize;
    loop {
        let c = unsafe { *s.add(n) };
        if c == 0 {
            break;
        }
        if unsafe { wcschr(accept, c) }.is_null() {
            break;
        }
        n += 1;
    }
    n
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcscspn(s: *const WChar, reject: *const WChar) -> usize {
    let mut n = 0usize;
    loop {
        let c = unsafe { *s.add(n) };
        if c == 0 {
            break;
        }
        if !unsafe { wcschr(reject, c) }.is_null() {
            break;
        }
        n += 1;
    }
    n
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wmemcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let d = dst as *mut WChar;
    let s = src as *const WChar;
    for i in 0..n {
        unsafe { *d.add(i) = *s.add(i) };
    }
    dst
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wmemmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let d = dst as *mut WChar;
    let s = src as *const WChar;
    if (d as usize) < (s as usize) {
        for i in 0..n {
            unsafe { *d.add(i) = *s.add(i) };
        }
    } else {
        for i in (0..n).rev() {
            unsafe { *d.add(i) = *s.add(i) };
        }
    }
    dst
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wmemset(s: *mut c_void, c: WChar, n: usize) -> *mut c_void {
    let p = s as *mut WChar;
    for i in 0..n {
        unsafe { *p.add(i) = c };
    }
    s
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wmemcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int {
    let x = a as *const WChar;
    let y = b as *const WChar;
    for i in 0..n {
        let (cx, cy) = unsafe { (*x.add(i), *y.add(i)) };
        if cx != cy {
            return if cx > cy { 1 } else { -1 };
        }
    }
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wmemchr(s: *const WChar, c: WChar, n: usize) -> *mut WChar {
    for i in 0..n {
        if unsafe { *s.add(i) } == c {
            return unsafe { s.add(i) } as *mut WChar;
        }
    }
    core::ptr::null_mut()
}

fn wdigit(c: WChar) -> i32 {
    if (b'0' as WChar..=b'9' as WChar).contains(&c) {
        return c - b'0' as WChar;
    }
    if (b'a' as WChar..=b'z' as WChar).contains(&c) {
        return c - b'a' as WChar + 10;
    }
    if (b'A' as WChar..=b'Z' as WChar).contains(&c) {
        return c - b'A' as WChar + 10;
    }
    -1
}

fn wcstoull_impl(s: *const WChar, endptr: *mut *mut WChar, base: i32) -> u64 {
    let start = s;
    let mut p = s;
    while unsafe { *p } == b' ' as WChar || unsafe { *p } == b'\t' as WChar {
        p = unsafe { p.add(1) };
    }
    if unsafe { *p } == b'+' as WChar || unsafe { *p } == b'-' as WChar {
        p = unsafe { p.add(1) };
    }
    let mut base = base;
    if base == 0 {
        if unsafe { *p } == b'0' as WChar
            && (unsafe { *p.add(1) } == b'x' as WChar || unsafe { *p.add(1) } == b'X' as WChar)
        {
            base = 16;
            p = unsafe { p.add(2) };
        } else if unsafe { *p } == b'0' as WChar {
            base = 8;
        } else {
            base = 10;
        }
    }
    let mut v: u64 = 0;
    let mut any = false;
    loop {
        let d = wdigit(unsafe { *p });
        if d < 0 || d >= base {
            break;
        }
        v = v.wrapping_mul(base as u64).wrapping_add(d as u64);
        any = true;
        p = unsafe { p.add(1) };
    }
    if !endptr.is_null() {
        unsafe { *endptr = if any { p } else { start } as *mut WChar };
    }
    v
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcstoull(s: *const WChar, endptr: *mut *mut WChar, base: c_int) -> u64 {
    wcstoull_impl(s, endptr, base)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcstoll(s: *const WChar, endptr: *mut *mut WChar, base: c_int) -> i64 {
    let start = s;
    let mut p = s;
    while unsafe { *p } == b' ' as WChar || unsafe { *p } == b'\t' as WChar {
        p = unsafe { p.add(1) };
    }
    let mut neg = false;
    if unsafe { *p } == b'-' as WChar {
        neg = true;
        p = unsafe { p.add(1) };
    } else if unsafe { *p } == b'+' as WChar {
        p = unsafe { p.add(1) };
    }
    let v = wcstoull_impl(p, endptr, base);
    if !endptr.is_null() && unsafe { *endptr } == p as *mut WChar {
        unsafe { *endptr = start as *mut WChar };
    }
    if neg {
        (v as i64).wrapping_neg()
    } else {
        v as i64
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcstoul(s: *const WChar, endptr: *mut *mut WChar, base: c_int) -> c_ulong {
    wcstoull_impl(s, endptr, base) as c_ulong
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcstol(s: *const WChar, endptr: *mut *mut WChar, base: c_int) -> c_long {
    unsafe { wcstoll(s, endptr, base) as c_long }
}

fn is_float_char(c: WChar) -> bool {
    (c >= b'0' as WChar && c <= b'9' as WChar)
        || (c >= b'a' as WChar && c <= b'e' as WChar)
        || (c >= b'A' as WChar && c <= b'E' as WChar)
        || c == b'.' as WChar
        || c == b'+' as WChar
        || c == b'-' as WChar
}

fn wcstod_helper(s: *const WChar, endptr: *mut *mut WChar) -> f64 {
    let start = s;
    let mut p = s;
    while unsafe { *p } == b' ' as WChar || unsafe { *p } == b'\t' as WChar {
        p = unsafe { p.add(1) };
    }
    if unsafe { *p } == b'+' as WChar || unsafe { *p } == b'-' as WChar {
        p = unsafe { p.add(1) };
    }
    let num = p;
    while is_float_char(unsafe { *p }) {
        p = unsafe { p.add(1) };
    }
    if p == num {
        if !endptr.is_null() {
            unsafe { *endptr = start as *mut WChar };
        }
        return 0.0;
    }
    let n = (p as usize - num as usize) / core::mem::size_of::<WChar>();
    let mut narrow = [0u8; 64];
    let copy = n.min(narrow.len() - 1);
    for (i, slot) in narrow[..copy].iter_mut().enumerate() {
        *slot = unsafe { *num.add(i) } as u8;
    }
    narrow[copy] = 0;
    let mut nend: *mut u8 = core::ptr::null_mut();
    let v = crate::c_stdlib::strtod_impl(narrow.as_ptr(), &mut nend);
    if !endptr.is_null() {
        let consumed = nend as usize - narrow.as_ptr() as usize;
        unsafe { *endptr = num.add(consumed) as *mut WChar };
    }
    v
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcstod(s: *const WChar, endptr: *mut *mut WChar) -> f64 {
    wcstod_helper(s, endptr)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcstof(s: *const WChar, endptr: *mut *mut WChar) -> f32 {
    wcstod_helper(s, endptr) as f32
}

#[cfg(all(target_os = "minix", target_arch = "x86_64"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn wcstold(s: *const WChar, endptr: *mut *mut WChar) -> f64 {
    // 80-bit long double in ST0, see `strtold` in c_stdlib.rs.
    core::arch::naked_asm!(
        "sub rsp, 24",
        "call {wcstod}",
        "movsd [rsp], xmm0",
        "fld qword ptr [rsp]",
        "add rsp, 24",
        "ret",
        wcstod = sym crate::c_wchar::wcstod,
    )
}

/// Wide printf: narrow the format, format through `vsnprintf`, widen back.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vswprintf(
    ws: *mut WChar,
    n: usize,
    fmt: *const WChar,
    ap: VaList<'_>,
) -> c_int {
    if ws.is_null() || fmt.is_null() || n == 0 {
        return -1;
    }
    let mut narrow = [0u8; 128];
    let mut len = 0usize;
    while unsafe { *fmt.add(len) } != 0 && len < narrow.len() - 1 {
        narrow[len] = unsafe { *fmt.add(len) } as u8;
        len += 1;
    }
    narrow[len] = 0;
    let mut buf = [0u8; 512];
    let r = unsafe {
        crate::c_stdio::vsnprintf(
            buf.as_mut_ptr() as *mut i8,
            buf.len(),
            narrow.as_ptr() as *const i8,
            ap,
        )
    };
    if r < 0 {
        return -1;
    }
    let out = (r as usize).min(n - 1);
    for i in 0..out {
        unsafe { *ws.add(i) = buf[i] as WChar };
    }
    unsafe { *ws.add(out) = 0 };
    out as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn swprintf(ws: *mut WChar, n: usize, fmt: *const WChar, args: ...) -> c_int {
    unsafe { vswprintf(ws, n, fmt, args) }
}

// ---- wctype.h (ASCII-only, mirroring the narrow ctype) ----

pub(crate) fn isw_alpha(c: WInt) -> bool {
    (b'a' as WInt..=b'z' as WInt).contains(&c) || (b'A' as WInt..=b'Z' as WInt).contains(&c)
}

pub(crate) fn isw_lower(c: WInt) -> bool {
    (b'a' as WInt..=b'z' as WInt).contains(&c)
}

pub(crate) fn isw_upper(c: WInt) -> bool {
    (b'A' as WInt..=b'Z' as WInt).contains(&c)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswalpha(c: WInt) -> c_int {
    isw_alpha(c) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswdigit(c: WInt) -> c_int {
    (b'0' as WInt..=b'9' as WInt).contains(&c) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswalnum(c: WInt) -> c_int {
    (isw_alpha(c) || (b'0' as WInt..=b'9' as WInt).contains(&c)) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswspace(c: WInt) -> c_int {
    (c == b' ' as WInt
        || c == b'\t' as WInt
        || c == b'\n' as WInt
        || c == b'\r' as WInt
        || c == b'\x0c' as WInt
        || c == b'\x0b' as WInt) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswlower(c: WInt) -> c_int {
    (b'a' as WInt..=b'z' as WInt).contains(&c) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswupper(c: WInt) -> c_int {
    (b'A' as WInt..=b'Z' as WInt).contains(&c) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswxdigit(c: WInt) -> c_int {
    ((b'0' as WInt..=b'9' as WInt).contains(&c)
        || (b'a' as WInt..=b'f' as WInt).contains(&c)
        || (b'A' as WInt..=b'F' as WInt).contains(&c)) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswpunct(c: WInt) -> c_int {
    ((b'!' as WInt..=b'/' as WInt).contains(&c)
        || (b':' as WInt..=b'@' as WInt).contains(&c)
        || (b'[' as WInt..=b'`' as WInt).contains(&c)
        || (b'{' as WInt..=b'~' as WInt).contains(&c)) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswgraph(c: WInt) -> c_int {
    (b'!' as WInt..=b'~' as WInt).contains(&c) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswprint(c: WInt) -> c_int {
    (b' ' as WInt..=b'~' as WInt).contains(&c) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswcntrl(c: WInt) -> c_int {
    (c < b' ' as WInt || c == 127) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswblank(c: WInt) -> c_int {
    (c == b' ' as WInt || c == b'\t' as WInt) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn towlower(c: WInt) -> WInt {
    if isw_upper(c) {
        c + (b'a' as WInt - b'A' as WInt)
    } else {
        c
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn towupper(c: WInt) -> WInt {
    if isw_lower(c) {
        c - (b'a' as WInt - b'A' as WInt)
    } else {
        c
    }
}

// Wide FILE I/O (C locale: one byte == one wide char, so these forward to
// the narrow stdio functions). libc++'s std_stream.h calls getwc/ungetwc/
// fputwc directly.

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getwc(stream: *mut crate::c_stdio::FILE) -> WInt {
    let c = unsafe { crate::c_stdio::fgetc(stream) };
    if c == -1 { WEOF } else { c as WInt }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgetwc(stream: *mut crate::c_stdio::FILE) -> WInt {
    unsafe { getwc(stream) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn putwc(wc: WChar, stream: *mut crate::c_stdio::FILE) -> WInt {
    unsafe { crate::c_stdio::fputc(wc, stream) as WInt }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fputwc(wc: WChar, stream: *mut crate::c_stdio::FILE) -> WInt {
    unsafe { putwc(wc, stream) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ungetwc(wc: WInt, stream: *mut crate::c_stdio::FILE) -> WInt {
    let r = unsafe { crate::c_stdio::ungetc(wc as c_int, stream) };
    if r == -1 { WEOF } else { r as WInt }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getwchar() -> WInt {
    unsafe { getwc(crate::c_stdio::stdin) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn putwchar(wc: WChar) -> WInt {
    unsafe { putwc(wc, crate::c_stdio::stdout) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wcstoull_parses() {
        let s: [WChar; 8] = [b'1' as WChar, b'2' as WChar, b'f' as WChar, 0, 0, 0, 0, 0];
        let mut end: *mut WChar = core::ptr::null_mut();
        let v = wcstoull_impl(s.as_ptr(), &mut end, 16);
        assert_eq!(v, 0x12f);
        assert_eq!(end, unsafe { s.as_ptr().add(3) } as *mut WChar);
    }

    #[test]
    fn wcstoull_no_digits_keeps_start() {
        let s: [WChar; 4] = [b'x' as WChar, b'1' as WChar, 0, 0];
        let mut end: *mut WChar = core::ptr::null_mut();
        let v = wcstoull_impl(s.as_ptr(), &mut end, 10);
        assert_eq!(v, 0);
        assert_eq!(end, s.as_ptr() as *mut WChar);
    }

    #[test]
    fn wcstod_helper_parses() {
        let s: [WChar; 8] = [
            b'1' as WChar,
            b'.' as WChar,
            b'5' as WChar,
            b'e' as WChar,
            b'2' as WChar,
            0,
            0,
            0,
        ];
        let mut end: *mut WChar = core::ptr::null_mut();
        let v = wcstod_helper(s.as_ptr(), &mut end);
        assert_eq!(v, 150.0);
        assert_eq!(end, unsafe { s.as_ptr().add(5) } as *mut WChar);
    }
}
