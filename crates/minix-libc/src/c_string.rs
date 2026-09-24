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
static mut STRTOK_SAVE: *mut c_char = core::ptr::null_mut();

/// POSIX `strtok()`: the token after `delim`, with the resume position kept in
/// a static. Not thread-safe, like every other C library.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtok(str_: *mut c_char, delim: *const c_char) -> *mut c_char {
    let mut p = if str_.is_null() {
        unsafe { STRTOK_SAVE }
    } else {
        str_
    };
    if p.is_null() {
        return core::ptr::null_mut();
    }
    while unsafe { *p } != 0 && !unsafe { strchr(delim, *p as c_int) }.is_null() {
        p = unsafe { p.add(1) };
    }
    if unsafe { *p } == 0 {
        unsafe { STRTOK_SAVE = core::ptr::null_mut() };
        return core::ptr::null_mut();
    }
    let start = p;
    while unsafe { *p } != 0 && unsafe { strchr(delim, *p as c_int) }.is_null() {
        p = unsafe { p.add(1) };
    }
    if unsafe { *p } != 0 {
        unsafe { *p = 0 };
        p = unsafe { p.add(1) };
    }
    unsafe { STRTOK_SAVE = p };
    start
}

/// POSIX `strcoll()`: collation in the C locale is byte order, which is what
/// the port's `_l` form already does.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcoll(a: *const c_char, b: *const c_char) -> c_int {
    unsafe { strcmp(a, b) }
}

/// POSIX `strxfrm()`: copy at most `n` bytes of `src` into `dest` and return
/// the full transformed length, so a caller can size a buffer.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strxfrm(dest: *mut c_char, src: *const c_char, n: usize) -> usize {
    if src.is_null() {
        return 0;
    }
    let mut len = 0usize;
    while unsafe { *src.add(len) } != 0 {
        len += 1;
    }
    if !dest.is_null() && n > 0 {
        let copy = if len < n - 1 { len } else { n - 1 };
        let mut i = 0usize;
        while i < copy {
            unsafe { *dest.add(i) = *src.add(i) };
            i += 1;
        }
        unsafe { *dest.add(copy) = 0 };
    }
    len
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

// POSIX strings.h folds case over the whole unsigned-char range, so this is
// the ASCII fold the port's ctype uses, not a locale lookup.
#[cfg(target_os = "minix")]
fn fold_ascii_case(c: u8) -> u8 {
    if c.is_ascii_uppercase() { c | 0x20 } else { c }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcasecmp(a: *const c_char, b: *const c_char) -> c_int {
    let mut i = 0usize;
    loop {
        let ca = fold_ascii_case(unsafe { *a.add(i) } as u8);
        let cb = fold_ascii_case(unsafe { *b.add(i) } as u8);
        if ca == 0 || ca != cb {
            return ca as c_int - cb as c_int;
        }
        i += 1;
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncasecmp(a: *const c_char, b: *const c_char, mut n: usize) -> c_int {
    let mut i = 0usize;
    while n > 0 {
        let ca = fold_ascii_case(unsafe { *a.add(i) } as u8);
        let cb = fold_ascii_case(unsafe { *b.add(i) } as u8);
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

/// Sized for the longest message in `ERRNO_NAMES` (49 bytes) — a message
/// that did not fit would come back truncated, which reads as a different
/// failure than the real one.
const STRERROR_BUF_LEN: usize = 64;
static mut STRERROR_BUF: [u8; STRERROR_BUF_LEN] = [0; STRERROR_BUF_LEN];

/// `strerror` messages, indexed by errno — the numbers `tools/c-include/errno.h`
/// gives, which is the C surface a caller compares against. A `&[&[u8]]`
/// rather than a fixed-size array so the table's length cannot drift from the
/// highest errno it covers.
///
/// The empty entries are the numbers `errno.h` does not declare. They stay
/// empty rather than borrowing a neighbouring system's text: this port's
/// numbering is MINIX's for the base set and Linux's for the socket family,
/// so an unlisted number has no unambiguous meaning to describe.
///
/// `tests::every_declared_errno_has_a_message` holds this against the header.
const ERRNO_NAMES: &[&[u8]] = &[
    b"Success",                                           // 0
    b"Operation not permitted",                           // 1 EPERM
    b"No such file or directory",                         // 2 ENOENT
    b"No such process",                                   // 3 ESRCH
    b"Interrupted system call",                           // 4 EINTR
    b"I/O error",                                         // 5 EIO
    b"No such device or address",                         // 6 ENXIO
    b"Argument list too long",                            // 7 E2BIG
    b"Exec format error",                                 // 8 ENOEXEC
    b"Bad file descriptor",                               // 9 EBADF
    b"No child processes",                                // 10 ECHILD
    b"Resource temporarily unavailable",                  // 11 EAGAIN
    b"Cannot allocate memory",                            // 12 ENOMEM
    b"Permission denied",                                 // 13 EACCES
    b"Bad address",                                       // 14 EFAULT
    b"Block device required",                             // 15 ENOTBLK
    b"Device or resource busy",                           // 16 EBUSY
    b"File exists",                                       // 17 EEXIST
    b"Invalid cross-device link",                         // 18 EXDEV
    b"No such device",                                    // 19 ENODEV
    b"Not a directory",                                   // 20 ENOTDIR
    b"Is a directory",                                    // 21 EISDIR
    b"Invalid argument",                                  // 22 EINVAL
    b"Too many open files in system",                     // 23 ENFILE
    b"Too many open files",                               // 24 EMFILE
    b"Inappropriate ioctl for device",                    // 25 ENOTTY
    b"Text file busy",                                    // 26 ETXTBSY
    b"File too large",                                    // 27 EFBIG
    b"No space left on device",                           // 28 ENOSPC
    b"Illegal seek",                                      // 29 ESPIPE
    b"Read-only file system",                             // 30 EROFS
    b"Too many links",                                    // 31 EMLINK
    b"Broken pipe",                                       // 32 EPIPE
    b"Numerical argument out of domain",                  // 33 EDOM
    b"Numerical result out of range",                     // 34 ERANGE
    b"Resource deadlock avoided",                         // 35 EDEADLK
    b"File name too long",                                // 36 ENAMETOOLONG
    b"",                                                  // 37
    b"",                                                  // 38
    b"",                                                  // 39
    b"Too many levels of symbolic links",                 // 40 ELOOP
    b"",                                                  // 41
    b"",                                                  // 42
    b"",                                                  // 43
    b"",                                                  // 44
    b"",                                                  // 45
    b"",                                                  // 46
    b"",                                                  // 47
    b"",                                                  // 48
    b"",                                                  // 49
    b"",                                                  // 50
    b"",                                                  // 51
    b"",                                                  // 52
    b"",                                                  // 53
    b"",                                                  // 54
    b"",                                                  // 55
    b"",                                                  // 56
    b"",                                                  // 57
    b"",                                                  // 58
    b"",                                                  // 59
    b"",                                                  // 60
    b"",                                                  // 61
    b"",                                                  // 62
    b"",                                                  // 63
    b"",                                                  // 64
    b"",                                                  // 65
    b"",                                                  // 66
    b"",                                                  // 67
    b"",                                                  // 68
    b"",                                                  // 69
    b"",                                                  // 70
    b"",                                                  // 71
    b"",                                                  // 72
    b"",                                                  // 73
    b"",                                                  // 74
    b"Value too large for defined data type",             // 75 EOVERFLOW
    b"",                                                  // 76
    b"",                                                  // 77
    b"Function not implemented",                          // 78 ENOSYS
    b"",                                                  // 79
    b"",                                                  // 80
    b"",                                                  // 81
    b"",                                                  // 82
    b"",                                                  // 83
    b"Invalid or incomplete multibyte or wide character", // 84 EILSEQ
    b"",                                                  // 85
    b"",                                                  // 86
    b"",                                                  // 87
    b"Socket operation on non-socket",                    // 88 ENOTSOCK
    b"Destination address required",                      // 89 EDESTADDRREQ
    b"Message too long",                                  // 90 EMSGSIZE
    b"",                                                  // 91
    b"Protocol not available",                            // 92 ENOPROTOOPT
    b"Protocol not supported",                            // 93 EPROTONOSUPPORT
    b"",                                                  // 94
    b"Operation not supported",                           // 95 EOPNOTSUPP
    b"",                                                  // 96
    b"Address family not supported by protocol",          // 97 EAFNOSUPPORT
    b"Address already in use",                            // 98 EADDRINUSE
    b"Cannot assign requested address",                   // 99 EADDRNOTAVAIL
    b"Network is down",                                   // 100 ENETDOWN
    b"Network is unreachable",                            // 101 ENETUNREACH
    b"Network dropped connection on reset",               // 102 ENETRESET
    b"Software caused connection abort",                  // 103 ECONNABORTED
    b"Connection reset by peer",                          // 104 ECONNRESET
    b"No buffer space available",                         // 105 ENOBUFS
    b"Transport endpoint is already connected",           // 106 EISCONN
    b"Transport endpoint is not connected",               // 107 ENOTCONN
    b"Cannot send after transport endpoint shutdown",     // 108 ESHUTDOWN
    b"Too many references: cannot splice",                // 109 ETOOMANYREFS
    b"Connection timed out",                              // 110 ETIMEDOUT
    b"Connection refused",                                // 111 ECONNREFUSED
    b"Host is down",                                      // 112 EHOSTDOWN
    b"No route to host",                                  // 113 EHOSTUNREACH
    b"Operation already in progress",                     // 114 EALREADY
    b"Operation now in progress",                         // 115 EINPROGRESS
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
    let copy = msg.len().min(STRERROR_BUF_LEN - 1);
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

// The BSD names. They predate the POSIX ones and are still what a lot of C
// code calls (bash's globbing uses `bcopy`), but they are also what
// `strings.h` could not declare while the libc had nothing to back them.

/// BSD `bcopy()`: `memcpy()` with its arguments the other way round.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcopy(src: *const c_void, dst: *mut c_void, n: usize) {
    unsafe { core::ptr::copy(src as *const u8, dst as *mut u8, n) };
}

/// BSD `bzero()`: zero `n` bytes.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bzero(dst: *mut c_void, n: usize) {
    unsafe { core::ptr::write_bytes(dst as *mut u8, 0, n) };
}

/// BSD `bcmp()`: `memcmp()`, specified only to say equal or not.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int {
    unsafe { memcmp(a, b, n) }
}

/// BSD `index()`: `strchr()` under its old name.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn index(s: *const c_char, c: c_int) -> *mut c_char {
    unsafe { strchr(s, c) }
}

/// BSD `rindex()`: `strrchr()` under its old name.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rindex(s: *const c_char, c: c_int) -> *mut c_char {
    unsafe { strrchr(s, c) }
}

/// The position of the least significant set bit of `i`, counting from 1, or 0
/// when `i` is zero.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn ffs(i: c_int) -> c_int {
    if i == 0 {
        0
    } else {
        (i as u32).trailing_zeros() as c_int + 1
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
    fn cmp_str_n(a: &[u8], b: &[u8], n: usize) -> c_int {
        unsafe { strncasecmp(a.as_ptr() as *const c_char, b.as_ptr() as *const c_char, n) }
    }

    #[cfg(target_os = "minix")]
    fn cmp_str_case(a: &[u8], b: &[u8]) -> c_int {
        unsafe { strcasecmp(a.as_ptr() as *const c_char, b.as_ptr() as *const c_char) }
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
    fn strcasecmp_folds_ascii_case() {
        assert_eq!(cmp_str_case(b"abc\0", b"ABC\0"), 0);
        assert_eq!(cmp_str_case(b"MiXeD\0", b"mixed\0"), 0);
        assert!(cmp_str_case(b"abc\0", b"abd\0") < 0);
        assert!(cmp_str_case(b"ABD\0", b"abc\0") > 0);
        assert!(cmp_str_case(b"abc\0", b"abcd\0") < 0);
        // Non-letters are compared as-is, so case folding must not touch them.
        assert!(cmp_str_case(b"abc[\0", b"abc{\0") < 0);
    }

    #[cfg(target_os = "minix")]
    #[test]
    fn strncasecmp_stops_at_n() {
        assert_eq!(cmp_str_n(b"abc\0", b"abd\0", 2), 0);
        assert!(cmp_str_n(b"abc\0", b"abd\0", 3) < 0);
        assert_eq!(cmp_str_n(b"ABC\0", b"abc\0", 4), 0);
        assert_eq!(cmp_str_n(b"abc\0", b"xyz\0", 0), 0);
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

    // The errno table is checked against the C header rather than against a
    // second copy of the numbers: the header is the contract a C caller
    // compiles against, and a table that drifted from it would answer for a
    // number the program does not mean.

    use super::{ERRNO_NAMES, STRERROR_BUF_LEN};

    /// Every errno `errno.h` declares needs a message. A program printing
    /// `strerror(ENOSYS)` must be told what happened — bash's
    /// `shell-init: error retrieving current directory` reported
    /// "Unknown error" because this table stopped at 34.
    #[test]
    fn every_declared_errno_has_a_message() {
        let header = include_str!("../../../tools/c-include/errno.h");
        let mut checked = 0;
        for line in header.lines() {
            let Some(rest) = line.strip_prefix("#define E") else {
                continue;
            };
            let mut fields = rest.split_whitespace();
            let name = fields.next().unwrap_or("");
            let Some(value) = fields.next() else {
                continue;
            };
            // `#define EWOULDBLOCK EAGAIN` names another errno, not a number.
            let Ok(number) = value.parse::<usize>() else {
                continue;
            };
            assert!(
                number < ERRNO_NAMES.len(),
                "{name} = {number} is past the end of ERRNO_NAMES ({} entries)",
                ERRNO_NAMES.len()
            );
            assert!(
                !ERRNO_NAMES[number].is_empty(),
                "{name} = {number} has no strerror message"
            );
            checked += 1;
        }
        // A parse that matched nothing would pass while checking nothing,
        // which is the failure this assertion exists to prevent.
        assert!(
            checked >= 60,
            "only {checked} errno definitions were read from the header"
        );
    }

    /// `strerror` copies into a fixed buffer, so a message that did not fit
    /// would come back truncated — a different failure than the real one.
    #[test]
    fn no_errno_message_outgrows_the_strerror_buffer() {
        let room = STRERROR_BUF_LEN - 1; // one byte for the terminator
        for (number, message) in ERRNO_NAMES.iter().enumerate() {
            assert!(
                message.len() <= room,
                "errno {number} needs {} bytes, the buffer leaves {room}",
                message.len()
            );
        }
    }
}
