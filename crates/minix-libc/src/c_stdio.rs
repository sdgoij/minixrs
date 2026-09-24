//! C stdio: unbuffered FILE streams and the printf family.
//!
//! The format engine is a faithful port of the old `tools/c-libc.c`
//! `vformat`, keeping the same conversion set (`%d %i %u %x %X %p %s %c
//! %%`, with `l`/`z` length prefixes) and the same (ASCII-only) behavior.

#[cfg(target_os = "minix")]
use core::ffi::VaList;
use core::ffi::{c_char, c_int};
#[cfg(target_os = "minix")]
use core::ffi::{c_long, c_ulong, c_void};

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

/// `FILE.flags` bits: the mode's read/write mask, plus the sticky pair that
/// `feof`/`ferror` report and `clearerr` clears.
const F_READ: c_int = 0x1;
const F_WRITE: c_int = 0x2;
#[cfg(target_os = "minix")]
const F_EOF: c_int = 0x4;
#[cfg(target_os = "minix")]
const F_ERR: c_int = 0x8;

/// Opaque-in-C `FILE`: an fd, the mode/status word, and the `ungetc` pushback
/// stack. Streams are unbuffered, so there is no buffer to flush and `fflush`
/// stays a no-op.
#[repr(C)]
#[allow(clippy::upper_case_acronyms)]
pub struct FILE {
    fd: c_int,
    flags: c_int,
    owns_fd: c_int, // set by fopen/fdopen: fclose closes and frees the stream
    // C requires `ungetc` to work for at least one character and allows
    // successive pushes, so this is a small stack rather than one slot. It is
    // what a stream-based `scanf` needs to over-read and give back the tail.
    pushback: [u8; 8],
    pushback_len: usize,
}

static mut _STDIN: FILE = FILE {
    fd: 0,
    flags: F_READ,
    owns_fd: 0,
    pushback: [0; 8],
    pushback_len: 0,
};
static mut _STDOUT: FILE = FILE {
    fd: 1,
    flags: F_WRITE,
    owns_fd: 0,
    pushback: [0; 8],
    pushback_len: 0,
};
static mut _STDERR: FILE = FILE {
    fd: 2,
    flags: F_WRITE,
    owns_fd: 0,
    pushback: [0; 8],
    pushback_len: 0,
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

#[cfg(target_os = "minix")]
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

#[cfg(target_os = "minix")]
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

/// POSIX `vsprintf()`: the `va_list` form of `sprintf`. Unbounded, like the C
/// function it replaces -- `vsnprintf` is the one to call with a size.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsprintf(str_: *mut c_char, fmt: *const c_char, ap: VaList<'_>) -> c_int {
    unsafe { vsnprintf(str_, usize::MAX, fmt, ap) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vfprintf(
    stream: *mut FILE,
    fmt: *const c_char,
    mut ap: VaList<'_>,
) -> c_int {
    // A null stream has no fd to honour; C leaves that undefined, so fall
    // back to stdout rather than dereferencing it.
    let fd = if stream.is_null() {
        1
    } else {
        unsafe { (*stream).fd }
    };
    let mut emit = |c: u8| {
        let b = c;
        let _ = unsafe { crate::write(fd, &b as *const u8 as *const c_void, 1) };
    };
    unsafe { vformat(fmt, &mut ap, &mut emit) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fprintf(stream: *mut FILE, fmt: *const c_char, args: ...) -> c_int {
    unsafe { vfprintf(stream, fmt, args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fputs(s: *const c_char, stream: *mut FILE) -> c_int {
    if s.is_null() || stream.is_null() {
        return -1;
    }
    let n = c_strlen(s);
    if unsafe { crate::write((*stream).fd, s as *const c_void, n) } == n as isize {
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

/// The input a scan directive reads from, one character at a time. `sscanf`
/// reads a NUL-terminated string and `fscanf` a stream, so the directive walk
/// below is written once against this and instantiated for each.
trait Scan {
    /// The next character, or `None` at end of input.
    fn read(&mut self) -> Option<u8>;

    /// Gives `c` back to the input. Returns false when the source cannot hold
    /// it; the scanner never has more than two characters outstanding.
    fn unread(&mut self, c: u8) -> bool;
}

/// A scan source over a NUL-terminated C string, as `sscanf` reads: the first
/// NUL ends the input, embedded ones included.
struct StrScan {
    p: *const c_char,
}

impl Scan for StrScan {
    fn read(&mut self) -> Option<u8> {
        let c = unsafe { *self.p } as u8;
        if c == 0 {
            None
        } else {
            self.p = unsafe { self.p.add(1) };
            Some(c)
        }
    }

    fn unread(&mut self, _c: u8) -> bool {
        self.p = unsafe { self.p.sub(1) };
        true
    }
}

/// A scan source over a stream, as `fscanf` reads. It goes through `fgetc`/
/// `ungetc`, so the stream's own pushback stack holds the lookahead.
#[cfg(target_os = "minix")]
struct FileScan {
    stream: *mut FILE,
}

#[cfg(target_os = "minix")]
impl Scan for FileScan {
    fn read(&mut self) -> Option<u8> {
        let c = unsafe { fgetc(self.stream) };
        if c < 0 { None } else { Some(c as u8) }
    }

    fn unread(&mut self, c: u8) -> bool {
        (unsafe { ungetc(c as c_int, self.stream) }) >= 0
    }
}

/// Input position, the character count `%n` reports, and whether the input has
/// run out -- which is what separates an input failure from a matching one.
struct Scanner<S: Scan> {
    src: S,
    read: usize,
    eof: bool,
}

impl<S: Scan> Scanner<S> {
    fn get(&mut self) -> Option<u8> {
        match self.src.read() {
            Some(c) => {
                self.read += 1;
                Some(c)
            }
            None => {
                self.eof = true;
                None
            }
        }
    }

    fn put(&mut self, c: u8) {
        let held = self.src.unread(c);
        debug_assert!(held, "scan pushback overflow");
        if held {
            self.read -= 1;
        }
    }

    /// One character of lookahead: read it, then give it straight back.
    fn peek(&mut self) -> Option<u8> {
        let c = self.get()?;
        self.put(c);
        Some(c)
    }

    /// The format's whitespace directive: any run of input whitespace,
    /// possibly empty, is left unread.
    fn skip_space(&mut self) {
        while let Some(c) = self.peek() {
            if !is_scan_space(c) {
                break;
            }
            let _ = self.get();
        }
    }
}

/// The whitespace a directive skips: C's `isspace` set in the C locale.
fn is_scan_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// The value of a digit in a base up to 16, or `None` when `c` is not one.
fn digit_value(c: u8) -> Option<u64> {
    match c {
        b'0'..=b'9' => Some(u64::from(c - b'0')),
        b'a'..=b'f' => Some(u64::from(c - b'a') + 10),
        b'A'..=b'F' => Some(u64::from(c - b'A') + 10),
        _ => None,
    }
}

/// A ten-power large enough to overflow an f64, so clamping the exponent here
/// keeps the scale loop bounded without changing the result.
const SCAN_MAX_EXP: i32 = 400;

/// Reads an integer field: an optional sign, an optional `0x` or `0` prefix,
/// then digits. `base` is 0 for `%i`, which takes the base from the prefix.
///
/// The value carries the field's sign already applied, wrapping as C requires
/// of a negative field converted to an unsigned type, so the caller only has
/// to cast it to the width its length modifier selects. `None` means no digit
/// was converted, and leaves the input at the start of the field.
fn scan_uint<S: Scan>(sc: &mut Scanner<S>, width: usize, base: i32) -> Option<u64> {
    let full = |used: usize| width != 0 && used >= width;
    let mut used = 0usize;

    let mut sign = 0u8;
    let mut neg = false;
    if !full(used)
        && let Some(c) = sc.peek()
        && matches!(c, b'+' | b'-')
    {
        let _ = sc.get();
        sign = c;
        neg = c == b'-';
        used += 1;
    }

    let mut base = base;
    let mut digits = 0usize;

    // A leading `0` introduces an octal field and `0x` a hexadecimal one:
    // `%i` picks the base from that prefix, and `%x` accepts the same prefix.
    if !full(used) && sc.peek() == Some(b'0') {
        let _ = sc.get();
        used += 1;
        digits = 1;
        if matches!(base, 0 | 16) {
            if !full(used) && matches!(sc.peek(), Some(b'x' | b'X')) {
                let x = match sc.get() {
                    Some(c) => c,
                    None => return Some(0),
                };
                used += 1;
                if !full(used) && sc.peek().is_some_and(|c| digit_value(c).is_some()) {
                    base = 16;
                } else {
                    // No hex digit follows, so the field was just the `0` and
                    // the `x` belongs to whatever comes next.
                    sc.put(x);
                    return Some(0);
                }
            } else if base == 0 {
                base = 8;
            }
        }
    }
    if base == 0 {
        base = 10;
    }

    let mut v = 0u64;
    loop {
        if full(used) {
            break;
        }
        let Some(c) = sc.peek() else { break };
        let Some(d) = digit_value(c) else { break };
        if d >= base as u64 {
            break;
        }
        let _ = sc.get();
        used += 1;
        v = v.wrapping_mul(base as u64).wrapping_add(d);
        digits += 1;
    }

    if digits == 0 {
        // Nothing but the sign was read; give it back so the next directive
        // starts where this one did.
        if sign != 0 {
            sc.put(sign);
        }
        return None;
    }
    Some(if neg { v.wrapping_neg() } else { v })
}

/// Reads a floating-point field: sign, mantissa (digits with at most one `.`),
/// then an exponent. Digits after `e` are what make it an exponent, so `1e`
/// scans as `1` -- the longest prefix that could match.
///
/// Decimal notation only: `inf` and `nan` are not accepted, matching this
/// crate's `strtod`. `None` means no mantissa digit was converted, and leaves
/// the input at the start of the field.
fn scan_float<S: Scan>(sc: &mut Scanner<S>, width: usize) -> Option<f64> {
    let full = |used: usize| width != 0 && used >= width;
    let mut used = 0usize;

    let mut sign = 0u8;
    let mut neg = false;
    if !full(used)
        && let Some(c) = sc.peek()
        && matches!(c, b'+' | b'-')
    {
        let _ = sc.get();
        sign = c;
        neg = c == b'-';
        used += 1;
    }

    let mut mant = 0f64;
    let mut scale = 1f64;
    let mut digits = 0usize;
    let mut dot = false;
    loop {
        if full(used) {
            break;
        }
        let Some(c) = sc.peek() else { break };
        if c.is_ascii_digit() {
            let _ = sc.get();
            used += 1;
            digits += 1;
            if dot {
                scale *= 0.1;
                mant += f64::from(c - b'0') * scale;
            } else {
                mant = mant * 10.0 + f64::from(c - b'0');
            }
        } else if c == b'.' && !dot {
            let _ = sc.get();
            used += 1;
            dot = true;
        } else {
            break;
        }
    }

    if digits > 0 && !full(used) && matches!(sc.peek(), Some(b'e' | b'E')) {
        let _ = sc.get();
        used += 1;
        let mut esign = 0u8;
        if !full(used)
            && let Some(c) = sc.peek()
            && matches!(c, b'+' | b'-')
        {
            let _ = sc.get();
            esign = c;
            used += 1;
        }
        let mut exp = 0i32;
        let mut edigits = 0usize;
        loop {
            if full(used) {
                break;
            }
            let Some(c) = sc.peek() else { break };
            if !c.is_ascii_digit() {
                break;
            }
            let _ = sc.get();
            used += 1;
            exp = exp.saturating_mul(10).saturating_add(i32::from(c - b'0'));
            edigits += 1;
        }
        if edigits == 0 {
            // No digits after the `e`, so there was no exponent: put the
            // lookahead back. The field ends with the mantissa.
            if esign != 0 {
                sc.put(esign);
            }
            sc.put(b'e');
        } else {
            let mut m = 1f64;
            let mut k = exp.min(SCAN_MAX_EXP);
            while k > 0 {
                m *= 10.0;
                k -= 1;
            }
            mant = if esign == b'-' { mant / m } else { mant * m };
        }
    }

    if digits == 0 {
        if dot {
            sc.put(b'.');
        }
        if sign != 0 {
            sc.put(sign);
        }
        return None;
    }
    Some(if neg { -mant } else { mant })
}

/// The length modifier of a directive. For the integer conversions it selects
/// how wide the object its argument points to is; signedness never enters into
/// it, because the parsed value already carries the field's sign and the store
/// truncates by cast.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Length {
    None,
    Hh,
    H,
    L,
    Ll,
    J,
    Z,
    T,
    /// `L`: `long double`, which is f64 on this target.
    Big,
}

/// Reads a length modifier at `*p`, advancing `*p` past it.
unsafe fn parse_length(p: &mut *const c_char) -> Length {
    let c = unsafe { **p } as u8;
    let length = match c {
        b'h' => Length::H,
        b'l' => Length::L,
        b'j' => Length::J,
        b'z' => Length::Z,
        b't' => Length::T,
        b'L' => Length::Big,
        // BSD spells `long long` `q`, and glibc accepts it too.
        b'q' => Length::Ll,
        _ => return Length::None,
    };
    let doubled = usize::from(matches!(c, b'h' | b'l') && unsafe { *(*p).add(1) } as u8 == c);
    *p = unsafe { (*p).add(1 + doubled) };
    match (c, doubled) {
        (b'h', 1) => Length::Hh,
        (b'l', 1) => Length::Ll,
        _ => length,
    }
}

/// Sets one bit of a byte-membership table indexed by character value.
fn set_bit(set: &mut [u64; 4], c: u8) {
    set[(c >> 6) as usize] |= 1 << (c & 63);
}

/// Whether `c` is a member of `set`.
fn set_has(set: &[u64; 4], c: u8) -> bool {
    set[(c >> 6) as usize] & (1 << (c & 63)) != 0
}

/// Parses a `%[...]` scanset. A `^` first negates the set; a `]` in the first
/// position is a member rather than the terminator, and so is a `-` with
/// nothing to range over. Returns the membership table, the negation, and the
/// format position after the closing bracket.
fn parse_scanset(p: *const c_char) -> ([u64; 4], bool, *const c_char) {
    let mut set = [0u64; 4];
    let mut p = p;
    let negate = unsafe { *p } as u8 == b'^';
    if negate {
        p = unsafe { p.add(1) };
    }
    let mut first = true;
    loop {
        let c = unsafe { *p } as u8;
        if c == 0 {
            break;
        }
        p = unsafe { p.add(1) };
        if c == b']' && !first {
            break;
        }
        first = false;
        let next = unsafe { *p } as u8;
        let end = unsafe { *p.add(1) } as u8;
        if next == b'-' && end != b']' && end != 0 {
            for ch in c..=end {
                set_bit(&mut set, ch);
            }
            p = unsafe { p.add(2) };
        } else {
            set_bit(&mut set, c);
        }
    }
    (set, negate, p)
}

/// Stores a converted integer through the argument its length modifier
/// selects. The value arrives with the field's sign already applied, so the
/// truncating cast is the whole conversion and the arms differ only in the
/// width of the pointer they read out of the argument list.
#[cfg(target_os = "minix")]
macro_rules! assign_int {
    ($args:expr, $length:expr, $v:expr) => {
        match $length {
            Length::Hh => unsafe { *$args.next_arg::<*mut i8>() = $v as i8 },
            Length::H => unsafe { *$args.next_arg::<*mut i16>() = $v as i16 },
            Length::L => unsafe { *$args.next_arg::<*mut c_long>() = $v as c_long },
            Length::Ll | Length::J => unsafe { *$args.next_arg::<*mut i64>() = $v as i64 },
            Length::Z | Length::T => unsafe { *$args.next_arg::<*mut isize>() = $v as isize },
            Length::None | Length::Big => unsafe { *$args.next_arg::<*mut c_int>() = $v as c_int },
        }
    };
}

/// Reads a text field -- `%s` and `%[` -- and stores it NUL-terminated. The
/// argument is taken on the field's first character, so a failed directive
/// leaves both the caller's object and the argument list where they were.
/// Returns the field length; zero is a matching failure.
#[cfg(target_os = "minix")]
unsafe fn scan_text<S: Scan, F: Fn(u8) -> bool>(
    sc: &mut Scanner<S>,
    width: usize,
    accept: F,
    suppress: bool,
    args: &mut VaList<'_>,
) -> usize {
    let mut dest: *mut c_char = core::ptr::null_mut();
    let mut n = 0usize;
    loop {
        if width != 0 && n >= width {
            break;
        }
        let Some(c) = sc.peek() else { break };
        if !accept(c) {
            break;
        }
        let _ = sc.get();
        if n == 0 && !suppress {
            dest = unsafe { args.next_arg::<*mut c_char>() };
        }
        if !dest.is_null() {
            unsafe { *dest.add(n) = c as c_char };
        }
        n += 1;
    }
    if n > 0 && !dest.is_null() {
        unsafe { *dest.add(n) = 0 };
    }
    n
}

/// Runs the format's directives: reads fields from `sc` and stores each
/// converted value through the next argument. Whitespace in the format
/// matches any run of input whitespace, any other character must match
/// exactly, and the scan stops at the first failure.
///
/// Returns the number of assignments, or `EOF` when the input ran out before
/// the first one completed.
#[cfg(target_os = "minix")]
unsafe fn scan_directives<S: Scan>(
    sc: &mut Scanner<S>,
    fmt: *const c_char,
    args: &mut VaList<'_>,
) -> c_int {
    let mut p = fmt;
    let mut assigned = 0i32;
    loop {
        let c = unsafe { *p } as u8;
        if c == 0 {
            break;
        }
        p = unsafe { p.add(1) };
        if c != b'%' {
            if is_scan_space(c) {
                sc.skip_space();
                continue;
            }
            match sc.get() {
                Some(ch) if ch == c => continue,
                Some(ch) => {
                    sc.put(ch);
                    break;
                }
                None => break,
            }
        }

        let mut suppress = false;
        if unsafe { *p } as u8 == b'*' {
            suppress = true;
            p = unsafe { p.add(1) };
        }
        let mut width = 0usize;
        loop {
            let c = unsafe { *p } as u8;
            if !c.is_ascii_digit() {
                break;
            }
            width = width
                .saturating_mul(10)
                .saturating_add(usize::from(c - b'0'));
            p = unsafe { p.add(1) };
        }
        let length = unsafe { parse_length(&mut p) };
        let conv = unsafe { *p } as u8;
        if conv == 0 {
            break;
        }
        p = unsafe { p.add(1) };

        match conv {
            b'%' => match sc.get() {
                Some(b'%') => {}
                Some(ch) => {
                    sc.put(ch);
                    break;
                }
                None => break,
            },
            b'n' => {
                // `%n` reads no input: it is the one conversion that assigns
                // without a field.
                if !suppress {
                    assign_int!(args, length, sc.read);
                }
            }
            b'd' | b'u' | b'i' | b'o' | b'x' | b'X' => {
                sc.skip_space();
                let base = match conv {
                    b'o' => 8,
                    b'x' | b'X' => 16,
                    b'i' => 0,
                    _ => 10,
                };
                let Some(v) = scan_uint(sc, width, base) else {
                    break;
                };
                if !suppress {
                    assign_int!(args, length, v);
                    assigned += 1;
                }
            }
            b'f' | b'e' | b'g' | b'a' | b'E' | b'G' | b'A' => {
                sc.skip_space();
                let Some(v) = scan_float(sc, width) else {
                    break;
                };
                if !suppress {
                    // long double is f64 here; `%Lf` writes the same width.
                    unsafe { *args.next_arg::<*mut f64>() = v };
                    assigned += 1;
                }
            }
            b'p' => {
                sc.skip_space();
                let Some(v) = scan_uint(sc, width, 16) else {
                    break;
                };
                if !suppress {
                    // `%p` takes a `void **`, so the value is stored through
                    // one more level of indirection than the other integers.
                    let dest = unsafe { args.next_arg::<*mut *mut c_void>() };
                    unsafe { *dest = core::ptr::with_exposed_provenance_mut(v as usize) };
                    assigned += 1;
                }
            }
            b's' => {
                sc.skip_space();
                let n = unsafe { scan_text(sc, width, |c| !is_scan_space(c), suppress, args) };
                if n == 0 {
                    break;
                }
                if !suppress {
                    assigned += 1;
                }
            }
            b'[' => {
                let (set, negate, rest) = parse_scanset(p);
                p = rest;
                let n =
                    unsafe { scan_text(sc, width, |c| set_has(&set, c) != negate, suppress, args) };
                if n == 0 {
                    break;
                }
                if !suppress {
                    assigned += 1;
                }
            }
            b'c' => {
                // `%c` reads exactly the field width -- one character by
                // default -- verbatim and unterminated.
                let n = if width == 0 { 1 } else { width };
                let mut dest: *mut c_char = core::ptr::null_mut();
                let mut got = 0usize;
                while got < n {
                    let Some(ch) = sc.get() else { break };
                    if got == 0 && !suppress {
                        dest = unsafe { args.next_arg::<*mut c_char>() };
                    }
                    if !dest.is_null() {
                        unsafe { *dest.add(got) = ch as c_char };
                    }
                    got += 1;
                }
                // A short field is a matching failure. Where the input then
                // sits is unspecified, so the characters read stay consumed.
                if got < n {
                    break;
                }
                if !dest.is_null() {
                    assigned += 1;
                }
            }
            // An unknown conversion is undefined in C; ending the scan is the
            // conservative reading of it.
            _ => break,
        }
    }
    if assigned == 0 && sc.eof {
        // `EOF` is -1: an input failure before any conversion completed.
        return -1;
    }
    assigned
}

/// POSIX `vsscanf()`: the `va_list` form of `sscanf`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsscanf(
    s: *const c_char,
    fmt: *const c_char,
    mut args: VaList<'_>,
) -> c_int {
    if s.is_null() || fmt.is_null() {
        return -1;
    }
    let mut sc = Scanner {
        src: StrScan { p: s },
        read: 0,
        eof: false,
    };
    unsafe { scan_directives(&mut sc, fmt, &mut args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sscanf(s: *const c_char, fmt: *const c_char, args: ...) -> c_int {
    unsafe { vsscanf(s, fmt, args) }
}

/// POSIX `vfscanf()`: the `va_list` form of `fscanf`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vfscanf(
    stream: *mut FILE,
    fmt: *const c_char,
    mut args: VaList<'_>,
) -> c_int {
    if stream.is_null() || fmt.is_null() {
        return -1;
    }
    let mut sc = Scanner {
        src: FileScan { stream },
        read: 0,
        eof: false,
    };
    unsafe { scan_directives(&mut sc, fmt, &mut args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fscanf(stream: *mut FILE, fmt: *const c_char, args: ...) -> c_int {
    unsafe { vfscanf(stream, fmt, args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vscanf(fmt: *const c_char, args: VaList<'_>) -> c_int {
    unsafe { vfscanf(stdin, fmt, args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scanf(fmt: *const c_char, args: ...) -> c_int {
    unsafe { vscanf(fmt, args) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fputc(c: c_int, stream: *mut FILE) -> c_int {
    if stream.is_null() {
        return -1;
    }
    let st = unsafe { &mut *stream };
    let b = c as u8;
    if unsafe { crate::write(st.fd, &b as *const u8 as *const c_void, 1) } == 1 {
        b as c_int
    } else {
        st.flags |= F_ERR;
        -1
    }
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
    if stream.is_null() {
        return -1;
    }
    let st = unsafe { &mut *stream };
    if st.pushback_len > 0 {
        st.pushback_len -= 1;
        return st.pushback[st.pushback_len] as c_int;
    }
    let mut c: u8 = 0;
    let n = unsafe { crate::read(st.fd, &mut c as *mut u8 as *mut c_void, 1) };
    if n == 1 {
        c as c_int
    } else if n < 0 {
        st.flags |= F_ERR;
        -1
    } else {
        st.flags |= F_EOF;
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
    if c == -1 || st.pushback_len == st.pushback.len() {
        return -1; // EOF, or the pushback stack is full
    }
    st.pushback[st.pushback_len] = c as u8;
    st.pushback_len += 1;
    c
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fflush(stream: *mut FILE) -> c_int {
    let _ = stream;
    0
}

/// POSIX `setlinebuf()`: make a stream line-buffered. Every stream here is
/// unbuffered already, which is what line buffering asks for — a line cannot
/// wait in a buffer that does not exist — so there is nothing to change.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setlinebuf(stream: *mut FILE) {
    let _ = stream;
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fclose(stream: *mut FILE) -> c_int {
    if stream.is_null() {
        return -1;
    }
    let st = unsafe { &mut *stream };
    // Only fopen/fdopen streams own their fd and their own storage; the three
    // stdio statics are neither closed nor freed.
    let owned = st.owns_fd != 0;
    let r = if owned { crate::close(st.fd) } else { 0 };
    if owned {
        unsafe { free(stream as *mut c_void) };
    }
    r
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fileno(stream: *mut FILE) -> c_int {
    unsafe { (*stream).fd }
}

/// Translate a stdio mode string into `open` flags and the stream's own
/// read/write mask. `None` for a mode C leaves undefined. The flag values
/// mirror tools/c-include/fcntl.h.
#[cfg(target_os = "minix")]
unsafe fn mode_flags(mode: *const c_char) -> Option<(c_int, c_int)> {
    const O_RDONLY: c_int = 0o00;
    const O_WRONLY: c_int = 0o01;
    const O_RDWR: c_int = 0o02;
    const O_CREAT: c_int = 0o100;
    const O_TRUNC: c_int = 0o1000;
    const O_APPEND: c_int = 0o2000;
    if mode.is_null() {
        return None;
    }
    let first = unsafe { *mode } as u8;
    let mut plus = false;
    let mut p = unsafe { mode.add(1) };
    loop {
        let c = unsafe { *p } as u8;
        if c == 0 {
            break;
        }
        if c == b'+' {
            plus = true;
        }
        p = unsafe { p.add(1) };
    }
    let (open_flags, stdio_flags) = match (first, plus) {
        (b'r', false) => (O_RDONLY, F_READ),
        (b'r', true) => (O_RDWR, F_READ | F_WRITE),
        (b'w', false) => (O_WRONLY | O_CREAT | O_TRUNC, F_WRITE),
        (b'w', true) => (O_RDWR | O_CREAT | O_TRUNC, F_READ | F_WRITE),
        (b'a', false) => (O_WRONLY | O_CREAT | O_APPEND, F_WRITE),
        (b'a', true) => (O_RDWR | O_CREAT | O_APPEND, F_READ | F_WRITE),
        _ => return None,
    };
    Some((open_flags, stdio_flags))
}

// `malloc`/`free` live in the stdlib half of the libc. Binding them here
// keeps this module off the crate root, which does not re-export them.
#[cfg(target_os = "minix")]
unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

/// Wrap an fd in a heap `FILE`, or null when out of memory.
#[cfg(target_os = "minix")]
unsafe fn stream_for_fd(fd: c_int, stdio_flags: c_int, owns: c_int) -> *mut FILE {
    let f = unsafe { malloc(core::mem::size_of::<FILE>()) } as *mut FILE;
    if f.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        *f = FILE {
            fd,
            flags: stdio_flags,
            owns_fd: owns,
            pushback: [0; 8],
            pushback_len: 0,
        }
    };
    f
}

/// POSIX `fopen()`: open `path` and wrap it. The stream owns the fd, so
/// `fclose` closes it.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut FILE {
    let Some((open_flags, stdio_flags)) = (unsafe { mode_flags(mode) }) else {
        return core::ptr::null_mut();
    };
    let fd = unsafe { crate::open(path, open_flags, 0o666) };
    if fd < 0 {
        return core::ptr::null_mut();
    }
    let f = unsafe { stream_for_fd(fd, stdio_flags, 1) };
    if f.is_null() {
        crate::close(fd);
    }
    f
}

/// POSIX `fdopen()`: wrap an fd. The stream takes ownership, as C specifies.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdopen(fd: c_int, mode: *const c_char) -> *mut FILE {
    if fd < 0 {
        return core::ptr::null_mut();
    }
    let Some((_open_flags, stdio_flags)) = (unsafe { mode_flags(mode) }) else {
        return core::ptr::null_mut();
    };
    unsafe { stream_for_fd(fd, stdio_flags, 1) }
}

/// POSIX `fread()`: whole items from the stream. Unbuffered, so this is one
/// `read` per request plus whatever `ungetc` left in the pushback slot.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fread(
    ptr: *mut c_void,
    size: usize,
    nmemb: usize,
    stream: *mut FILE,
) -> usize {
    if ptr.is_null() || stream.is_null() || size == 0 || nmemb == 0 {
        return 0;
    }
    let st = unsafe { &mut *stream };
    let want = size.saturating_mul(nmemb);
    let base = ptr as *mut u8;
    let mut got = 0usize;
    while got < want && st.pushback_len > 0 {
        st.pushback_len -= 1;
        unsafe { *base.add(got) = st.pushback[st.pushback_len] };
        got += 1;
    }
    while got < want {
        let n = unsafe { crate::read(st.fd, base.add(got) as *mut c_void, want - got) };
        if n < 0 {
            st.flags |= F_ERR;
            break;
        }
        if n == 0 {
            st.flags |= F_EOF;
            break;
        }
        got += n as usize;
    }
    got / size
}

/// POSIX `fseek()`: `lseek` the stream's fd. Discards the `ungetc` pushback
/// and clears the EOF indicator, as C requires.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fseek(stream: *mut FILE, offset: i64, whence: c_int) -> c_int {
    if stream.is_null() {
        return -1;
    }
    let st = unsafe { &mut *stream };
    st.pushback_len = 0;
    st.flags &= !F_EOF;
    if crate::lseek(st.fd, offset, whence) < 0 {
        st.flags |= F_ERR;
        -1
    } else {
        0
    }
}

/// POSIX `ftell()`: the stream's current offset.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ftell(stream: *mut FILE) -> i64 {
    const SEEK_CUR: c_int = 1;
    if stream.is_null() {
        return -1;
    }
    unsafe { crate::lseek((*stream).fd, 0, SEEK_CUR) }
}

/// POSIX `rewind()`: seek to the start, then clear the error indicators.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rewind(stream: *mut FILE) {
    const SEEK_SET: c_int = 0;
    let _ = unsafe { fseek(stream, 0, SEEK_SET) };
    unsafe { clearerr(stream) };
}

/// POSIX `feof()`: has the stream hit end-of-file?
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn feof(stream: *mut FILE) -> c_int {
    if stream.is_null() {
        return 0;
    }
    ((unsafe { (*stream).flags } & F_EOF) != 0) as c_int
}

/// POSIX `ferror()`: has an operation on the stream failed?
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ferror(stream: *mut FILE) -> c_int {
    if stream.is_null() {
        return 0;
    }
    ((unsafe { (*stream).flags } & F_ERR) != 0) as c_int
}

/// POSIX `clearerr()`: drop the sticky EOF and error indicators.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clearerr(stream: *mut FILE) {
    if !stream.is_null() {
        unsafe { (*stream).flags &= !(F_EOF | F_ERR) };
    }
}

/// POSIX `perror()`: `s`, the description of `errno`, and a newline on
/// stderr. The message is assembled here because the libc has no `FILE`
/// buffering to lean on.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn perror(s: *const c_char) {
    let errno = unsafe { *crate::__errno_location() };
    let msg = unsafe { crate::c_string::strerror(errno) };
    let mut buf = [0u8; 320];
    let mut n = 0usize;
    if !s.is_null() && unsafe { *s } != 0 {
        let mut i = 0usize;
        while n < buf.len() - 2 {
            let c = unsafe { *s.add(i) };
            if c == 0 {
                break;
            }
            buf[n] = c as u8;
            n += 1;
            i += 1;
        }
        buf[n] = b':';
        buf[n + 1] = b' ';
        n += 2;
    }
    if !msg.is_null() {
        let mut i = 0usize;
        while n < buf.len() - 1 {
            let c = unsafe { *msg.add(i) };
            if c == 0 {
                break;
            }
            buf[n] = c as u8;
            n += 1;
            i += 1;
        }
    }
    buf[n] = b'\n';
    n += 1;
    let _ = unsafe { crate::write(2, buf.as_ptr() as *const c_void, n) };
}

/// POSIX `freopen()`: point `stream` at `path` instead, closing what it had.
/// The reopen-on-NULL form (same file, new mode) is not supported: the stream
/// does not remember its path.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn freopen(
    path: *const c_char,
    mode: *const c_char,
    stream: *mut FILE,
) -> *mut FILE {
    if path.is_null() || stream.is_null() {
        return core::ptr::null_mut();
    }
    let Some((open_flags, stdio_flags)) = (unsafe { mode_flags(mode) }) else {
        return core::ptr::null_mut();
    };
    let st = unsafe { &mut *stream };
    if st.owns_fd != 0 {
        crate::close(st.fd);
    }
    let fd = unsafe { crate::open(path, open_flags, 0o666) };
    if fd < 0 {
        return core::ptr::null_mut();
    }
    st.fd = fd;
    st.flags = stdio_flags;
    st.pushback_len = 0;
    st.owns_fd = 1;
    stream
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::ffi::c_char;
    #[cfg(target_os = "minix")]
    use core::ffi::c_int;
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

    /// A scanner over a NUL-terminated byte string: the same input `sscanf`
    /// reads. The field readers are pure, so the host suite covers them
    /// without a variadic entry point having to be called.
    fn str_scanner(s: &[u8]) -> Scanner<StrScan> {
        Scanner {
            src: StrScan {
                p: s.as_ptr() as *const c_char,
            },
            read: 0,
            eof: false,
        }
    }

    fn scan_uint_str(s: &[u8], width: usize, base: i32) -> (Option<u64>, usize) {
        let mut sc = str_scanner(s);
        let v = scan_uint(&mut sc, width, base);
        (v, sc.read)
    }

    fn scan_float_str(s: &[u8], width: usize) -> (Option<f64>, usize) {
        let mut sc = str_scanner(s);
        let v = scan_float(&mut sc, width);
        (v, sc.read)
    }

    #[test]
    fn scan_uint_reads_each_base() {
        assert_eq!(scan_uint_str(b"42\0", 0, 10), (Some(42), 2));
        assert_eq!(scan_uint_str(b"1f\0", 0, 16), (Some(31), 2));
        assert_eq!(scan_uint_str(b"0x1f\0", 0, 16), (Some(31), 4));
        assert_eq!(scan_uint_str(b"017\0", 0, 8), (Some(15), 3));
        assert_eq!(scan_uint_str(b"+7\0", 0, 10), (Some(7), 2));
        // A negative field wraps, as C converts it to an unsigned type.
        assert_eq!(scan_uint_str(b"-7\0", 0, 10), (Some((-7i64) as u64), 2));
    }

    /// `%i` takes its base from the prefix, and a `0` not followed by octal
    /// digits ends the field there: `08` is the field `0` with `8` unread.
    #[test]
    fn scan_uint_percent_i_detects_the_base() {
        assert_eq!(scan_uint_str(b"0x1f\0", 0, 0), (Some(31), 4));
        assert_eq!(scan_uint_str(b"017\0", 0, 0), (Some(15), 3));
        assert_eq!(scan_uint_str(b"0\0", 0, 0), (Some(0), 1));
        assert_eq!(scan_uint_str(b"08\0", 0, 0), (Some(0), 1));
        assert_eq!(scan_uint_str(b"7\0", 0, 0), (Some(7), 1));
    }

    /// Without a hex digit the `x` is not part of the field, so `0x` scans as
    /// the value `0` and leaves the `x` behind.
    #[test]
    fn scan_uint_hex_prefix_needs_a_digit() {
        let mut sc = str_scanner(b"0xg\0");
        assert_eq!(scan_uint(&mut sc, 0, 16), Some(0));
        assert_eq!(sc.read, 1);
        assert_eq!(sc.get(), Some(b'x'));
    }

    #[test]
    fn scan_uint_width_caps_the_field() {
        assert_eq!(scan_uint_str(b"12345\0", 3, 10), (Some(123), 3));
        // The sign counts against the width, as C specifies.
        assert_eq!(scan_uint_str(b"-12\0", 2, 10), (Some((-1i64) as u64), 2));
    }

    #[test]
    fn scan_uint_without_digits_rewinds() {
        assert_eq!(scan_uint_str(b"abc\0", 0, 10), (None, 0));
        assert_eq!(scan_uint_str(b"\0", 0, 10), (None, 0));
        let mut sc = str_scanner(b"-x\0");
        assert_eq!(scan_uint(&mut sc, 0, 10), None);
        assert_eq!(sc.read, 0);
        assert_eq!(sc.get(), Some(b'-'));
    }

    #[test]
    fn scan_float_reads_decimals() {
        assert_eq!(scan_float_str(b"3.5\0", 0), (Some(3.5), 3));
        assert_eq!(scan_float_str(b"-2\0", 0), (Some(-2.0), 2));
        assert_eq!(scan_float_str(b".5\0", 0), (Some(0.5), 2));
        assert_eq!(scan_float_str(b"1.\0", 0), (Some(1.0), 2));
        assert_eq!(scan_float_str(b"1e3\0", 0), (Some(1000.0), 3));
        assert_eq!(scan_float_str(b"1E-2\0", 0), (Some(0.01), 4));
        assert_eq!(scan_float_str(b"12.5e2\0", 0), (Some(1250.0), 6));
    }

    /// An exponent only belongs to the field when digits follow it, so `1e`
    /// scans as `1` and leaves the `e` for the next directive.
    #[test]
    fn scan_float_keeps_an_empty_exponent() {
        assert_eq!(scan_float_str(b"1e\0", 0), (Some(1.0), 1));
        assert_eq!(scan_float_str(b"1e+\0", 0), (Some(1.0), 1));
        let mut sc = str_scanner(b"1ex\0");
        assert_eq!(scan_float(&mut sc, 0), Some(1.0));
        assert_eq!(sc.get(), Some(b'e'));
    }

    #[test]
    fn scan_float_without_a_mantissa_rewinds() {
        assert_eq!(scan_float_str(b".\0", 0), (None, 0));
        assert_eq!(scan_float_str(b"e5\0", 0), (None, 0));
        let mut sc = str_scanner(b"-.x\0");
        assert_eq!(scan_float(&mut sc, 0), None);
        assert_eq!(sc.read, 0);
        assert_eq!(sc.get(), Some(b'-'));
        assert_eq!(sc.get(), Some(b'.'));
    }

    #[test]
    fn scan_float_stops_at_a_second_dot() {
        assert_eq!(scan_float_str(b"1.2.3\0", 0), (Some(1.2), 3));
    }

    /// The scanset's own edges: a first `]` is a member, a `-` with nothing to
    /// range over is a member, and `^` negates the set.
    #[test]
    fn scan_scanset_parses_ranges_and_negation() {
        let (set, negate, rest) = parse_scanset(c"abc]x".as_ptr());
        assert!(!negate);
        assert!(set_has(&set, b'a') && set_has(&set, b'c'));
        assert!(!set_has(&set, b']') && !set_has(&set, b'd'));
        assert_eq!(unsafe { *rest }, b'x' as c_char);

        let (set, negate, rest) = parse_scanset(c"^0-9]x".as_ptr());
        assert!(negate);
        assert!(set_has(&set, b'0') && set_has(&set, b'9'));
        assert!(!set_has(&set, b'a'));
        assert_eq!(unsafe { *rest }, b'x' as c_char);

        let (set, _, rest) = parse_scanset(c"]ab]x".as_ptr());
        assert!(set_has(&set, b']') && set_has(&set, b'a') && set_has(&set, b'b'));
        assert_eq!(unsafe { *rest }, b'x' as c_char);

        let (set, _, rest) = parse_scanset(c"a-]x".as_ptr());
        assert!(set_has(&set, b'a') && set_has(&set, b'-'));
        assert!(!set_has(&set, b'b'));
        assert_eq!(unsafe { *rest }, b'x' as c_char);
    }

    #[test]
    fn scan_length_modifiers() {
        fn parse(fmt: &[u8]) -> (Length, usize) {
            let mut p = fmt.as_ptr() as *const c_char;
            let len = unsafe { parse_length(&mut p) };
            (len, p as usize - fmt.as_ptr() as usize)
        }
        assert_eq!(parse(b"d"), (Length::None, 0));
        assert_eq!(parse(b"ld"), (Length::L, 1));
        assert_eq!(parse(b"llu"), (Length::Ll, 2));
        assert_eq!(parse(b"hhd"), (Length::Hh, 2));
        assert_eq!(parse(b"jd"), (Length::J, 1));
        assert_eq!(parse(b"zu"), (Length::Z, 1));
        assert_eq!(parse(b"td"), (Length::T, 1));
        assert_eq!(parse(b"Lf"), (Length::Big, 1));
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
