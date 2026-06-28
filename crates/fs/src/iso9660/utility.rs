//! ISO 9660 utility functions — adapted from `minix/fs/iso9660fs/utility.c`

use crate::iso9660::consts::*;

/// Date fields of a `DirRecord` contain:
///   [0]: year  (offset from 1900)
///   [1]: month (1-12)
///   [2]: day   (1-31)
///   [3]: hour  (0-23)
///   [4]: min   (0-59)
///   [5]: sec   (0-59)
///   [6]: GMT offset in quarter-hours (signed, encoded as u8)
///
/// Convert to a Unix timestamp (seconds since epoch).
pub fn iso_date_to_unix(rec_date: &[u8; 7]) -> i64 {
    let year = rec_date[0] as i64 + 1900;
    let month = rec_date[1] as i64;
    let day = rec_date[2] as i64;
    let hour = rec_date[3] as i64;
    let min = rec_date[4] as i64;
    let sec = rec_date[5] as i64;

    // Days since 1970-01-01 using a simple algorithm.
    let days = days_from_ymd(year, month, day);
    let total_secs = days * 86400 + hour * 3600 + min * 60 + sec;

    // Apply GMT offset: rec_date[6] is in quarter-hours, signed.
    // For 0 (unspecified), treat as 0.
    let gmt_quarter = rec_date[6] as i8 as i64;
    total_secs.saturating_sub(gmt_quarter * 900)
}

/// Number of days from 1970-01-01 to (year, month, day).
fn days_from_ymd(year: i64, month: i64, day: i64) -> i64 {
    // Count days from year 0 to the given date.
    let y = year - 1;
    let total = y * 365 + y / 4 - y / 100 + y / 400;

    static DAYS_IN_MONTH: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let month_days: i64 = (1..month).map(|m| DAYS_IN_MONTH[(m - 1) as usize]).sum();
    let leap_extra = if month > 2 && is_leap_year(year) {
        1
    } else {
        0
    };
    let total = total + month_days + leap_extra + day - 1;

    // Days from year 0 to 1970-01-01
    let epoch_start = 1969 * 365 + 1969 / 4 - 1969 / 100 + 1969 / 400;
    total - epoch_start
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// `no_sys()` — default handler for unimplemented system calls.
/// Returns EINVAL.
// Reference: utility.c no_sys()
pub fn no_sys() -> i32 {
    EINVAL
}

/// `do_noop()` — do nothing, return OK.
// Reference: utility.c do_noop()
pub fn do_noop() -> i32 {
    OK
}

/// Copy bytes from source to destination (like memcpy).
/// Returns the number of bytes copied.
pub fn memcpy_bytes(dst: &mut [u8], src: &[u8]) -> usize {
    let len = core::cmp::min(dst.len(), src.len());
    dst[..len].copy_from_slice(&src[..len]);
    len
}

/// Read a little-endian u16 from a byte buffer.
pub fn read_le_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

/// Read a little-endian u32 from a byte buffer.
pub fn read_le_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

/// Read a big-endian u16 from a byte buffer.
pub fn read_be_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([buf[offset], buf[offset + 1]])
}

/// Read a big-endian u32 from a byte buffer.
pub fn read_be_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_sys() {
        assert_eq!(no_sys(), EINVAL);
    }

    #[test]
    fn test_do_noop() {
        assert_eq!(do_noop(), OK);
    }

    #[test]
    fn test_memcpy_bytes() {
        let mut dst = [0u8; 10];
        let src = [1u8, 2, 3];
        assert_eq!(memcpy_bytes(&mut dst, &src), 3);
        assert_eq!(dst[..3], src);
    }

    #[test]
    fn test_read_le_u16() {
        let buf = [0x34, 0x12, 0x00, 0x00];
        assert_eq!(read_le_u16(&buf, 0), 0x1234);
    }

    #[test]
    fn test_read_be_u16() {
        let buf = [0x12, 0x34, 0x00, 0x00];
        assert_eq!(read_be_u16(&buf, 0), 0x1234);
    }

    #[test]
    fn test_read_le_u32() {
        let buf = [0x78, 0x56, 0x34, 0x12];
        assert_eq!(read_le_u32(&buf, 0), 0x12345678);
    }

    #[test]
    fn test_read_be_u32() {
        let buf = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(read_be_u32(&buf, 0), 0x12345678);
    }

    #[test]
    fn test_iso_date_to_unix() {
        // 2025-01-15 12:30:00 GMT
        let date = [125, 1, 15, 12, 30, 0, 0];
        let ts = iso_date_to_unix(&date);
        assert!(ts > 1700000000);
        assert!(ts < 1800000000);
    }

    #[test]
    fn test_days_from_ymd() {
        // 1970-01-01 should be 0
        assert_eq!(days_from_ymd(1970, 1, 1), 0);
        // 1970-01-02 should be 1
        assert_eq!(days_from_ymd(1970, 1, 2), 1);
    }
}
