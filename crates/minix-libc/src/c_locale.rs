//! C locale (`locale.h`/`wctype.h` multibyte surface) — C locale only.
//!
//! libc++ with `_LIBCPP_HAS_LOCALIZATION` pulls in `<locale>`'s
//! `locale_base_api.h`, whose BSD-fallback path calls plain C functions
//! (`setlocale`, `localeconv`, `mbrtowc`, `strtof_l`, ...). Everything
//! here is a faithful C-locale implementation: one byte == one wide
//! char, collation == comparison, no currency formatting.

#[cfg(target_os = "minix")]
use core::ffi::{VaList, c_int};
use core::ffi::{c_char, c_uint, c_void};

/// C `locale_t` — an opaque handle; any non-null value is the "C" locale.
pub type LocaleT = *mut c_void;

/// C `struct lconv` — layout matches `tools/c-include/locale.h`.
#[repr(C)]
pub struct Lconv {
    decimal_point: *mut c_char,
    thousands_sep: *mut c_char,
    grouping: *mut c_char,
    int_curr_symbol: *mut c_char,
    currency_symbol: *mut c_char,
    mon_decimal_point: *mut c_char,
    mon_thousands_sep: *mut c_char,
    mon_grouping: *mut c_char,
    positive_sign: *mut c_char,
    negative_sign: *mut c_char,
    int_frac_digits: c_char,
    frac_digits: c_char,
    p_cs_precedes: c_char,
    p_sep_by_space: c_char,
    n_cs_precedes: c_char,
    n_sep_by_space: c_char,
    p_sign_posn: c_char,
    n_sign_posn: c_char,
    int_p_cs_precedes: c_char,
    int_p_sep_by_space: c_char,
    int_n_cs_precedes: c_char,
    int_n_sep_by_space: c_char,
    int_p_sign_posn: c_char,
    int_n_sign_posn: c_char,
}

const CHAR_MAX: c_char = 127;

static DECIMAL_POINT: [c_char; 2] = [b'.' as c_char, 0];
static EMPTY_STR: [c_char; 1] = [0];

static mut LCONV: Lconv = Lconv {
    decimal_point: core::ptr::addr_of!(DECIMAL_POINT) as *mut c_char,
    thousands_sep: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    grouping: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    int_curr_symbol: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    currency_symbol: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    mon_decimal_point: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    mon_thousands_sep: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    mon_grouping: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    positive_sign: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    negative_sign: core::ptr::addr_of!(EMPTY_STR) as *mut c_char,
    int_frac_digits: CHAR_MAX,
    frac_digits: CHAR_MAX,
    p_cs_precedes: CHAR_MAX,
    p_sep_by_space: CHAR_MAX,
    n_cs_precedes: CHAR_MAX,
    n_sep_by_space: CHAR_MAX,
    p_sign_posn: CHAR_MAX,
    n_sign_posn: CHAR_MAX,
    int_p_cs_precedes: CHAR_MAX,
    int_p_sep_by_space: CHAR_MAX,
    int_n_cs_precedes: CHAR_MAX,
    int_n_sep_by_space: CHAR_MAX,
    int_p_sign_posn: CHAR_MAX,
    int_n_sign_posn: CHAR_MAX,
};

/// The one locale handle (the "C" locale).
static mut C_LOCALE: u8 = 0;

fn c_locale() -> LocaleT {
    core::ptr::addr_of_mut!(C_LOCALE) as *mut c_void
}

fn is_c_locale(name: *const c_char) -> bool {
    if name.is_null() {
        return true;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes();
    s == b"C" || s == b"POSIX" || s == b""
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setlocale(_category: c_int, locale: *const c_char) -> *mut c_char {
    if is_c_locale(locale) {
        b"C\0".as_ptr() as *const c_char as *mut c_char
    } else {
        core::ptr::null_mut()
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn localeconv() -> *mut Lconv {
    core::ptr::addr_of_mut!(LCONV)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn newlocale(
    _category_mask: c_int,
    locale: *const c_char,
    _base: LocaleT,
) -> LocaleT {
    if is_c_locale(locale) {
        c_locale()
    } else {
        core::ptr::null_mut() // unsupported locale
    }
}

static mut CURRENT_LOCALE: *mut c_void = core::ptr::null_mut();

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uselocale(newloc: LocaleT) -> LocaleT {
    let old = unsafe { CURRENT_LOCALE };
    // LC_GLOBAL_LOCALE is (locale_t)-1; 0 queries without changing.
    if !newloc.is_null() && newloc != -1isize as LocaleT {
        unsafe { CURRENT_LOCALE = newloc };
    }
    if old.is_null() { c_locale() } else { old }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn freelocale(_loc: LocaleT) {}

// ---- strto*_l / strcoll_l / strxfrm_l ----

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtod_l(
    nptr: *const c_char,
    endptr: *mut *mut c_char,
    _loc: LocaleT,
) -> f64 {
    crate::c_stdlib::strtod_impl(nptr as *const u8, endptr as *mut *mut u8)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtof_l(
    nptr: *const c_char,
    endptr: *mut *mut c_char,
    _loc: LocaleT,
) -> f32 {
    crate::c_stdlib::strtod_impl(nptr as *const u8, endptr as *mut *mut u8) as f32
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcoll_l(s1: *const c_char, s2: *const c_char, _loc: LocaleT) -> c_int {
    unsafe { crate::c_string::strcmp(s1, s2) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strxfrm_l(
    dest: *mut c_char,
    src: *const c_char,
    n: usize,
    _loc: LocaleT,
) -> usize {
    // C locale: the transformed string is the source.
    let len = unsafe { crate::c_string::strnlen(src, usize::MAX) };
    if !dest.is_null() && n > 0 {
        let copy = len.min(n - 1);
        for i in 0..copy {
            unsafe { *dest.add(i) = *src.add(i) };
        }
        unsafe { *dest.add(copy) = 0 };
    }
    len
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn toupper_l(c: c_int, _loc: LocaleT) -> c_int {
    unsafe { crate::c_string::toupper(c) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tolower_l(c: c_int, _loc: LocaleT) -> c_int {
    unsafe { crate::c_string::tolower(c) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strftime_l(
    s: *mut c_char,
    max: usize,
    format: *const c_char,
    tm: *const crate::c_time::Tm,
    _loc: LocaleT,
) -> usize {
    unsafe { crate::c_time::strftime(s, max, format, tm) }
}

// ---- wide *_l variants ----

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcscoll_l(
    s1: *const crate::c_wchar::WChar,
    s2: *const crate::c_wchar::WChar,
    _loc: LocaleT,
) -> c_int {
    unsafe { crate::c_wchar::wcscmp(s1, s2) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsxfrm_l(
    dest: *mut crate::c_wchar::WChar,
    src: *const crate::c_wchar::WChar,
    n: usize,
    _loc: LocaleT,
) -> usize {
    let len = unsafe { crate::c_wchar::wcslen(src) };
    let copy = len.min(n.saturating_sub(1));
    for i in 0..copy {
        unsafe { *dest.add(i) = *src.add(i) };
    }
    unsafe { *dest.add(copy) = 0 };
    len
}

fn isw_ctype(wc: c_uint, desc: c_uint) -> bool {
    if desc & WCTYPE_ALNUM != 0 && crate::c_wchar::isw_alpha(wc) {
        return true;
    }
    if desc & WCTYPE_ALPHA != 0 && crate::c_wchar::isw_alpha(wc) {
        return true;
    }
    if desc & WCTYPE_BLANK != 0 && (wc == b' ' as c_uint || wc == b'\t' as c_uint) {
        return true;
    }
    if desc & WCTYPE_CNTRL != 0 && (wc < b' ' as c_uint || wc == 127) {
        return true;
    }
    if desc & WCTYPE_DIGIT != 0 && (b'0' as c_uint..=b'9' as c_uint).contains(&wc) {
        return true;
    }
    if desc & WCTYPE_GRAPH != 0 && (b'!' as c_uint..=b'~' as c_uint).contains(&wc) {
        return true;
    }
    if desc & WCTYPE_LOWER != 0 && crate::c_wchar::isw_lower(wc) {
        return true;
    }
    if desc & WCTYPE_PRINT != 0 && (b' ' as c_uint..=b'~' as c_uint).contains(&wc) {
        return true;
    }
    if desc & WCTYPE_PUNCT != 0
        && ((b'!' as c_uint..=b'/' as c_uint).contains(&wc)
            || (b':' as c_uint..=b'@' as c_uint).contains(&wc)
            || (b'[' as c_uint..=b'`' as c_uint).contains(&wc)
            || (b'{' as c_uint..=b'~' as c_uint).contains(&wc))
    {
        return true;
    }
    if desc & WCTYPE_SPACE != 0
        && (wc == b' ' as c_uint
            || wc == b'\t' as c_uint
            || wc == b'\n' as c_uint
            || wc == b'\r' as c_uint
            || wc == b'\x0c' as c_uint
            || wc == b'\x0b' as c_uint)
    {
        return true;
    }
    if desc & WCTYPE_UPPER != 0 && crate::c_wchar::isw_upper(wc) {
        return true;
    }
    if desc & WCTYPE_XDIGIT != 0
        && ((b'0' as c_uint..=b'9' as c_uint).contains(&wc)
            || (b'a' as c_uint..=b'f' as c_uint).contains(&wc)
            || (b'A' as c_uint..=b'F' as c_uint).contains(&wc))
    {
        return true;
    }
    false
}

const WCTYPE_ALNUM: c_uint = 1 << 0;
const WCTYPE_ALPHA: c_uint = 1 << 1;
const WCTYPE_BLANK: c_uint = 1 << 2;
const WCTYPE_CNTRL: c_uint = 1 << 3;
const WCTYPE_DIGIT: c_uint = 1 << 4;
const WCTYPE_GRAPH: c_uint = 1 << 5;
const WCTYPE_LOWER: c_uint = 1 << 6;
const WCTYPE_PRINT: c_uint = 1 << 7;
const WCTYPE_PUNCT: c_uint = 1 << 8;
const WCTYPE_SPACE: c_uint = 1 << 9;
const WCTYPE_UPPER: c_uint = 1 << 10;
const WCTYPE_XDIGIT: c_uint = 1 << 11;

/// Map a `wctype(3)` name to its mask (0 if unknown).
pub(crate) fn wctype_name(name: &[u8]) -> c_uint {
    match name {
        b"alnum" => WCTYPE_ALNUM,
        b"alpha" => WCTYPE_ALPHA,
        b"blank" => WCTYPE_BLANK,
        b"cntrl" => WCTYPE_CNTRL,
        b"digit" => WCTYPE_DIGIT,
        b"graph" => WCTYPE_GRAPH,
        b"lower" => WCTYPE_LOWER,
        b"print" => WCTYPE_PRINT,
        b"punct" => WCTYPE_PUNCT,
        b"space" => WCTYPE_SPACE,
        b"upper" => WCTYPE_UPPER,
        b"xdigit" => WCTYPE_XDIGIT,
        _ => 0,
    }
}

const WCTRANS_TOLOWER: c_uint = 1;
const WCTRANS_TOUPPER: c_uint = 2;

pub(crate) fn wctrans_name(name: &[u8]) -> c_uint {
    match name {
        b"tolower" => WCTRANS_TOLOWER,
        b"toupper" => WCTRANS_TOUPPER,
        _ => 0,
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wctype(name: *const c_char) -> c_uint {
    if name.is_null() {
        return 0;
    }
    wctype_name(unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes())
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wctrans(name: *const c_char) -> c_uint {
    if name.is_null() {
        return 0;
    }
    wctrans_name(unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes())
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswctype(wc: c_uint, desc: c_uint) -> c_int {
    isw_ctype(wc, desc) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn towctrans(wc: c_uint, desc: c_uint) -> c_uint {
    match desc {
        WCTRANS_TOLOWER => unsafe { crate::c_wchar::towlower(wc) },
        WCTRANS_TOUPPER => unsafe { crate::c_wchar::towupper(wc) },
        _ => wc,
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswspace_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswspace(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswprint_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswprint(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswcntrl_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswcntrl(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswupper_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswupper(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswlower_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswlower(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswalpha_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswalpha(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswblank_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswblank(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswdigit_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswdigit(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswpunct_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswpunct(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswxdigit_l(wc: c_uint, _loc: LocaleT) -> c_int {
    unsafe { crate::c_wchar::iswxdigit(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iswctype_l(wc: c_uint, desc: c_uint, _loc: LocaleT) -> c_int {
    isw_ctype(wc, desc) as c_int
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn towupper_l(wc: c_uint, _loc: LocaleT) -> c_uint {
    unsafe { crate::c_wchar::towupper(wc) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn towlower_l(wc: c_uint, _loc: LocaleT) -> c_uint {
    unsafe { crate::c_wchar::towlower(wc) }
}

// ---- multibyte conversions (C locale: byte == wide char) ----

const WEOF: c_uint = c_uint::MAX;

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn btowc(c: c_int) -> c_uint {
    if (0..=255).contains(&c) {
        c as c_uint
    } else {
        WEOF
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wctob(wc: c_uint) -> c_int {
    if wc < 256 {
        wc as c_int
    } else {
        -1 // EOF
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcrtomb(s: *mut c_char, wc: c_uint, _ps: *mut c_void) -> usize {
    if s.is_null() {
        return 1; // state reset needs one byte for a NUL
    }
    if wc < 0x80 {
        unsafe { *s = wc as c_char };
        1
    } else {
        usize::MAX // (size_t)-1: EILSEQ
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mbrtowc(
    pwc: *mut crate::c_wchar::WChar,
    s: *const c_char,
    n: usize,
    _ps: *mut c_void,
) -> usize {
    if s.is_null() {
        return 0; // reset
    }
    if n == 0 {
        return usize::MAX - 1; // (size_t)-2: incomplete
    }
    let b = unsafe { *s } as u8;
    if b < 0x80 {
        if !pwc.is_null() {
            unsafe { *pwc = b as crate::c_wchar::WChar };
        }
        1
    } else {
        usize::MAX // (size_t)-1: EILSEQ
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mbtowc(
    pwc: *mut crate::c_wchar::WChar,
    pmb: *const c_char,
    max: usize,
) -> c_int {
    if pmb.is_null() {
        return 0; // not state-dependent
    }
    if max == 0 {
        return -1;
    }
    let b = unsafe { *pmb } as u8;
    if b < 0x80 {
        if !pwc.is_null() {
            unsafe { *pwc = b as crate::c_wchar::WChar };
        }
        1
    } else {
        -1
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mbrlen(s: *const c_char, n: usize, ps: *mut c_void) -> usize {
    unsafe { mbrtowc(core::ptr::null_mut(), s, n, ps) }
}

fn mbsrtowcs_impl(
    dest: *mut crate::c_wchar::WChar,
    src: *mut *const c_char,
    limit: usize,
    byte_limit: Option<usize>,
) -> usize {
    if src.is_null() || unsafe { *src }.is_null() {
        return usize::MAX;
    }
    let mut p = unsafe { *src };
    let mut n = 0usize;
    loop {
        if n >= limit {
            break;
        }
        if let Some(bmax) = byte_limit
            && n >= bmax
        {
            break;
        }
        let b = unsafe { *p } as u8;
        if b == 0 {
            if !dest.is_null() {
                unsafe { *src = core::ptr::null() };
            }
            break;
        }
        if b >= 0x80 {
            return usize::MAX; // EILSEQ
        }
        if !dest.is_null() {
            unsafe { *dest.add(n) = b as crate::c_wchar::WChar };
        }
        n += 1;
        p = unsafe { p.add(1) };
    }
    n
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mbsrtowcs(
    dest: *mut crate::c_wchar::WChar,
    src: *mut *const c_char,
    len: usize,
    _ps: *mut c_void,
) -> usize {
    mbsrtowcs_impl(dest, src, len, None)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mbsnrtowcs(
    dest: *mut crate::c_wchar::WChar,
    src: *mut *const c_char,
    nms: usize,
    len: usize,
    _ps: *mut c_void,
) -> usize {
    mbsrtowcs_impl(dest, src, len, Some(nms))
}

fn wcsnrtombs_impl(
    dest: *mut c_char,
    src: *mut *const crate::c_wchar::WChar,
    nwc: usize,
    len: usize,
) -> usize {
    if src.is_null() || unsafe { *src }.is_null() {
        return usize::MAX;
    }
    let mut p = unsafe { *src };
    let mut n = 0usize;
    loop {
        if n >= nwc || n >= len {
            break;
        }
        let wc = unsafe { *p } as u32;
        if wc == 0 {
            if !dest.is_null() {
                unsafe { *src = core::ptr::null() };
            }
            break;
        }
        if wc >= 0x80 {
            return usize::MAX; // EILSEQ
        }
        if !dest.is_null() {
            unsafe { *dest.add(n) = wc as c_char };
        }
        n += 1;
        p = unsafe { p.add(1) };
    }
    n
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsnrtombs(
    dest: *mut c_char,
    src: *mut *const crate::c_wchar::WChar,
    nwc: usize,
    len: usize,
    _ps: *mut c_void,
) -> usize {
    wcsnrtombs_impl(dest, src, nwc, len)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wcsrtombs(
    dest: *mut c_char,
    src: *mut *const crate::c_wchar::WChar,
    len: usize,
    _ps: *mut c_void,
) -> usize {
    wcsnrtombs_impl(dest, src, usize::MAX, len)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vasprintf(
    strp: *mut *mut c_char,
    fmt: *const c_char,
    ap: VaList<'_>,
) -> c_int {
    if strp.is_null() || fmt.is_null() {
        return crate::fail(22); // EINVAL
    }
    // Format into a fixed scratch buffer first, then copy to a malloc'd
    // buffer sized to the result.
    let mut scratch = [0u8; 512];
    let mut ap2 = ap.clone();
    let n = crate::c_stdio::format_to_buf(scratch.as_mut_ptr(), scratch.len(), fmt, &mut ap2);
    if n < 0 {
        return -1;
    }
    let total = (n as usize) + 1;
    let buf = unsafe { crate::malloc(total) };
    if buf.is_null() {
        return -1;
    }
    let copy = (n as usize).min(scratch.len() - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(scratch.as_ptr(), buf as *mut u8, copy);
        *((buf as *mut u8).add(copy)) = 0;
        *strp = buf as *mut c_char;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wctype_names_map_to_masks() {
        assert_eq!(wctype_name(b"digit"), WCTYPE_DIGIT);
        assert_eq!(wctype_name(b"upper"), WCTYPE_UPPER);
        assert_eq!(wctype_name(b"bogus"), 0);
    }

    #[test]
    fn wctrans_names_map() {
        assert_eq!(wctrans_name(b"tolower"), WCTRANS_TOLOWER);
        assert_eq!(wctrans_name(b"toupper"), WCTRANS_TOUPPER);
        assert_eq!(wctrans_name(b"bogus"), 0);
    }

    #[test]
    fn iswctype_masks() {
        assert!(isw_ctype(b'7' as c_uint, WCTYPE_DIGIT));
        assert!(isw_ctype(b'Q' as c_uint, WCTYPE_ALPHA | WCTYPE_UPPER));
        assert!(!isw_ctype(b'7' as c_uint, WCTYPE_ALPHA));
        assert!(!isw_ctype(b' ' as c_uint, WCTYPE_DIGIT));
    }

    #[test]
    fn lconv_is_c_locale() {
        let l = unsafe { &*core::ptr::addr_of!(LCONV) };
        assert_eq!(
            unsafe { core::ffi::CStr::from_ptr(l.decimal_point) }.to_bytes(),
            b"."
        );
        assert_eq!(
            unsafe { core::ffi::CStr::from_ptr(l.thousands_sep) }.to_bytes(),
            b""
        );
    }
}
