//! C stdio: unbuffered FILE streams and the printf family.
//!
//! The format engine is a port of the old `tools/c-libc.c` `vformat` with the
//! integer, string, character and pointer conversions and every length modifier,
//! plus `%f`/`%e`/`%g` over the value's exact decimal digits (see the float
//! section below). `%a`/`%A` (hex float) are the one conversion that still falls
//! through to the literal echo.

#[cfg(target_os = "minix")]
use core::ffi::VaList;
use core::ffi::{c_char, c_int};
#[cfg(target_os = "minix")]
use core::ffi::{c_long, c_longlong, c_uint, c_ulong, c_ulonglong, c_void};

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
    // Non-null only for an `open_memstream` stream: the growable buffer and the
    // two out-parameters the caller passed. `fd` is -1 for such a stream.
    mem: *mut MemStream,
}

static mut _STDIN: FILE = FILE {
    fd: 0,
    flags: F_READ,
    owns_fd: 0,
    pushback: [0; 8],
    pushback_len: 0,
    mem: core::ptr::null_mut(),
};
static mut _STDOUT: FILE = FILE {
    fd: 1,
    flags: F_WRITE,
    owns_fd: 0,
    pushback: [0; 8],
    pushback_len: 0,
    mem: core::ptr::null_mut(),
};
static mut _STDERR: FILE = FILE {
    fd: 2,
    flags: F_WRITE,
    owns_fd: 0,
    pushback: [0; 8],
    pushback_len: 0,
    mem: core::ptr::null_mut(),
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

/// One parsed conversion specification: `%[flags][width][.precision]<conv>`.
/// The five C flags for these conversions are `-`, `+`, ` `, `0` and `#`.
struct Spec {
    left: bool,
    zero: bool,
    plus: bool,
    space: bool,
    alt: bool,
    width: i32,
    prec: Option<i32>,
}

impl Spec {
    const fn new() -> Self {
        Self {
            left: false,
            zero: false,
            plus: false,
            space: false,
            alt: false,
            width: 0,
            prec: None,
        }
    }

    /// The character that precedes a signed conversion's digits, if any.
    fn sign(&self, neg: bool) -> Option<u8> {
        if neg {
            Some(b'-')
        } else if self.plus {
            Some(b'+')
        } else if self.space {
            Some(b' ')
        } else {
            None
        }
    }
}

/// Emit `v` in `base` with `digits`, returning how many characters it wrote.
///
/// The return counts the whole field, padding included -- that is what C
/// requires of `printf`, and callers such as Mesa's `tgsi_dump` advance a
/// write cursor by it, so a short count would clobber what follows.
#[allow(clippy::too_many_arguments)]
fn emit_int(
    v: u64,
    base: u32,
    digits: &[u8],
    sign: Option<u8>,
    prefix: &[u8],
    spec: &Spec,
    emit: &mut Emit<'_>,
) -> i32 {
    let mut buf = [0u8; 40];
    let mut n = 0usize;
    let mut x = v;
    loop {
        buf[n] = digits[(x % base as u64) as usize];
        n += 1;
        x /= base as u64;
        if x == 0 {
            break;
        }
    }
    // A precision sets a minimum digit count; a precision of zero prints no
    // digits at all for a zero value (C11 7.21.6.1p6).
    let mut zeros = 0i32;
    if let Some(p) = spec.prec {
        let p = p.max(0);
        if p == 0 && v == 0 {
            n = 0;
        } else if p > n as i32 {
            zeros = p - n as i32;
        }
    }
    let body = n + zeros as usize + usize::from(sign.is_some()) + prefix.len();
    let padn = (spec.width - body as i32).max(0);
    // The `0` flag is ignored when a precision is given.
    let zero_pad = spec.zero && !spec.left && spec.prec.is_none();
    let mut count = 0;
    if zero_pad {
        // The sign and any `#` prefix come before the zero padding.
        if let Some(c) = sign {
            emit(c);
            count += 1;
        }
        for b in prefix {
            emit(*b);
            count += 1;
        }
        emit_pad(b'0', padn, emit);
        count += padn;
    } else {
        if !spec.left {
            emit_pad(b' ', padn, emit);
            count += padn;
        }
        if let Some(c) = sign {
            emit(c);
            count += 1;
        }
        for b in prefix {
            emit(*b);
            count += 1;
        }
    }
    emit_pad(b'0', zeros, emit);
    count += zeros;
    for i in (0..n).rev() {
        emit(buf[i]);
        count += 1;
    }
    if spec.left {
        emit_pad(b' ', padn, emit);
        count += padn;
    }
    count
}

/// Emit a signed base-10 value under `spec`.
fn emit_signed(l: i64, spec: &Spec, emit: &mut Emit<'_>) -> i32 {
    if l < 0 {
        // Two's-complement negation without overflowing the min value.
        let v = (!(l as u64)).wrapping_add(1);
        emit_int(v, 10, b"0123456789", spec.sign(true), &[], spec, emit)
    } else {
        emit_int(
            l as u64,
            10,
            b"0123456789",
            spec.sign(false),
            &[],
            spec,
            emit,
        )
    }
}

// ---- float formatting ----
//
// `%f`/`%e`/`%g` need the value's *exact* decimal digits: the digit past the one
// printed decides the rounding, and an f64's exact value is not the short decimal
// it prints as (`0.1` is 0.1000000000000000055511151231257827..., and `2.5` at
// zero precision is a tie that rounds to even). An f64 is `m * 2^e`, so its
// expansion is finite and ends `-e` places after the point: `m << e` for `e >= 0`,
// and `m * 5^-e / 10^-e` for `e < 0`. Both numerators are integers, built here in
// a fixed-limb bignum and read out one decimal digit at a time.

/// Limbs for `m * 5^1074` (the smallest subnormal): 2547 bits, rounded up.
const BIG_LIMBS: usize = 80;

/// Room for the longest expansion an f64 has: 16 integer digits (no value below
/// `2^53` needs more) plus its 1074 fractional ones, with slack for a carry.
const FIXED_CAP: usize = 1100;

/// A little-endian non-negative integer, big enough for every intermediate above.
#[derive(Clone, Copy)]
struct Big {
    limb: [u32; BIG_LIMBS],
    n: usize,
}

impl Big {
    const fn new() -> Self {
        Self {
            limb: [0; BIG_LIMBS],
            n: 0,
        }
    }

    fn from_u64(mut v: u64) -> Self {
        let mut b = Self::new();
        while v != 0 {
            b.limb[b.n] = v as u32;
            b.n += 1;
            v >>= 32;
        }
        b
    }

    fn is_zero(&self) -> bool {
        self.n == 0
    }

    /// Multiply in place by one limb. `m` is only 2 or 5 below, and the operands
    /// are sized so the result never needs an extra limb of its own.
    fn mul_small(&mut self, m: u32) {
        let n = self.n;
        let mut carry: u64 = 0;
        for limb in &mut self.limb[..n] {
            let t = u64::from(*limb) * u64::from(m) + carry;
            *limb = t as u32;
            carry = t >> 32;
        }
        if carry != 0 {
            debug_assert!(n < BIG_LIMBS);
            self.limb[n] = carry as u32;
            self.n = n + 1;
        }
    }

    /// Divide in place by one limb, returning the remainder.
    fn divmod_small(&mut self, d: u32) -> u32 {
        let mut rem: u64 = 0;
        for i in (0..self.n).rev() {
            let cur = (rem << 32) | u64::from(self.limb[i]);
            self.limb[i] = (cur / u64::from(d)) as u32;
            rem = cur % u64::from(d);
        }
        while self.n > 0 && self.limb[self.n - 1] == 0 {
            self.n -= 1;
        }
        rem as u32
    }
}

/// The decimal digits of a finite non-negative value, most significant *integer*
/// digit first, with the point after `int_len` of them. `flen` is how many are
/// stored; anything past them is zero, because the expansion is finite.
struct Fixed {
    int_len: usize,
    flen: usize,
}

/// Write `b`'s decimal digits into `f[start..]`, most significant first, and
/// return how many there are. `b` is consumed.
fn write_digits(b: &mut Big, f: &mut [u8; FIXED_CAP], start: usize) -> usize {
    let mut n = 0;
    while !b.is_zero() {
        let d = b.divmod_small(10);
        f[start + n] = b'0' + d as u8;
        n += 1;
    }
    if n == 0 {
        f[start] = b'0';
        n = 1;
    }
    f[start..start + n].reverse();
    n
}

/// Split a finite positive value into its exact decimal digits.
fn decompose(v: f64, f: &mut [u8; FIXED_CAP]) -> Fixed {
    let bits = v.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e2) = if biased == 0 {
        // Subnormal: there is no implicit leading bit.
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), biased - 1075)
    };
    if e2 >= 0 {
        let mut b = Big::from_u64(m);
        for _ in 0..e2 {
            b.mul_small(2);
        }
        let k = write_digits(&mut b, f, 0);
        Fixed {
            int_len: k,
            flen: k,
        }
    } else {
        // `v = m / 2^k = (m * 5^k) / 10^k`: the digits of `m * 5^k` with the point
        // `k` places from the right.
        let k = (-e2) as usize;
        let mut b = Big::from_u64(m);
        for _ in 0..k {
            b.mul_small(5);
        }
        let n = write_digits(&mut b, f, 0);
        if n > k {
            Fixed {
                int_len: n - k,
                flen: n,
            }
        } else {
            // Below 1: a `0` for the integer part, then the fractional zeros that
            // come before the first significant digit.
            let shift = 1 + (k - n);
            f.copy_within(0..n, shift);
            for d in &mut f[..shift] {
                *d = b'0';
            }
            Fixed {
                int_len: 1,
                flen: shift + n,
            }
        }
    }
}

/// Whether the dropped digit `rd` (with `sticky` digits after it) rounds the
/// value up. An exact tie rounds to even, like the hardware default C follows.
fn rounds_up(rd: u8, sticky: bool, last_kept: u8) -> bool {
    match rd {
        b'0'..=b'4' => false,
        b'6'..=b'9' => true,
        b'5' => sticky || (last_kept - b'0') % 2 == 1,
        _ => false,
    }
}

/// Add one to `f[..len]`, returning the digit count -- one more when every digit
/// was a nine, which is the only way the integer part grows.
fn increment(f: &mut [u8; FIXED_CAP], len: usize) -> usize {
    let mut i = len;
    while i > 0 {
        i -= 1;
        if f[i] == b'9' {
            f[i] = b'0';
        } else {
            f[i] += 1;
            return len;
        }
    }
    f.copy_within(0..len, 1);
    f[0] = b'1';
    len + 1
}

/// Round to `prec` places after the point. Returns the integer digit count after
/// the carry, and how many fractional digits of `f` the rounding kept.
fn round_fixed(f: &mut [u8; FIXED_CAP], fx: &Fixed, prec: usize) -> (usize, usize) {
    let avail = fx.flen - fx.int_len;
    let keep = prec.min(avail);
    let (rd, sticky) = if prec < avail {
        let i = fx.int_len + prec;
        (f[i], f[i + 1..fx.flen].iter().any(|&d| d != b'0'))
    } else {
        (b'0', false)
    };
    let last = if keep > 0 {
        f[fx.int_len + keep - 1]
    } else {
        f[fx.int_len - 1]
    };
    let mut int_len = fx.int_len;
    if rounds_up(rd, sticky, last) {
        let kept = int_len + keep;
        int_len += increment(f, kept) - kept;
    }
    (int_len, keep)
}

/// Round to `want = prec + 1` significant digits, counted from `sig0`. Returns
/// how many digits of `f` that kept, and the exponent (a carry advances it).
fn round_exp(
    f: &mut [u8; FIXED_CAP],
    fx: &Fixed,
    sig0: usize,
    mut exp: i32,
    want: usize,
) -> (usize, i32) {
    let nfull = fx.flen - sig0;
    let keep = want.min(nfull);
    let (rd, sticky) = if want < nfull {
        (
            f[sig0 + want],
            f[sig0 + want + 1..fx.flen].iter().any(|&d| d != b'0'),
        )
    } else {
        (b'0', false)
    };
    if rounds_up(rd, sticky, f[sig0 + keep - 1]) {
        let mut i = sig0 + keep;
        let mut carry = true;
        while carry && i > sig0 {
            i -= 1;
            if f[i] == b'9' {
                f[i] = b'0';
            } else {
                f[i] += 1;
                carry = false;
            }
        }
        if carry {
            // Every kept digit was a nine: `1.00...` and one up on the exponent.
            f[sig0] = b'1';
            for d in &mut f[sig0 + 1..sig0 + keep] {
                *d = b'0';
            }
            exp += 1;
        }
    }
    (keep, exp)
}

/// How many decimal digits the exponent needs, at least two.
fn exp_digits(exp: i32) -> usize {
    let mut a = exp.unsigned_abs();
    let mut n = 1;
    while a >= 10 {
        a /= 10;
        n += 1;
    }
    n.max(2)
}

/// Emit `e`/`E`, the exponent's sign, and its magnitude to at least two digits.
fn emit_exp_part(exp: i32, upper: bool, emit: &mut Emit<'_>) -> i32 {
    emit(if upper { b'E' } else { b'e' });
    emit(if exp < 0 { b'-' } else { b'+' });
    let mut buf = [0u8; 4];
    let mut n = 0;
    let mut a = exp.unsigned_abs();
    loop {
        buf[n] = b'0' + (a % 10) as u8;
        n += 1;
        a /= 10;
        if a == 0 {
            break;
        }
    }
    while n < 2 {
        buf[n] = b'0';
        n += 1;
    }
    for d in buf[..n].iter().rev() {
        emit(*d);
    }
    n as i32 + 2
}

/// Emit a body bracketed by the sign and the field width. `body_len` is what the
/// closure will emit, so the padding can be decided first.
fn emit_field(
    sign: Option<u8>,
    body_len: usize,
    spec: &Spec,
    emit: &mut Emit<'_>,
    mut body: impl FnMut(&mut Emit<'_>) -> i32,
) -> i32 {
    let pad = (spec.width - body_len as i32 - i32::from(sign.is_some())).max(0);
    let mut count = 0;
    if spec.zero && !spec.left {
        if let Some(c) = sign {
            emit(c);
            count += 1;
        }
        emit_pad(b'0', pad, emit);
        count += pad;
    } else {
        if !spec.left {
            emit_pad(b' ', pad, emit);
            count += pad;
        }
        if let Some(c) = sign {
            emit(c);
            count += 1;
        }
    }
    count += body(emit);
    if spec.left {
        emit_pad(b' ', pad, emit);
        count += pad;
    }
    count
}

/// `inf`/`nan`: width still applies, but never zero-padding.
fn emit_special(sign: Option<u8>, word: &[u8], spec: &Spec, emit: &mut Emit<'_>) -> i32 {
    let pad = (spec.width - word.len() as i32 - i32::from(sign.is_some())).max(0);
    let mut count = 0;
    if !spec.left {
        emit_pad(b' ', pad, emit);
        count += pad;
    }
    if let Some(c) = sign {
        emit(c);
        count += 1;
    }
    for b in word {
        emit(*b);
        count += 1;
    }
    if spec.left {
        emit_pad(b' ', pad, emit);
        count += pad;
    }
    count
}

fn emit_fixed(
    f: &mut [u8; FIXED_CAP],
    fx: &Fixed,
    sign: Option<u8>,
    prec: usize,
    spec: &Spec,
    emit: &mut Emit<'_>,
) -> i32 {
    let (int_len, keep) = round_fixed(f, fx, prec);
    let point = prec > 0 || spec.alt;
    let body_len = int_len + usize::from(point) + prec;
    emit_field(sign, body_len, spec, emit, |e| {
        let mut n = 0;
        for &d in &f[..int_len] {
            e(d);
            n += 1;
        }
        if point {
            e(b'.');
            n += 1;
        }
        for &d in &f[int_len..int_len + keep] {
            e(d);
            n += 1;
        }
        for _ in keep..prec {
            e(b'0');
            n += 1;
        }
        n
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_exp(
    f: &mut [u8; FIXED_CAP],
    fx: &Fixed,
    sig0: usize,
    exp: i32,
    sign: Option<u8>,
    prec: usize,
    spec: &Spec,
    upper: bool,
    emit: &mut Emit<'_>,
) -> i32 {
    let want = prec + 1;
    let (keep, exp) = round_exp(f, fx, sig0, exp, want);
    let point = prec > 0 || spec.alt;
    let body_len = 1 + usize::from(point) + prec + 2 + exp_digits(exp);
    emit_field(sign, body_len, spec, emit, |e| {
        let mut n = 1;
        e(f[sig0]);
        if point {
            e(b'.');
            n += 1;
        }
        for &d in &f[sig0 + 1..sig0 + keep] {
            e(d);
            n += 1;
        }
        for _ in keep..want {
            e(b'0');
            n += 1;
        }
        n + emit_exp_part(exp, upper, e)
    })
}

/// How many leading digits of `d` are significant: trailing zeros dropped, but
/// always at least one digit kept.
fn trim(d: &[u8]) -> usize {
    let mut n = d.len();
    while n > 1 && d[n - 1] == b'0' {
        n -= 1;
    }
    n
}

/// How many fractional digits `%g` prints after dropping trailing zeros: the
/// digits held in `f` plus the zeros that pad out to `fp` places.
fn trim_frac(f: &[u8; FIXED_CAP], int_len: usize, keep: usize, fp: usize) -> usize {
    let mut zeros = fp - keep;
    let mut n = keep;
    while n > 0 && f[int_len + n - 1] == b'0' {
        zeros += 1;
        n -= 1;
    }
    fp - zeros
}

#[allow(clippy::too_many_arguments)]
fn emit_gen(
    f: &mut [u8; FIXED_CAP],
    fx: &Fixed,
    sig0: usize,
    exp: i32,
    sign: Option<u8>,
    prec: usize,
    spec: &Spec,
    upper: bool,
    emit: &mut Emit<'_>,
) -> i32 {
    // `%g`'s precision is significant digits, and a precision of zero means one.
    let p = if prec == 0 { 1 } else { prec };
    if exp < -4 || exp >= p as i32 {
        // `%e` with `p - 1` digits after the point.
        let (keep, exp) = round_exp(f, fx, sig0, exp, p);
        let digits = if spec.alt {
            p
        } else {
            trim(&f[sig0..sig0 + keep])
        };
        let point = digits > 1;
        let body_len = 1 + usize::from(point) + (digits - 1) + 2 + exp_digits(exp);
        emit_field(sign, body_len, spec, emit, |e| {
            let mut n = 1;
            e(f[sig0]);
            if point {
                e(b'.');
                n += 1;
            }
            let shown = digits.min(keep);
            for &d in &f[sig0 + 1..sig0 + shown] {
                e(d);
                n += 1;
            }
            for _ in shown..digits {
                e(b'0');
                n += 1;
            }
            n + emit_exp_part(exp, upper, e)
        })
    } else {
        // `%f` with `p - 1 - exp` digits after the point.
        let fp = (p as i32 - 1 - exp) as usize;
        let (int_len, keep) = round_fixed(f, fx, fp);
        let fraclen = if spec.alt {
            fp
        } else {
            trim_frac(f, int_len, keep, fp)
        };
        let point = fraclen > 0;
        let body_len = int_len + usize::from(point) + fraclen;
        emit_field(sign, body_len, spec, emit, |e| {
            let mut n = 0;
            for &d in &f[..int_len] {
                e(d);
                n += 1;
            }
            if point {
                e(b'.');
                n += 1;
            }
            let shown = fraclen.min(keep);
            for &d in &f[int_len..int_len + shown] {
                e(d);
                n += 1;
            }
            for _ in shown..fraclen {
                e(b'0');
                n += 1;
            }
            n
        })
    }
}

fn emit_zero(
    kind: FloatKind,
    upper: bool,
    sign: Option<u8>,
    prec: usize,
    spec: &Spec,
    emit: &mut Emit<'_>,
) -> i32 {
    match kind {
        FloatKind::Fixed => {
            let point = prec > 0 || spec.alt;
            let body_len = 1 + usize::from(point) + prec;
            emit_field(sign, body_len, spec, emit, |e| {
                let mut n = 1;
                e(b'0');
                if point {
                    e(b'.');
                    n += 1;
                }
                for _ in 0..prec {
                    e(b'0');
                    n += 1;
                }
                n
            })
        }
        FloatKind::Exp => {
            let point = prec > 0 || spec.alt;
            let body_len = 1 + usize::from(point) + prec + 4;
            emit_field(sign, body_len, spec, emit, |e| {
                let mut n = 1;
                e(b'0');
                if point {
                    e(b'.');
                    n += 1;
                }
                for _ in 0..prec {
                    e(b'0');
                    n += 1;
                }
                n + emit_exp_part(0, upper, e)
            })
        }
        FloatKind::Gen => {
            let p = if prec == 0 { 1 } else { prec };
            let keep = if spec.alt { p - 1 } else { 0 };
            let point = keep > 0;
            let body_len = 1 + usize::from(point) + keep;
            emit_field(sign, body_len, spec, emit, |e| {
                let mut n = 1;
                e(b'0');
                if point {
                    e(b'.');
                    n += 1;
                }
                for _ in 0..keep {
                    e(b'0');
                    n += 1;
                }
                n
            })
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatKind {
    /// `%f`
    Fixed,
    /// `%e`
    Exp,
    /// `%g`
    Gen,
}

/// Format `v` as `%f`, `%e` or `%g` under `spec`; `upper` selects `F`/`E`/`G`.
fn emit_float(v: f64, kind: FloatKind, upper: bool, spec: &Spec, emit: &mut Emit<'_>) -> i32 {
    let neg = v.is_sign_negative();
    let sign = spec.sign(neg);
    if v.is_nan() {
        return emit_special(sign, if upper { b"NAN" } else { b"nan" }, spec, emit);
    }
    if v.is_infinite() {
        return emit_special(sign, if upper { b"INF" } else { b"inf" }, spec, emit);
    }
    let prec = spec.prec.unwrap_or(6).max(0) as usize;
    let mag = if neg { -v } else { v };
    if mag == 0.0 {
        return emit_zero(kind, upper, sign, prec, spec, emit);
    }
    let mut f = [0u8; FIXED_CAP];
    let fx = decompose(mag, &mut f);
    let sig0 = f[..fx.flen].iter().position(|&d| d != b'0').unwrap_or(0);
    let exp = fx.int_len as i32 - 1 - sig0 as i32;
    match kind {
        FloatKind::Fixed => emit_fixed(&mut f, &fx, sign, prec, spec, emit),
        FloatKind::Exp => emit_exp(&mut f, &fx, sig0, exp, sign, prec, spec, upper, emit),
        FloatKind::Gen => emit_gen(&mut f, &fx, sig0, exp, sign, prec, spec, upper, emit),
    }
}

/// The largest width or precision honored, so a format like `%999999999999d`
/// saturates instead of overflowing the counter.
const MAX_FIELD: i32 = 1 << 20;

/// Read a run of decimal digits into a saturated `i32`, advancing `*p`.
unsafe fn parse_number(p: &mut *const c_char) -> i32 {
    let mut v: i32 = 0;
    while (unsafe { **p } as u8).is_ascii_digit() {
        v = v
            .saturating_mul(10)
            .saturating_add(i32::from(unsafe { **p } as u8 - b'0'));
        *p = unsafe { (*p).add(1) };
    }
    v.min(MAX_FIELD)
}

#[cfg(target_os = "minix")]
/// Read a vararg integer for the conversion's length modifier, applying the
/// narrowing `hh`/`h` ask for.
unsafe fn read_signed(len: Length, args: &mut VaList<'_>) -> i64 {
    match len {
        Length::Hh => i64::from(unsafe { args.next_arg::<c_int>() } as i8),
        Length::H => i64::from(unsafe { args.next_arg::<c_int>() } as i16),
        Length::L => i64::from(unsafe { args.next_arg::<c_long>() }),
        Length::Ll | Length::J => unsafe { args.next_arg::<c_longlong>() },
        Length::Z | Length::T => unsafe { args.next_arg::<isize>() as i64 },
        Length::None | Length::Big => i64::from(unsafe { args.next_arg::<c_int>() }),
    }
}

#[cfg(target_os = "minix")]
/// The unsigned counterpart of `read_signed`.
unsafe fn read_unsigned(len: Length, args: &mut VaList<'_>) -> u64 {
    match len {
        Length::Hh => u64::from(unsafe { args.next_arg::<c_uint>() } as u8),
        Length::H => u64::from(unsafe { args.next_arg::<c_uint>() } as u16),
        Length::L => u64::from(unsafe { args.next_arg::<c_ulong>() }),
        Length::Ll | Length::J => unsafe { args.next_arg::<c_ulonglong>() },
        Length::Z | Length::T => unsafe { args.next_arg::<usize>() as u64 },
        Length::None | Length::Big => u64::from(unsafe { args.next_arg::<c_uint>() }),
    }
}

#[cfg(target_os = "minix")]
/// `%n`: store the characters written so far through the pointer the caller gave.
unsafe fn write_count(len: Length, args: &mut VaList<'_>, count: i32) {
    match len {
        Length::Hh => unsafe { *args.next_arg::<*mut i8>() = count as i8 },
        Length::H => unsafe { *args.next_arg::<*mut i16>() = count as i16 },
        Length::L => unsafe { *args.next_arg::<*mut c_long>() = c_long::from(count) },
        Length::Ll | Length::J => unsafe {
            *args.next_arg::<*mut c_longlong>() = c_longlong::from(count)
        },
        Length::Z | Length::T => unsafe { *args.next_arg::<*mut isize>() = count as isize },
        Length::None | Length::Big => unsafe { *args.next_arg::<*mut c_int>() = count },
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
        let mut spec = Spec::new();
        loop {
            match unsafe { *p } as u8 {
                b'-' => spec.left = true,
                b'+' => spec.plus = true,
                b' ' => spec.space = true,
                b'0' => spec.zero = true,
                b'#' => spec.alt = true,
                _ => break,
            }
            p = unsafe { p.add(1) };
        }
        spec.width = unsafe { parse_number(&mut p) };
        if (unsafe { *p } as u8) == b'.' {
            p = unsafe { p.add(1) };
            spec.prec = Some(unsafe { parse_number(&mut p) });
        }
        let len = unsafe { parse_length(&mut p) };
        let conv = unsafe { *p } as u8;
        match conv {
            b'%' => {
                emit(b'%');
                count += 1;
            }
            b'c' => {
                let c = unsafe { args.next_arg::<c_int>() } as u8;
                let padn = (spec.width - 1).max(0);
                if !spec.left {
                    emit_pad(b' ', padn, &mut emit);
                }
                emit(c);
                if spec.left {
                    emit_pad(b' ', padn, &mut emit);
                }
                count += 1 + padn;
            }
            b's' => {
                let mut s = unsafe { args.next_arg::<*const c_char>() };
                if s.is_null() {
                    s = b"(null)".as_ptr() as *const c_char;
                }
                let mut len = c_strlen(s) as i32;
                if let Some(prec) = spec.prec {
                    len = len.min(prec.max(0));
                }
                let padn = (spec.width - len).max(0);
                if !spec.left {
                    emit_pad(b' ', padn, &mut emit);
                }
                for i in 0..len {
                    emit(unsafe { *s.add(i as usize) } as u8);
                }
                if spec.left {
                    emit_pad(b' ', padn, &mut emit);
                }
                count += len + padn;
            }
            b'p' => {
                let v = unsafe { args.next_arg::<*const c_void>() } as usize as u64;
                count += emit_int(v, 16, b"0123456789abcdef", None, &[], &spec, &mut emit);
            }
            b'd' | b'i' => {
                count += emit_signed(unsafe { read_signed(len, args) }, &spec, &mut emit);
            }
            b'u' => {
                let v = unsafe { read_unsigned(len, args) };
                count += emit_int(v, 10, b"0123456789", None, &[], &spec, &mut emit);
            }
            b'o' => {
                let v = unsafe { read_unsigned(len, args) };
                let prefix: &[u8] = if spec.alt && v != 0 { b"0" } else { &[] };
                count += emit_int(v, 8, b"01234567", None, prefix, &spec, &mut emit);
            }
            b'x' | b'X' => {
                let v = unsafe { read_unsigned(len, args) };
                let (digits, prefix): (&[u8], &[u8]) = if conv == b'x' {
                    (b"0123456789abcdef", b"0x")
                } else {
                    (b"0123456789ABCDEF", b"0X")
                };
                let prefix: &[u8] = if spec.alt && v != 0 { prefix } else { &[] };
                count += emit_int(v, 16, digits, None, prefix, &spec, &mut emit);
            }
            b'f' | b'F' => {
                let v = unsafe { args.next_arg::<f64>() };
                count += emit_float(v, FloatKind::Fixed, conv == b'F', &spec, &mut emit);
            }
            b'e' | b'E' => {
                let v = unsafe { args.next_arg::<f64>() };
                count += emit_float(v, FloatKind::Exp, conv == b'E', &spec, &mut emit);
            }
            b'g' | b'G' => {
                let v = unsafe { args.next_arg::<f64>() };
                count += emit_float(v, FloatKind::Gen, conv == b'G', &spec, &mut emit);
            }
            b'n' => unsafe { write_count(len, args, count) },
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
    // back to stdout rather than dereferencing it. A memory stream has no fd at
    // all, and its bytes go to the buffer.
    let fd = if stream.is_null() {
        1
    } else {
        unsafe { (*stream).fd }
    };
    let mem = if stream.is_null() {
        core::ptr::null_mut()
    } else {
        unsafe { (*stream).mem }
    };
    let mut emit = |c: u8| {
        let b = c;
        if mem.is_null() {
            let _ = unsafe { crate::write(fd, &b as *const u8 as *const c_void, 1) };
        } else {
            let _ = unsafe { mem_write(mem, &b as *const u8, 1) };
        }
    };
    unsafe { vformat(fmt, &mut ap, &mut emit) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fprintf(stream: *mut FILE, fmt: *const c_char, args: ...) -> c_int {
    unsafe { vfprintf(stream, fmt, args) }
}

/// POSIX `setvbuf()`. Streams here write through to their descriptor with no
/// user buffer, so the request is accepted and `_IONBF` is already the case.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setvbuf(
    _stream: *mut FILE,
    _buf: *mut c_char,
    _mode: c_int,
    _size: usize,
) -> c_int {
    0
}

/// POSIX `openlog()`: the identity and facility for later `syslog` calls. There
/// is no syslog service, so the settings are dropped; `syslog` writes to stderr.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openlog(_ident: *const c_char, _option: c_int, _facility: c_int) {}

/// POSIX `syslog()`: Mesa's `log.c` routes to it under `DETECT_OS_POSIX`. There
/// is no syslog service, so the message goes to stderr (the priorities and the
/// call site are what vary; the destination is not).
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syslog(_priority: c_int, fmt: *const c_char, args: ...) {
    unsafe { vfprintf(stderr, fmt, args) };
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
    let st = unsafe { &*stream };
    if !st.mem.is_null() {
        return if unsafe { mem_write(st.mem, ptr as *const u8, total) } {
            nmemb
        } else {
            0
        };
    }
    let fd = st.fd;
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
    if !st.mem.is_null() {
        if unsafe { mem_write(st.mem, &b as *const u8, 1) } {
            return b as c_int;
        }
        st.flags |= F_ERR;
        return -1;
    }
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
    if !stream.is_null() {
        let st = unsafe { &*stream };
        if !st.mem.is_null() {
            unsafe { mem_finalize(st.mem) };
        }
    }
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
    if !st.mem.is_null() {
        unsafe {
            mem_finalize(st.mem);
            free(st.mem as *mut c_void);
            free(stream as *mut c_void);
        }
        return 0;
    }
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
    fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
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
            mem: core::ptr::null_mut(),
        }
    };
    f
}

/// State for an `open_memstream` stream: a growable buffer and the two
/// out-parameters POSIX has the caller pass for the final buffer and length.
/// Compiled on every target because `FILE` holds one; only the minix target's
/// `open_memstream` constructs it.
#[allow(dead_code)]
struct MemStream {
    buf: *mut u8,
    len: usize,
    cap: usize,
    ptr: *mut *mut c_char,
    sizeloc: *mut usize,
}

/// Append `n` bytes to a memory stream, growing the buffer (and keeping it NUL
/// terminated) as needed. False on allocation failure, which the caller turns
/// into the stream's error flag.
#[cfg(target_os = "minix")]
unsafe fn mem_write(ms: *mut MemStream, bytes: *const u8, n: usize) -> bool {
    let ms = unsafe { &mut *ms };
    if ms.len + n + 1 > ms.cap {
        let mut cap = ms.cap.max(64);
        while ms.len + n + 1 > cap {
            cap = cap.saturating_mul(2);
        }
        let grown = unsafe { realloc(ms.buf as *mut c_void, cap) } as *mut u8;
        if grown.is_null() {
            return false;
        }
        ms.buf = grown;
        ms.cap = cap;
    }
    unsafe { core::ptr::copy_nonoverlapping(bytes, ms.buf.add(ms.len), n) };
    ms.len += n;
    unsafe { *ms.buf.add(ms.len) = 0 };
    true
}

/// Publish a memory stream's buffer and length to the caller's out-parameters,
/// which POSIX has `fflush`/`fclose` do.
#[cfg(target_os = "minix")]
unsafe fn mem_finalize(ms: *mut MemStream) {
    let ms = unsafe { &mut *ms };
    unsafe {
        *ms.buf.add(ms.len) = 0;
        *ms.ptr = ms.buf as *mut c_char;
        *ms.sizeloc = ms.len;
    }
}

/// POSIX `open_memstream()`: a write-only `FILE` that accumulates into a heap
/// buffer. POSIX leaves the buffer to the caller to `free` after `fclose`, so it
/// is deliberately not freed here.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open_memstream(ptr: *mut *mut c_char, sizeloc: *mut usize) -> *mut FILE {
    if ptr.is_null() || sizeloc.is_null() {
        return core::ptr::null_mut();
    }
    let ms = unsafe { malloc(core::mem::size_of::<MemStream>()) } as *mut MemStream;
    if ms.is_null() {
        return core::ptr::null_mut();
    }
    let buf = unsafe { malloc(64) } as *mut u8;
    if buf.is_null() {
        unsafe { free(ms as *mut c_void) };
        return core::ptr::null_mut();
    }
    let f = unsafe { malloc(core::mem::size_of::<FILE>()) } as *mut FILE;
    if f.is_null() {
        unsafe {
            free(buf as *mut c_void);
            free(ms as *mut c_void);
        }
        return core::ptr::null_mut();
    }
    unsafe {
        *buf = 0;
        *ms = MemStream {
            buf,
            len: 0,
            cap: 64,
            ptr,
            sizeloc,
        };
        *ptr = buf as *mut c_char;
        *sizeloc = 0;
        *f = FILE {
            fd: -1,
            flags: F_WRITE,
            owns_fd: 0,
            pushback: [0; 8],
            pushback_len: 0,
            mem: ms,
        };
    }
    f
}

/// C99 `asprintf()`: format into a freshly allocated string the caller frees. A
/// variadic front end for `vasprintf`, exactly as `printf` is for `vprintf`.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asprintf(strp: *mut *mut c_char, fmt: *const c_char, args: ...) -> c_int {
    unsafe { crate::c_locale::vasprintf(strp, fmt, args) }
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
        assert_eq!(r, 15); // full length reported
        assert_eq!(&buf[..15], b"n=42 s=hi h=1f%");
        assert_eq!(buf[15], 0);
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

    fn spec(flags: &str, width: i32, prec: Option<i32>) -> Spec {
        let mut s = Spec::new();
        for f in flags.bytes() {
            match f {
                b'-' => s.left = true,
                b'+' => s.plus = true,
                b' ' => s.space = true,
                b'0' => s.zero = true,
                b'#' => s.alt = true,
                _ => panic!("bad flag"),
            }
        }
        s.width = width;
        s.prec = prec;
        s
    }

    fn fmt_int(v: u64, base: u32, digits: &[u8], s: &Spec) -> (Vec<u8>, i32) {
        let mut out = Vec::new();
        let mut emit: Emit<'_> = &mut |c: u8| out.push(c);
        let n = emit_int(v, base, digits, None, &[], s, &mut emit);
        (out, n)
    }

    fn fmt_signed(l: i64, s: &Spec) -> (Vec<u8>, i32) {
        let mut out = Vec::new();
        let mut emit: Emit<'_> = &mut |c: u8| out.push(c);
        let n = emit_signed(l, s, &mut emit);
        (out, n)
    }

    #[test]
    fn emit_int_counts_padding() {
        // The zero padding is part of the field, so it is part of the count
        // too -- a caller advancing a cursor by the count must land on the
        // terminator.
        let (out, n) = fmt_int(0x2A, 16, b"0123456789abcdef", &spec("0", 8, None));
        assert_eq!(out, b"0000002a");
        assert_eq!(n, 8);
    }

    #[test]
    fn emit_int_space_flag() {
        // Mesa's `tgsi_dump` labels each instruction with `"% 3u"`; the space
        // flag has to be recognized or the literal `% 3u` reaches the wire.
        let (out, n) = fmt_int(0, 10, b"0123456789", &spec(" ", 3, None));
        assert_eq!(out, b"  0");
        assert_eq!(n, 3);
    }

    #[test]
    fn emit_int_precision_sets_min_digits() {
        let (out, n) = fmt_int(0x2A, 16, b"0123456789abcdef", &spec("", 0, Some(4)));
        assert_eq!(out, b"002a");
        assert_eq!(n, 4);
        // A precision of zero prints nothing for a zero value.
        let (out, n) = fmt_int(0, 10, b"0123456789", &spec("", 0, Some(0)));
        assert_eq!(out, b"");
        assert_eq!(n, 0);
    }

    #[test]
    fn emit_int_left_justifies() {
        let (out, n) = fmt_int(7, 10, b"0123456789", &spec("-", 5, None));
        assert_eq!(out, b"7    ");
        assert_eq!(n, 5);
    }

    #[test]
    fn emit_int_alt_prefixes_hex() {
        let (out, n) = fmt_int(0x2A, 16, b"0123456789abcdef", &spec("#", 0, None));
        // `#` is applied by the caller, which is what supplies the prefix.
        assert_eq!(out, b"2a");
        assert_eq!(n, 2);
    }

    #[test]
    fn emit_signed_sign_flags() {
        assert_eq!(fmt_signed(7, &spec("+", 0, None)), (b"+7".to_vec(), 2));
        assert_eq!(fmt_signed(7, &spec(" ", 0, None)), (b" 7".to_vec(), 2));
        assert_eq!(fmt_signed(-7, &spec("+", 0, None)), (b"-7".to_vec(), 2));
        assert_eq!(fmt_signed(-7, &spec("0", 5, None)), (b"-0007".to_vec(), 5));
    }

    fn fmt_float(v: f64, kind: FloatKind, spec: &Spec) -> (Vec<u8>, i32) {
        let mut out = Vec::new();
        let mut emit: Emit<'_> = &mut |c: u8| out.push(c);
        let n = emit_float(v, kind, false, spec, &mut emit);
        (out, n)
    }

    fn fmt_upper(v: f64, kind: FloatKind, spec: &Spec) -> Vec<u8> {
        let mut out = Vec::new();
        let mut emit: Emit<'_> = &mut |c: u8| out.push(c);
        emit_float(v, kind, true, spec, &mut emit);
        out
    }

    fn fmt_f(v: f64, prec: Option<i32>) -> Vec<u8> {
        fmt_float(v, FloatKind::Fixed, &spec("", 0, prec)).0
    }

    fn fmt_e(v: f64, prec: Option<i32>) -> Vec<u8> {
        fmt_float(v, FloatKind::Exp, &spec("", 0, prec)).0
    }

    fn fmt_g(v: f64, prec: Option<i32>, alt: bool) -> Vec<u8> {
        fmt_float(
            v,
            FloatKind::Gen,
            &spec(if alt { "#" } else { "" }, 0, prec),
        )
        .0
    }

    /// Every expectation here is what C's `printf` prints, and each covers a
    /// corner the exact-digit machinery exists for: an exact tie, `0.1`'s real
    /// expansion, a carry that grows the integer part, a subnormal, `%e`'s
    /// exponent form, and `%g`'s choice between the two.
    #[test]
    fn emit_fixed_matches_printf() {
        assert_eq!(fmt_f(0.0, None), b"0.000000");
        assert_eq!(fmt_f(-0.0, None), b"-0.000000");
        assert_eq!(fmt_f(1.23456, Some(2)), b"1.23");
        assert_eq!(fmt_f(100.0, Some(2)), b"100.00");
        assert_eq!(fmt_f(123456.789, Some(3)), b"123456.789");
        assert_eq!(fmt_f(1e-5, Some(7)), b"0.0000100");
        // An exact tie rounds to even, and only the exact value decides.
        assert_eq!(fmt_f(2.5, Some(0)), b"2");
        assert_eq!(fmt_f(3.5, Some(0)), b"4");
        assert_eq!(fmt_f(0.5, Some(0)), b"0");
        assert_eq!(fmt_f(1.005, Some(2)), b"1.00");
        assert_eq!(fmt_f(0.1, Some(20)), b"0.10000000000000000555");
        // A carry that grows the integer part.
        assert_eq!(fmt_f(9.9999999, Some(6)), b"10.000000");
        assert_eq!(fmt_f(5e-324, Some(0)), b"0");
    }

    #[test]
    fn emit_fixed_of_a_huge_value_is_exact() {
        let want: &[u8] = b"1000000000000000052504760255204420248704468581108159154915854115511802457988908195786371375080447864043704443832883878176942523235360430575644792184786706982848387200926575803737830233794788090059368953234970799945081119038967640880074652742780142494579258788820056842838115669472196386865459400540160";
        assert_eq!(fmt_f(1e300, Some(0)), want);
    }

    #[test]
    fn emit_exp_matches_printf() {
        assert_eq!(fmt_e(0.0, None), b"0.000000e+00");
        assert_eq!(fmt_e(1.0, None), b"1.000000e+00");
        assert_eq!(fmt_e(1234.5, Some(2)), b"1.23e+03");
        assert_eq!(fmt_e(0.00012345, Some(4)), b"1.2345e-04");
        assert_eq!(fmt_e(9.9999999, Some(3)), b"1.000e+01");
        assert_eq!(fmt_e(1.0, Some(0)), b"1e+00");
        let (out, n) = fmt_float(1.0, FloatKind::Exp, &spec("", 0, Some(2)));
        assert_eq!(n as usize, out.len());
        assert_eq!(
            fmt_upper(1.0, FloatKind::Exp, &spec("", 0, Some(2))),
            b"1.00E+00"
        );
        assert_eq!(
            fmt_upper(f64::INFINITY, FloatKind::Fixed, &spec("", 0, None)),
            b"INF"
        );
    }

    #[test]
    fn emit_gen_matches_printf() {
        assert_eq!(fmt_g(0.0, None, false), b"0");
        assert_eq!(fmt_g(100.0, None, false), b"100");
        assert_eq!(fmt_g(0.0001, None, false), b"0.0001");
        assert_eq!(fmt_g(0.0001234, None, false), b"0.0001234");
        // Outside `%f`'s window: `%e` with the zeros trimmed.
        assert_eq!(fmt_g(1234567.0, None, false), b"1.23457e+06");
        assert_eq!(fmt_g(1000000.0, None, false), b"1e+06");
        assert_eq!(fmt_g(0.00001, None, false), b"1e-05");
        assert_eq!(fmt_g(1.5, Some(1), false), b"2");
        assert_eq!(fmt_g(1.0, Some(0), false), b"1");
        assert_eq!(fmt_g(100.0, Some(4), false), b"100");
        // `#` keeps the trailing zeros.
        assert_eq!(fmt_g(1.0, None, true), b"1.00000");
    }

    #[test]
    fn emit_float_specials_and_field() {
        let s = spec("", 0, None);
        assert_eq!(fmt_float(f64::INFINITY, FloatKind::Fixed, &s).0, b"inf");
        assert_eq!(
            fmt_float(f64::NEG_INFINITY, FloatKind::Fixed, &s).0,
            b"-inf"
        );
        assert_eq!(fmt_float(f64::NAN, FloatKind::Fixed, &s).0, b"nan");
        assert_eq!(
            fmt_float(3.0, FloatKind::Fixed, &spec("#", 0, Some(0))).0,
            b"3."
        );
        assert_eq!(
            fmt_float(-1.23456, FloatKind::Fixed, &spec("", 0, Some(2))).0,
            b"-1.23"
        );
        assert_eq!(
            fmt_float(1.23456, FloatKind::Fixed, &spec("+", 0, Some(2))).0,
            b"+1.23"
        );
        assert_eq!(
            fmt_float(1.23456, FloatKind::Fixed, &spec("0", 8, Some(2))).0,
            b"00001.23"
        );
        assert_eq!(
            fmt_float(-1.23456, FloatKind::Fixed, &spec("0", 8, Some(2))).0,
            b"-0001.23"
        );
        assert_eq!(
            fmt_float(1.23456, FloatKind::Fixed, &spec("-", 8, Some(2))).0,
            b"1.23    "
        );
        // The count covers the whole field, like the integer conversions.
        let (out, n) = fmt_float(1.23456, FloatKind::Fixed, &spec("0", 8, Some(2)));
        assert_eq!(n as usize, out.len());
    }
}
