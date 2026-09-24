//! C `netinet/in.h` and `arpa/inet.h`: the Internet address conversions, and
//! the byte-order helpers C code does around them.
//!
//! The socket calls themselves are in `lib.rs` (they arrived with the net
//! server); this is what C code does with the addresses it hands them. The
//! parsing and formatting are pure, so the host suite covers them.

#[cfg(target_os = "minix")]
use core::ffi::{c_char, c_int};

/// The 32-bit round trips. The wire carries network order and the machine
/// works in host order, so these are conversions, not a choice of byte order.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn htons(hostshort: u16) -> u16 {
    hostshort.to_be()
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn htonl(hostlong: u32) -> u32 {
    hostlong.to_be()
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn ntohs(netshort: u16) -> u16 {
    u16::from_be(netshort)
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub extern "C" fn ntohl(netlong: u32) -> u32 {
    u32::from_be(netlong)
}

/// The dotted-quad parser behind `inet_addr`/`inet_aton`, answering in host
/// order.
///
/// Up to four decimal parts, the last of which fills what the earlier ones
/// leave — `a.b` is `a` in the top byte with `b` in the rest — which is the
/// form the original implementation accepted and every implementation since
/// has kept. Each part is decimal: no sign, no `0x`, no octal.
fn parse_ipv4(s: &[u8]) -> Option<u32> {
    let mut parts = [0u32; 4];
    let mut count = 0;
    for field in s.split(|b| *b == b'.') {
        if count == 4 || field.is_empty() || field.len() > 9 {
            return None;
        }
        let mut v: u32 = 0;
        for c in field {
            if !c.is_ascii_digit() {
                return None;
            }
            v = v.checked_mul(10)?.checked_add(u32::from(*c - b'0'))?;
        }
        parts[count] = v;
        count += 1;
    }
    match count {
        1 => Some(parts[0]),
        2 if parts[0] <= 0xff && parts[1] <= 0x00ff_ffff => Some((parts[0] << 24) | parts[1]),
        3 if parts[0] <= 0xff && parts[1] <= 0xff && parts[2] <= 0xffff => {
            Some((parts[0] << 24) | (parts[1] << 16) | parts[2])
        }
        4 if parts.iter().all(|p| *p <= 0xff) => {
            Some((parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3])
        }
        _ => None,
    }
}

/// The dotted quad for `addr` (network order), NUL-terminated in `out`.
/// Returns the length without the terminator; `out` needs room for 16.
fn format_ipv4(addr: u32, out: &mut [u8; 16]) -> usize {
    let octets = u32::from_be(addr).to_be_bytes();
    let mut n = 0;
    for (i, o) in octets.iter().enumerate() {
        if i > 0 {
            out[n] = b'.';
            n += 1;
        }
        if *o >= 100 {
            out[n] = b'0' + o / 100;
            n += 1;
        }
        if *o >= 10 {
            out[n] = b'0' + (o % 100) / 10;
            n += 1;
        }
        out[n] = b'0' + o % 10;
        n += 1;
    }
    out[n] = 0;
    n
}

/// `inet_addr()`: `cp` as a network-order address, or `INADDR_NONE`
/// (`0xffffffff`) when it is not one.
///
/// `255.255.255.255` is such an address, so it is indistinguishable from
/// failure here — the wart that made `inet_aton` the interface to use.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inet_addr(cp: *const c_char) -> u32 {
    const INADDR_NONE: u32 = 0xffff_ffff;
    if cp.is_null() {
        return INADDR_NONE;
    }
    let text = unsafe { core::ffi::CStr::from_ptr(cp) }.to_bytes();
    match parse_ipv4(text) {
        Some(a) => a.to_be(),
        None => INADDR_NONE,
    }
}

/// `inet_aton()`: as `inet_addr`, but writing the address through `inp` and
/// answering 1 or 0, so every address is reportable.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inet_aton(cp: *const c_char, inp: *mut u32) -> c_int {
    if cp.is_null() {
        return 0;
    }
    let text = unsafe { core::ffi::CStr::from_ptr(cp) }.to_bytes();
    match parse_ipv4(text) {
        Some(a) => {
            if !inp.is_null() {
                unsafe { *inp = a.to_be() };
            }
            1
        }
        None => 0,
    }
}

/// The buffer `inet_ntoa` formats into. One buffer for the process, as the C
/// interface specifies — which is also why it is not thread-safe.
static mut NTOA_BUF: [u8; 16] = [0; 16];

/// `inet_ntoa()`: the dotted quad for `in` (network order), in a static buffer
/// that the next call overwrites.
#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inet_ntoa(in_: u32) -> *mut c_char {
    let mut text = [0u8; 16];
    let n = format_ipv4(in_, &mut text);
    let buf = core::ptr::addr_of_mut!(NTOA_BUF) as *mut u8;
    unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), buf, n + 1) };
    buf as *mut c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Option<u32> {
        parse_ipv4(s.as_bytes())
    }

    #[test]
    fn parse_ipv4_reads_the_standard_forms() {
        assert_eq!(parse("127.0.0.1"), Some(0x7f00_0001));
        assert_eq!(parse("255.255.255.255"), Some(0xffff_ffff));
        assert_eq!(parse("0.0.0.0"), Some(0));
        // The short forms: the last part fills what the earlier ones leave.
        assert_eq!(parse("127.1"), Some(0x7f00_0001));
        assert_eq!(parse("10.1.2"), Some(0x0a01_0002));
        assert_eq!(parse("0x7f"), None);
    }

    #[test]
    fn parse_ipv4_rejects_what_is_not_an_address() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("1.2.3.4.5"), None);
        assert_eq!(parse("1.2.3."), None);
        assert_eq!(parse("256.0.0.1"), None);
        assert_eq!(parse("1.2.3.4x"), None);
        assert_eq!(parse("1.-2.3.4"), None);
        // A part that does not fit the space left for it.
        assert_eq!(parse("1.16777216"), None);
        // Ten digits overflows the accumulator rather than wrapping.
        assert_eq!(parse("99999999999"), None);
    }

    #[test]
    fn format_ipv4_writes_a_dotted_quad() {
        let mut out = [0u8; 16];
        let n = format_ipv4(0x7f00_0001u32.to_be(), &mut out);
        assert_eq!(&out[..n], b"127.0.0.1");
        assert_eq!(out[n], 0);
        let n = format_ipv4(0xffff_ffffu32.to_be(), &mut out);
        assert_eq!(&out[..n], b"255.255.255.255");
        let n = format_ipv4(0u32.to_be(), &mut out);
        assert_eq!(&out[..n], b"0.0.0.0");
    }
}
