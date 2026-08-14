//! C string.h/ctype.h helpers — ported from the old `tools/c-libc.c`
//! plus the commonly-referenced functions libc++/LLVM need (`memchr`,
//! `strrchr`, `strstr`, `strcat`, `strerror`, ...). ASCII-only ctype.

#[cfg(target_os = "minix")]
use core::ffi::{c_char, c_int, c_void};

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int {
    let x = a as *const u8;
    let y = b as *const u8;
    for i in 0..n {
        let (cx, cy) = unsafe { (*x.add(i), *y.add(i)) };
        if cx != cy {
            return cx as c_int - cy as c_int;
        }
    }
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memchr(s: *const c_void, c: c_int, n: usize) -> *mut c_void {
    let p = s as *const u8;
    for i in 0..n {
        if unsafe { *p.add(i) } == c as u8 {
            return unsafe { p.add(i) } as *mut c_void;
        }
    }
    core::ptr::null_mut()
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcmp(a: *const c_char, b: *const c_char) -> c_int {
    let mut i = 0usize;
    loop {
        let ca = unsafe { *a.add(i) } as u8;
        let cb = unsafe { *b.add(i) } as u8;
        if ca == 0 || ca != cb {
            return ca as c_int - cb as c_int;
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncmp(a: *const c_char, b: *const c_char, mut n: usize) -> c_int {
    let mut i = 0usize;
    while n > 0 {
        let ca = unsafe { *a.add(i) } as u8;
        let cb = unsafe { *b.add(i) } as u8;
        if ca == 0 || ca != cb {
            return ca as c_int - cb as c_int;
        }
        i += 1;
        n -= 1;
    }
    0
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char {
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
pub unsafe extern "C" fn strncpy(
    dst: *mut c_char,
    src: *const c_char,
    mut n: usize,
) -> *mut c_char {
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
pub unsafe extern "C" fn strcat(dst: *mut c_char, src: *const c_char) -> *mut c_char {
    let base = dst;
    let mut d = 0usize;
    while unsafe { *dst.add(d) } != 0 {
        d += 1;
    }
    unsafe { strcpy(dst.add(d), src) };
    base
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncat(
    dst: *mut c_char,
    src: *const c_char,
    mut n: usize,
) -> *mut c_char {
    let base = dst;
    let mut d = 0usize;
    while unsafe { *dst.add(d) } != 0 {
        d += 1;
    }
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
    base
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strchr(s: *const c_char, c: c_int) -> *mut c_char {
    let mut i = 0usize;
    loop {
        let sc = unsafe { *s.add(i) } as u8;
        if sc == c as u8 {
            return unsafe { s.add(i) } as *mut c_char;
        }
        if sc == 0 {
            return core::ptr::null_mut();
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strrchr(s: *const c_char, c: c_int) -> *mut c_char {
    let mut i = 0usize;
    let mut found: *mut c_char = core::ptr::null_mut();
    loop {
        let sc = unsafe { *s.add(i) } as u8;
        if sc == 0 {
            if c as u8 == 0 {
                return unsafe { s.add(i) } as *mut c_char;
            }
            return found;
        }
        if sc == c as u8 {
            found = unsafe { s.add(i) } as *mut c_char;
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strstr(haystack: *const c_char, needle: *const c_char) -> *mut c_char {
    if unsafe { *needle } == 0 {
        return haystack as *mut c_char;
    }
    let mut h = 0usize;
    loop {
        let mut hi = h;
        let mut ni = 0usize;
        loop {
            let hc = unsafe { *haystack.add(hi) };
            let nc = unsafe { *needle.add(ni) };
            if nc == 0 {
                return unsafe { haystack.add(h) } as *mut c_char;
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
pub unsafe extern "C" fn strpbrk(s: *const c_char, accept: *const c_char) -> *mut c_char {
    let mut i = 0usize;
    loop {
        let sc = unsafe { *s.add(i) } as u8;
        if sc == 0 {
            return core::ptr::null_mut();
        }
        let mut a = 0usize;
        loop {
            let ac = unsafe { *accept.add(a) } as u8;
            if ac == 0 {
                break;
            }
            if sc == ac {
                return unsafe { s.add(i) } as *mut c_char;
            }
            a += 1;
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strspn(s: *const c_char, accept: *const c_char) -> usize {
    let mut n = 0usize;
    loop {
        let c = unsafe { *s.add(n) } as u8;
        if c == 0 {
            break;
        }
        if unsafe { strchr(accept, c as c_int) }.is_null() {
            break;
        }
        n += 1;
    }
    n
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcspn(s: *const c_char, reject: *const c_char) -> usize {
    let mut n = 0usize;
    loop {
        let c = unsafe { *s.add(n) } as u8;
        if c == 0 {
            break;
        }
        if !unsafe { strchr(reject, c as c_int) }.is_null() {
            break;
        }
        n += 1;
    }
    n
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strnlen(s: *const c_char, maxlen: usize) -> usize {
    let mut n = 0usize;
    while n < maxlen && unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

static mut STRERROR_BUF: [u8; 32] = [0; 32];

const ERRNO_NAMES: [&[u8]; 35] = [
    b"Success",
    b"Operation not permitted",
    b"No such file or directory",
    b"No such process",
    b"Interrupted system call",
    b"I/O error",
    b"No such device or address",
    b"", // 7: unused
    b"", // 8: unused
    b"Bad file descriptor",
    b"", // 10: unused
    b"Resource temporarily unavailable",
    b"Cannot allocate memory",
    b"Permission denied",
    b"Bad address",
    b"", // 15: unused
    b"Device or resource busy",
    b"File exists",
    b"", // 18: unused
    b"No such device",
    b"Not a directory",
    b"Is a directory",
    b"Invalid argument",
    b"", // 23: unused
    b"", // 24: unused
    b"", // 25: unused
    b"", // 26: unused
    b"", // 27: unused
    b"No space left on device",
    b"", // 29: unused
    b"", // 30: unused
    b"", // 31: unused
    b"", // 32: unused
    b"Numerical argument out of domain",
    b"Numerical result out of range",
];

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strerror(errnum: c_int) -> *mut c_char {
    let msg: &[u8] = if errnum >= 0 && (errnum as usize) < ERRNO_NAMES.len() {
        let m = ERRNO_NAMES[errnum as usize];
        if m.is_empty() { b"Unknown error" } else { m }
    } else {
        b"Unknown error"
    };
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(STRERROR_BUF) };
    let copy = msg.len().min(buf.len() - 1);
    buf[..copy].copy_from_slice(&msg[..copy]);
    buf[copy] = 0;
    buf.as_mut_ptr() as *mut c_char
}

// ---- ctype.h (ASCII-only) ----

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isalpha(c: c_int) -> c_int {
    ((c >= b'a' as c_int && c <= b'z' as c_int) || (c >= b'A' as c_int && c <= b'Z' as c_int))
        as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isdigit(c: c_int) -> c_int {
    (c >= b'0' as c_int && c <= b'9' as c_int) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isalnum(c: c_int) -> c_int {
    unsafe { isalpha(c) | isdigit(c) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isspace(c: c_int) -> c_int {
    matches!(c as u8, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'\x0b') as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn islower(c: c_int) -> c_int {
    (c >= b'a' as c_int && c <= b'z' as c_int) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isupper(c: c_int) -> c_int {
    (c >= b'A' as c_int && c <= b'Z' as c_int) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isxdigit(c: c_int) -> c_int {
    (unsafe { isdigit(c) != 0 }
        || (c >= b'a' as c_int && c <= b'f' as c_int)
        || (c >= b'A' as c_int && c <= b'F' as c_int)) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ispunct(c: c_int) -> c_int {
    ((c >= b'!' as c_int && c <= b'/' as c_int)
        || (c >= b':' as c_int && c <= b'@' as c_int)
        || (c >= b'[' as c_int && c <= b'`' as c_int)
        || (c >= b'{' as c_int && c <= b'~' as c_int)) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isgraph(c: c_int) -> c_int {
    (c >= b'!' as c_int && c <= b'~' as c_int) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isprint(c: c_int) -> c_int {
    (c >= b' ' as c_int && c <= b'~' as c_int) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iscntrl(c: c_int) -> c_int {
    ((c >= 0 && c < b' ' as c_int) || c == 127) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isblank(c: c_int) -> c_int {
    (c == b' ' as c_int || c == b'\t' as c_int) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tolower(c: c_int) -> c_int {
    if unsafe { isupper(c) != 0 } {
        c + (b'a' as c_int - b'A' as c_int)
    } else {
        c
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn toupper(c: c_int) -> c_int {
    if unsafe { islower(c) != 0 } {
        c - (b'a' as c_int - b'A' as c_int)
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "minix")]
    use super::*;
    #[cfg(target_os = "minix")]
    use core::ffi::{c_char, c_int, c_void};

    // The string/ctype exports are minix-only; the logic is trivial
    // memcmp-style loops, exercised on target.
    #[cfg(target_os = "minix")]
    fn cmp_str(a: &[u8], b: &[u8]) -> c_int {
        unsafe { strcmp(a.as_ptr() as *const c_char, b.as_ptr() as *const c_char) }
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn strcmp_cases() {
        assert_eq!(cmp_str(b"abc\0", b"abc\0"), 0);
        assert!(cmp_str(b"abc\0", b"abd\0") < 0);
        assert!(cmp_str(b"abd\0", b"abc\0") > 0);
        assert!(cmp_str(b"abc\0", b"abcd\0") < 0);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn memchr_finds() {
        let s = b"hello world";
        let r = unsafe { memchr(s.as_ptr() as *const c_void, b'w' as c_int, s.len()) };
        assert_eq!(r, unsafe { s.as_ptr().add(6) } as *mut c_void);
        let miss = unsafe { memchr(s.as_ptr() as *const c_void, b'z' as c_int, s.len()) };
        assert!(miss.is_null());
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn strchr_handles_nul_match() {
        let s = b"abc";
        let r = unsafe { strchr(s.as_ptr() as *const c_char, 0) };
        assert_eq!(r, unsafe { s.as_ptr().add(3) } as *mut c_char);
    }
}
