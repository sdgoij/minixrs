//! C stdio: unbuffered FILE streams and the printf family.
//!
//! The format engine is a faithful port of the old `tools/c-libc.c`
//! `vformat`, keeping the same conversion set (`%d %i %u %x %X %p %s %c
//! %%`, with `l`/`z` length prefixes) and the same (ASCII-only) behavior.

use core::ffi::VaList;
use core::ffi::{c_char, c_int, c_long, c_ulong, c_void};

fn c_strlen(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

/// Opaque-in-C `FILE`; the implementation only needs the fd plus a
/// one-char pushback slot for `ungetc` (the streams are unbuffered).
#[repr(C)]
#[allow(clippy::upper_case_acronyms)]
pub struct FILE {
    fd: c_int,
    pushback: c_int, // -1 = none
}

static mut _STDIN: FILE = FILE {
    fd: 0,
    pushback: -1,
};
static mut _STDOUT: FILE = FILE {
    fd: 1,
    pushback: -1,
};
static mut _STDERR: FILE = FILE {
    fd: 2,
    pushback: -1,
};

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub static mut stdin: *mut FILE = core::ptr::addr_of_mut!(_STDIN);
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub static mut stdout: *mut FILE = core::ptr::addr_of_mut!(_STDOUT);
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub static mut stderr: *mut FILE = core::ptr::addr_of_mut!(_STDERR);

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn putchar(c: c_int) -> c_int {
    let b = c as u8;
    if unsafe { crate::write(1, &b as *const u8 as *const c_void, 1) } == 1 {
        c as u8 as c_int
    } else {
        -1
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn puts(s: *const c_char) -> c_int {
    if s.is_null() {
        return -1;
    }
    let n = c_strlen(s);
    if unsafe { crate::write(1, s as *const c_void, n) } != n as isize {
        return -1;
    }
    unsafe { putchar(b'\n' as c_int) }
}

// ---- printf core, sink-parameterized ----

type Emit<'a> = &'a mut dyn FnMut(u8);

fn emit_pad(c: u8, n: i32, emit: &mut Emit<'_>) {
    for _ in 0..n {
        emit(c);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_num(
    mut v: u64,
    base: u32,
    digits: &[u8],
    neg: bool,
    left: bool,
    zero: bool,
    width: i32,
    emit: &mut Emit<'_>,
) -> i32 {
    let mut buf = [0u8; 40];
    let mut n: i32 = 0;
    loop {
        buf[n as usize] = digits[(v % base as u64) as usize];
        n += 1;
        v /= base as u64;
        if v == 0 {
            break;
        }
    }
    let padn = width - n - neg as i32;
    if !left && zero {
        if neg {
            emit(b'-');
        }
        emit_pad(b'0', padn, emit);
    } else {
        if !left {
            emit_pad(b' ', padn, emit);
        }
        if neg {
            emit(b'-');
        }
    }
    for i in (0..n).rev() {
        emit(buf[i as usize]);
    }
    if left {
        emit_pad(b' ', padn, emit);
    }
    n + neg as i32
}

fn signed_num(l: i64, left: bool, zero: bool, width: i32, emit: &mut Emit<'_>) -> i32 {
    if l < 0 {
        // Two's-complement negation without overflowing the min value.
        let v = (!(l as u64)).wrapping_add(1);
        emit_num(v, 10, b"0123456789", true, left, zero, width, emit)
    } else {
        emit_num(l as u64, 10, b"0123456789", false, left, zero, width, emit)
    }
}

/// Format `fmt` with the C varargs in `args`, feeding output to `emit`.
/// Returns the number of characters that would be written.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated C string, and the varargs must match the
/// conversions in it (same rules as C `printf`).
unsafe fn vformat(fmt: *const c_char, args: &mut VaList<'_>, mut emit: Emit<'_>) -> i32 {
    let mut count = 0;
    let mut p = fmt;
    loop {
        let c = unsafe { *p } as u8;
        if c == 0 {
            break;
        }
        if c != b'%' {
            emit(c);
            count += 1;
            p = unsafe { p.add(1) };
            continue;
        }
        p = unsafe { p.add(1) };
        let (mut left, mut zero, mut width) = (false, false, 0i32);
        loop {
            let d = unsafe { *p } as u8;
            if d == b'-' {
                left = true;
            } else if d == b'0' {
                zero = true;
            } else {
                break;
            }
            p = unsafe { p.add(1) };
        }
        while (unsafe { *p } as u8).is_ascii_digit() {
            width = width * 10 + (unsafe { *p } as u8 - b'0') as i32;
            p = unsafe { p.add(1) };
        }
        let conv = unsafe { *p } as u8;
        match conv {
            b'%' => {
                emit(b'%');
                count += 1;
            }
            b'c' => {
                let c = unsafe { args.next_arg::<c_int>() };
                emit(c as u8);
                count += 1;
            }
            b's' => {
                let mut s = unsafe { args.next_arg::<*const c_char>() };
                if s.is_null() {
                    s = b"(null)".as_ptr() as *const c_char;
                }
                let len = c_strlen(s) as i32;
                let padn = if width > len { width - len } else { 0 };
                if !left {
                    emit_pad(b' ', padn, &mut emit);
                }
                for i in 0..len {
                    emit(unsafe { *s.add(i as usize) } as u8);
                }
                if left {
                    emit_pad(b' ', padn, &mut emit);
                }
                count += len;
            }
            b'p' => {
                let v = unsafe { args.next_arg::<*const c_void>() } as usize as u64;
                count += emit_num(
                    v,
                    16,
                    b"0123456789abcdef",
                    false,
                    left,
                    zero,
                    width,
                    &mut emit,
                );
            }
            b'x' | b'X' => {
                let v = unsafe { args.next_arg::<u32>() } as u64;
                let digits: &[u8] = if conv == b'x' {
                    b"0123456789abcdef"
                } else {
                    b"0123456789ABCDEF"
                };
                count += emit_num(v, 16, digits, false, left, zero, width, &mut emit);
            }
            b'u' => {
                let v = unsafe { args.next_arg::<u32>() } as u64;
                count += emit_num(v, 10, b"0123456789", false, left, zero, width, &mut emit);
            }
            b'd' | b'i' => {
                let l = unsafe { args.next_arg::<c_int>() } as i64;
                count += signed_num(l, left, zero, width, &mut emit);
            }
            b'l' => {
                p = unsafe { p.add(1) };
                let lc = unsafe { *p } as u8;
                match lc {
                    b'd' | b'i' => {
                        let l = unsafe { args.next_arg::<c_long>() } as i64;
                        count += signed_num(l, left, zero, width, &mut emit);
                    }
                    b'u' => {
                        let v = unsafe { args.next_arg::<c_ulong>() } as u64;
                        count +=
                            emit_num(v, 10, b"0123456789", false, left, zero, width, &mut emit);
                    }
                    b'x' | b'X' => {
                        let v = unsafe { args.next_arg::<c_ulong>() } as u64;
                        let digits: &[u8] = if lc == b'x' {
                            b"0123456789abcdef"
                        } else {
                            b"0123456789ABCDEF"
                        };
                        count += emit_num(v, 16, digits, false, left, zero, width, &mut emit);
                    }
                    _ => {
                        emit(b'%');
                        emit(b'l');
                        count += 2;
                    }
                }
            }
            b'z' => {
                p = unsafe { p.add(1) };
                let zc = unsafe { *p } as u8;
                match zc {
                    b'u' => {
                        let v = unsafe { args.next_arg::<usize>() } as u64;
                        count +=
                            emit_num(v, 10, b"0123456789", false, left, zero, width, &mut emit);
                    }
                    b'd' | b'i' => {
                        let l = unsafe { args.next_arg::<isize>() } as i64;
                        count += signed_num(l, left, zero, width, &mut emit);
                    }
                    _ => {
                        emit(b'%');
                        emit(b'z');
                        count += 2;
                    }
                }
            }
            other => {
                emit(b'%');
                emit(other);
                count += 2;
            }
        }
        p = unsafe { p.add(1) };
    }
    count
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vprintf(fmt: *const c_char, mut ap: VaList<'_>) -> c_int {
    let mut emit = |c: u8| {
        let _ = unsafe { putchar(c as c_int) };
    };
    unsafe { vformat(fmt, &mut ap, &mut emit) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn printf(fmt: *const c_char, args: ...) -> c_int {
    unsafe { vprintf(fmt, args) }
}

/// Format into a bounded buffer (C `vsnprintf` semantics): at most
/// `size - 1` characters plus a NUL terminator when `size > 0`; returns
/// the full length regardless.
pub(crate) fn format_to_buf(
    str_: *mut u8,
    size: usize,
    fmt: *const c_char,
    args: &mut VaList<'_>,
) -> i32 {
    struct Buf {
        p: *mut u8,
        size: usize,
        i: usize,
    }
    let mut b = Buf {
        p: str_,
        size,
        i: 0,
    };
    let r = {
        let mut emit = |c: u8| {
            if b.i + 1 < b.size {
                unsafe { *b.p.add(b.i) = c };
            }
            b.i += 1;
        };
        unsafe { vformat(fmt, args, &mut emit) }
    };
    if size > 0 {
        if b.i < size {
            unsafe { *b.p.add(b.i) = 0 };
        } else {
            unsafe { *b.p.add(size - 1) = 0 };
        }
    }
    r
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsnprintf(
    str_: *mut c_char,
    size: usize,
    fmt: *const c_char,
    mut ap: VaList<'_>,
) -> c_int {
    format_to_buf(str_ as *mut u8, size, fmt, &mut ap)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn snprintf(
    str_: *mut c_char,
    size: usize,
    fmt: *const c_char,
    args: ...
) -> c_int {
    unsafe { vsnprintf(str_, size, fmt, args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sprintf(str_: *mut c_char, fmt: *const c_char, args: ...) -> c_int {
    unsafe { vsnprintf(str_, usize::MAX, fmt, args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vfprintf(stream: *mut FILE, fmt: *const c_char, ap: VaList<'_>) -> c_int {
    let _ = stream;
    unsafe { vprintf(fmt, ap) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fprintf(stream: *mut FILE, fmt: *const c_char, args: ...) -> c_int {
    unsafe { vfprintf(stream, fmt, args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fputs(s: *const c_char, stream: *mut FILE) -> c_int {
    let _ = stream;
    if s.is_null() {
        return -1;
    }
    let n = c_strlen(s);
    if unsafe { crate::write(1, s as *const c_void, n) } == n as isize {
        0
    } else {
        -1
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fwrite(
    ptr: *const c_void,
    size: usize,
    nmemb: usize,
    stream: *mut FILE,
) -> usize {
    if ptr.is_null() || stream.is_null() {
        return 0;
    }
    let total = size.saturating_mul(nmemb);
    if total == 0 {
        // POSIX: a zero-byte element write "succeeds" with all members.
        return nmemb;
    }
    let fd = unsafe { (*stream).fd };
    if unsafe { crate::write(fd, ptr, total) } == total as isize {
        nmemb
    } else {
        0
    }
}

/// `sscanf` subset used by the C++ runtime: integer (`%d %i %u %x %o`),
/// float (`%f %e %g`), `%s`, `%c`, `%n`, `%%`, with `*` suppression, widths
/// and the `l`/`L`/`h` length modifiers.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sscanf(
    s: *const c_char,
    fmt: *const c_char,
    mut args: VaList<'_>,
) -> c_int {
    use crate::c_stdlib::{strtod_impl, strtoull_impl};

    fn skip_space(sp: *const c_char) -> *const c_char {
        let mut sp = sp;
        while matches!(
            unsafe { *sp } as u8,
            b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c
        ) {
            sp = unsafe { sp.add(1) };
        }
        sp
    }

    let mut sp = s;
    let mut p = fmt;
    let mut assigned = 0i32;
    loop {
        let c = unsafe { *p } as u8;
        if c == 0 {
            break;
        }
        p = unsafe { p.add(1) };
        if c != b'%' {
            // Literal: must match (after optional whitespace skip).
            sp = skip_space(sp);
            if unsafe { *sp } as u8 == c {
                sp = unsafe { sp.add(1) };
            } else {
                break;
            }
            continue;
        }
        let mut suppress = false;
        let mut width = 0usize;
        if unsafe { *p } as u8 == b'*' {
            suppress = true;
            p = unsafe { p.add(1) };
        }
        while (unsafe { *p } as u8).is_ascii_digit() {
            width = width * 10 + (unsafe { *p } as u8 - b'0') as usize;
            p = unsafe { p.add(1) };
        }
        let mut length = 0u8;
        while matches!(unsafe { *p } as u8, b'h' | b'l' | b'L' | b'j' | b'z' | b't') {
            length = unsafe { *p } as u8;
            p = unsafe { p.add(1) };
        }
        let conv = unsafe { *p } as u8;
        p = unsafe { p.add(1) };
        if !matches!(conv, b'c' | b'n') {
            sp = skip_space(sp);
        }
        match conv {
            b'%' => {
                if unsafe { *sp } as u8 == b'%' {
                    sp = unsafe { sp.add(1) };
                } else {
                    break;
                }
            }
            b'n' => {
                if !suppress {
                    unsafe { *args.next_arg::<*mut c_int>() = assigned };
                }
            }
            b'd' | b'i' | b'u' | b'x' | b'X' | b'o' => {
                let base = match conv {
                    b'o' => 8,
                    b'x' | b'X' => 16,
                    b'i' => 0,
                    _ => 10,
                };
                let mut end: *mut u8 = core::ptr::null_mut();
                let v = strtoull_impl(sp as *const u8, &mut end, base);
                if end as *const u8 == sp as *const u8 {
                    break;
                }
                let mut end_p = end;
                let consumed = end_p as usize - sp as usize;
                if width > 0 && consumed > width {
                    end_p = unsafe { sp.add(width) as *mut u8 };
                }
                sp = end_p as *const c_char;
                if !suppress {
                    if length == b'l' {
                        unsafe { *args.next_arg::<*mut c_long>() = v as c_long };
                    } else if length == b'h' {
                        unsafe { *args.next_arg::<*mut i16>() = v as i16 };
                    } else {
                        unsafe { *args.next_arg::<*mut c_int>() = v as c_int };
                    }
                    assigned += 1;
                }
            }
            b'f' | b'e' | b'g' | b'E' | b'G' => {
                let mut end: *mut u8 = core::ptr::null_mut();
                let v = strtod_impl(sp as *const u8, &mut end);
                if end as *const u8 == sp as *const u8 {
                    break;
                }
                sp = end as *const c_char;
                if !suppress {
                    // long double is f64 here; `%Lf` writes the same width.
                    unsafe { *args.next_arg::<*mut f64>() = v };
                    assigned += 1;
                }
            }
            b's' => {
                let mut q = sp;
                while !matches!(
                    unsafe { *q } as u8,
                    0 | b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c
                ) && (width == 0 || (q as usize - sp as usize) < width)
                {
                    q = unsafe { q.add(1) };
                }
                if q == sp {
                    break;
                }
                if !suppress {
                    let out = unsafe { args.next_arg::<*mut c_char>() };
                    unsafe { core::ptr::copy_nonoverlapping(sp, out, q as usize - sp as usize) };
                    unsafe { *out.add(q as usize - sp as usize) = 0 };
                    assigned += 1;
                }
                sp = q;
            }
            b'c' => {
                let n = if width > 0 { width } else { 1 };
                if n == 0 {
                    break;
                }
                if !suppress {
                    let out = unsafe { args.next_arg::<*mut c_char>() };
                    unsafe { core::ptr::copy_nonoverlapping(sp, out, n) };
                    assigned += 1;
                }
                sp = unsafe { sp.add(n) };
            }
            _ => break,
        }
    }
    assigned
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fputc(c: c_int, stream: *mut FILE) -> c_int {
    let _ = stream;
    unsafe { putchar(c) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgets(s: *mut c_char, size: c_int, stream: *mut FILE) -> *mut c_char {
    if size <= 0 {
        return core::ptr::null_mut();
    }
    let mut i = 0usize;
    while i + 1 < size as usize {
        let c = unsafe { fgetc(stream) };
        if c == -1 {
            if i == 0 {
                return core::ptr::null_mut();
            }
            break;
        }
        unsafe { *s.add(i) = c as c_char };
        i += 1;
        if c == b'\n' as c_int {
            break;
        }
    }
    unsafe { *s.add(i) = 0 };
    s
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgetc(stream: *mut FILE) -> c_int {
    let st = unsafe { &mut *stream };
    if st.pushback != -1 {
        let c = st.pushback;
        st.pushback = -1;
        return c;
    }
    let mut c: u8 = 0;
    if unsafe { crate::read(0, &mut c as *mut u8 as *mut c_void, 1) } == 1 {
        c as c_int
    } else {
        -1
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getc(stream: *mut FILE) -> c_int {
    unsafe { fgetc(stream) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getchar() -> c_int {
    unsafe { fgetc(stdin) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn putc(c: c_int, stream: *mut FILE) -> c_int {
    unsafe { fputc(c, stream) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ungetc(c: c_int, stream: *mut FILE) -> c_int {
    let st = unsafe { &mut *stream };
    if c == -1 || st.pushback != -1 {
        return -1; // EOF: cannot push back EOF or a second char
    }
    st.pushback = c;
    c
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fflush(stream: *mut FILE) -> c_int {
    let _ = stream;
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fclose(stream: *mut FILE) -> c_int {
    let _ = stream;
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fileno(stream: *mut FILE) -> c_int {
    unsafe { (*stream).fd }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    #[cfg(target_os = "minix")]
    use core::ffi::{c_char, c_int};
    use std::vec::Vec;

    // Test-only entry point so the engine can be exercised without a C
    // caller. Variadic calls on the host (rustup nightly) mix pointer and
    // integer args unreliably, so these tests run on the minix target
    // (SysV va_list), where the fork's rustc is used.
    #[allow(dead_code)]
    #[cfg(target_os = "minix")]
    unsafe extern "C" fn tsnprintf(
        buf: *mut c_char,
        size: usize,
        fmt: *const c_char,
        args: ...
    ) -> c_int {
        let mut ap: VaList<'_> = args;
        format_to_buf(buf as *mut u8, size, fmt, &mut ap)
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn vsnprintf_basic() {
        let mut buf = [0u8; 64];
        let r = unsafe {
            tsnprintf(
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                b"n=%d s=%s h=%x%%".as_ptr() as *const c_char,
                42i32,
                b"hi".as_ptr() as *const c_char,
                0x1Fu32,
            )
        };
        assert_eq!(r, 13);
        assert_eq!(&buf[..13], b"n=42 s=hi h=1f%");
        assert_eq!(buf[13], 0);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn vsnprintf_padding_and_sign() {
        let mut buf = [0u8; 64];
        let r = unsafe {
            tsnprintf(
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                b"%5d|%-5d|%05d|%ld".as_ptr() as *const c_char,
                -7i32,
                7i32,
                7i32,
                -123456i64,
            )
        };
        // Values small enough to be representable as c_long on every host.
        assert_eq!(r, 5 + 1 + 5 + 1 + 5 + 1 + 7);
        assert_eq!(&buf[..r as usize], b"   -7|7    |00007|-123456");
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn vsnprintf_truncates_and_terminates() {
        let mut buf = [0u8; 8];
        let r = unsafe {
            tsnprintf(
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                b"abcdefghij".as_ptr() as *const c_char,
            )
        };
        assert_eq!(r, 10); // full length reported
        assert_eq!(&buf[..7], b"abcdefg");
        assert_eq!(buf[7], 0);
    }

    #[test]
    fn emit_num_zero_padding() {
        let mut out = Vec::new();
        let mut emit: Emit<'_> = &mut |c: u8| out.push(c);
        let n = emit_num(
            0x2A,
            16,
            b"0123456789abcdef",
            false,
            false,
            true,
            8,
            &mut emit,
        );
        // The return counts digits (+ sign); padding is emitted but not
        // counted, matching the C source.
        assert_eq!(n, 2);
        assert_eq!(out, b"0000002a");
    }
}
