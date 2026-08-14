//! C time: civil-time conversion and the `time.h`/`sys/time.h` entry
//! points, all backed by `minix-std`'s `clock_gettime` for the wall clock.
//!
//! Ported from the old `tools/c-libc.c` (Howard Hinnant's civil-from-days
//! algorithm); the conversion helpers are target-independent and
//! host-tested.

#[cfg(target_os = "minix")]
use core::ffi::{c_char, c_void};
use core::ffi::{c_int, c_long};

/// C `time_t` (the headers define it as `long`).
pub type TimeT = c_long;

/// C `struct tm` (nine `int` fields, exactly as `tools/c-include/time.h`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tm {
    pub tm_sec: c_int,
    pub tm_min: c_int,
    pub tm_hour: c_int,
    pub tm_mday: c_int,
    pub tm_mon: c_int,
    pub tm_year: c_int,
    pub tm_wday: c_int,
    pub tm_yday: c_int,
    pub tm_isdst: c_int,
}

/// C `struct timeval` (`tools/c-include/sys/time.h`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeVal {
    pub tv_sec: c_long,
    pub tv_usec: c_long,
}

/// Howard Hinnant's civil-from-days algorithm (public domain).
pub(crate) fn civil_from_days(days: i64, out: &mut Tm) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    y += (m <= 2) as i64;
    out.tm_year = (y - 1900) as c_int;
    out.tm_mon = (m - 1) as c_int;
    out.tm_mday = d as c_int;
    out.tm_yday = doy as c_int;
    let mut wday = ((days + 4) % 7) as c_int;
    if wday < 0 {
        wday += 7;
    }
    out.tm_wday = wday;
}

pub(crate) fn secs_to_tm(t: TimeT, out: &mut Tm) {
    let mut days = t as i64 / 86400;
    let mut rem = t as i64 % 86400;
    if rem < 0 {
        rem += 86400;
        days -= 1;
    }
    out.tm_hour = (rem / 3600) as c_int;
    rem %= 3600;
    out.tm_min = (rem / 60) as c_int;
    out.tm_sec = (rem % 60) as c_int;
    out.tm_isdst = 0;
    civil_from_days(days, out);
}

pub(crate) fn mktime_core(tm: &Tm) -> TimeT {
    let mut y = tm.tm_year as i64 + 1900;
    let m = tm.tm_mon as i64 + 1;
    let d = tm.tm_mday as i64;
    y -= (m <= 2) as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp_shifted = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp_shifted + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u32;
    let days = era * 146097 + doe as i64 - 719468;
    let secs = days * 86400 + tm.tm_hour as i64 * 3600 + tm.tm_min as i64 * 60 + tm.tm_sec as i64;
    secs as TimeT
}

fn fmt_int(s: &mut [u8], mut v: i32, width: usize, padc: u8) -> usize {
    let mut tmp = [0u8; 12];
    let mut n = 0;
    let neg = v < 0;
    if neg {
        v = -v;
    }
    loop {
        tmp[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let total = n + neg as usize;
    let written = total.max(width);
    let mut i = 0usize;
    if neg && i + 1 < s.len() {
        s[i] = b'-';
        i += 1;
    }
    let padn = width as i32 - n as i32 - neg as i32;
    for _ in 0..padn.max(0) {
        if i + 1 < s.len() {
            s[i] = padc;
            i += 1;
        }
    }
    for k in (0..n).rev() {
        if i + 1 < s.len() {
            s[i] = tmp[k];
            i += 1;
        }
    }
    written
}

const DAY_ABBR: [&[u8]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
const DAY_FULL: [&[u8]; 7] = [
    b"Sunday",
    b"Monday",
    b"Tuesday",
    b"Wednesday",
    b"Thursday",
    b"Friday",
    b"Saturday",
];
const MON_ABBR: [&[u8]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];
const MON_FULL: [&[u8]; 12] = [
    b"January",
    b"February",
    b"March",
    b"April",
    b"May",
    b"June",
    b"July",
    b"August",
    b"September",
    b"October",
    b"November",
    b"December",
];

pub(crate) fn strftime_core(s: &mut [u8], format: &[u8], tm: &Tm) -> usize {
    let mut n = 0usize;
    let mut p = 0usize;
    while p < format.len() && n + 1 < s.len() {
        if format[p] != b'%' {
            s[n] = format[p];
            n += 1;
            p += 1;
            continue;
        }
        p += 1;
        if p >= format.len() {
            break;
        }
        let year = tm.tm_year + 1900;
        match format[p] {
            b'Y' => n += fmt_int(&mut s[n..], year, 4, b'0'),
            b'y' => n += fmt_int(&mut s[n..], year % 100, 2, b'0'),
            b'm' => n += fmt_int(&mut s[n..], tm.tm_mon + 1, 2, b'0'),
            b'd' => n += fmt_int(&mut s[n..], tm.tm_mday, 2, b'0'),
            b'e' => n += fmt_int(&mut s[n..], tm.tm_mday, 2, b' '),
            b'H' => n += fmt_int(&mut s[n..], tm.tm_hour, 2, b'0'),
            b'I' => {
                let mut h = tm.tm_hour % 12;
                if h == 0 {
                    h = 12;
                }
                n += fmt_int(&mut s[n..], h, 2, b'0');
            }
            b'M' => n += fmt_int(&mut s[n..], tm.tm_min, 2, b'0'),
            b'S' => n += fmt_int(&mut s[n..], tm.tm_sec, 2, b'0'),
            b'j' => n += fmt_int(&mut s[n..], tm.tm_yday + 1, 3, b'0'),
            b'w' => n += fmt_int(&mut s[n..], tm.tm_wday, 1, b'0'),
            b'a' => {
                let x = DAY_ABBR[tm.tm_wday as usize];
                let c = x.len().min(s.len() - n - 1);
                s[n..n + c].copy_from_slice(&x[..c]);
                n += c;
            }
            b'A' => {
                let x = DAY_FULL[tm.tm_wday as usize];
                let c = x.len().min(s.len() - n - 1);
                s[n..n + c].copy_from_slice(&x[..c]);
                n += c;
            }
            b'b' | b'h' => {
                let x = MON_ABBR[tm.tm_mon as usize];
                let c = x.len().min(s.len() - n - 1);
                s[n..n + c].copy_from_slice(&x[..c]);
                n += c;
            }
            b'B' => {
                let x = MON_FULL[tm.tm_mon as usize];
                let c = x.len().min(s.len() - n - 1);
                s[n..n + c].copy_from_slice(&x[..c]);
                n += c;
            }
            b'p' => {
                let x = if tm.tm_hour < 12 { b"AM" } else { b"PM" };
                let c = x.len().min(s.len() - n - 1);
                s[n..n + c].copy_from_slice(&x[..c]);
                n += c;
            }
            b'T' => n += strftime_core(&mut s[n..], b"%H:%M:%S", tm),
            b'D' => n += strftime_core(&mut s[n..], b"%m/%d/%y", tm),
            b'F' => n += strftime_core(&mut s[n..], b"%Y-%m-%d", tm),
            b'R' => n += strftime_core(&mut s[n..], b"%H:%M", tm),
            b'x' => n += strftime_core(&mut s[n..], b"%m/%d/%y", tm),
            b'X' => n += strftime_core(&mut s[n..], b"%H:%M:%S", tm),
            b'c' => n += strftime_core(&mut s[n..], b"%a %b %e %H:%M:%S %Y", tm),
            b'Z' => {
                let x = b"UTC";
                let c = x.len().min(s.len() - n - 1);
                s[n..n + c].copy_from_slice(&x[..c]);
                n += c;
            }
            b'z' => {
                let x = b"+0000";
                let c = x.len().min(s.len() - n - 1);
                s[n..n + c].copy_from_slice(&x[..c]);
                n += c;
            }
            b'%' => {
                s[n] = b'%';
                n += 1;
            }
            other => {
                s[n] = other;
                n += 1;
            }
        }
        p += 1;
    }
    if n < s.len() {
        s[n] = 0;
    }
    n
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn time(tloc: *mut TimeT) -> TimeT {
    match minix_std::time::clock_gettime(minix_std::time::CLOCK_REALTIME) {
        Ok(ts) => {
            if !tloc.is_null() {
                unsafe { *tloc = ts.tv_sec as TimeT };
            }
            ts.tv_sec as TimeT
        }
        Err(_) => {
            if !tloc.is_null() {
                unsafe { *tloc = -1 };
            }
            -1
        }
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gettimeofday(tv: *mut TimeVal, tz: *mut c_void) -> c_int {
    let _ = tz;
    if tv.is_null() {
        return -1;
    }
    match minix_std::time::clock_gettime(minix_std::time::CLOCK_REALTIME) {
        Ok(ts) => {
            unsafe {
                (*tv).tv_sec = ts.tv_sec as c_long;
                (*tv).tv_usec = (ts.tv_nsec / 1000) as c_long;
            }
            0
        }
        Err(_) => -1,
    }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn localtime_r(timep: *const TimeT, result: *mut Tm) -> *mut Tm {
    if timep.is_null() || result.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { secs_to_tm(*timep, &mut *result) };
    result
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gmtime_r(timep: *const TimeT, result: *mut Tm) -> *mut Tm {
    if timep.is_null() || result.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { secs_to_tm(*timep, &mut *result) };
    result
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mktime(tm: *mut Tm) -> TimeT {
    if tm.is_null() {
        return -1;
    }
    unsafe { mktime_core(&*tm) }
}

#[cfg(target_os = "minix")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strftime(
    s: *mut c_char,
    max: usize,
    format: *const c_char,
    tm: *const Tm,
) -> usize {
    if s.is_null() || format.is_null() || tm.is_null() || max == 0 {
        return 0;
    }
    let fmt = unsafe { core::ffi::CStr::from_ptr(format) }.to_bytes();
    let out = unsafe { core::slice::from_raw_parts_mut(s as *mut u8, max) };
    strftime_core(out, fmt, unsafe { &*tm })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mktime_localtime_roundtrip() {
        let mut tm = Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 1,
            tm_mon: 0,
            tm_year: 70,
            tm_wday: 4,
            tm_yday: 0,
            tm_isdst: 0,
        };
        // 1970-01-01 00:00:00 UTC
        assert_eq!(mktime_core(&tm), 0);
        secs_to_tm(0, &mut tm);
        assert_eq!(tm.tm_year, 70);
        assert_eq!(tm.tm_mon, 0);
        assert_eq!(tm.tm_mday, 1);
        assert_eq!(tm.tm_wday, 4); // Thursday
    }

    #[test]
    fn civil_epoch_and_leap_year() {
        let mut tm = Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 0,
            tm_mon: 0,
            tm_year: 0,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
        };
        secs_to_tm(0, &mut tm);
        assert_eq!((tm.tm_year, tm.tm_mon, tm.tm_mday), (70, 0, 1));

        // 2024-02-29 (leap year) = 1970 + 54 years.
        let t = mktime_core(&Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 29,
            tm_mon: 1,
            tm_year: 124,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
        });
        secs_to_tm(t, &mut tm);
        assert_eq!((tm.tm_year, tm.tm_mon, tm.tm_mday), (124, 1, 29));
    }

    #[test]
    fn strftime_basic() {
        let tm = Tm {
            tm_sec: 5,
            tm_min: 4,
            tm_hour: 3,
            tm_mday: 2,
            tm_mon: 1,
            tm_year: 124,
            tm_wday: 5,
            tm_yday: 32,
            tm_isdst: 0,
        };
        let mut buf = [0u8; 64];
        let n = strftime_core(&mut buf, b"%Y-%m-%d %H:%M:%S %a", &tm);
        assert_eq!(&buf[..n], b"2024-02-02 03:04:05 Fri");
    }

    #[test]
    fn strftime_bounded() {
        let tm = Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 1,
            tm_mon: 0,
            tm_year: 124,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
        };
        let mut buf = [0u8; 5];
        let n = strftime_core(&mut buf, b"%Y-%m-%d", &tm);
        // The C loop stops once n + 1 == max, so only %Y fits in 5 bytes.
        assert_eq!(n, 4);
        assert_eq!(&buf[..4], b"2024");
        assert_eq!(buf[4], 0);
    }
}
