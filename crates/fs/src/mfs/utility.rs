//! MFS utility functions — adapted from `minix/fs/mfs/utility.c`

use crate::mfs::consts::*;

/// Return current time in seconds since epoch.
/// Stubbed to `todo!()` — depends on Minix kernel `getuptime`.
// Reference: utility.c clock_time()
pub fn clock_time() -> i64 {
    // TODO: Wire up to system clock when kernel interface is available.
    // In Minix this calls getuptime(&uptime, &realtime, &boottime).
    // For now, return 0 as a placeholder.
    0
}

/// Possibly swap a 16-bit word between little-endian and big-endian.
/// If `norm` is nonzero (native byte order), pass through unchanged.
/// If `norm` is 0 (BYTE_SWAP), swap bytes.
// Reference: utility.c conv2()
pub fn conv2(norm: i32, w: i32) -> u32 {
    if norm != 0 {
        (w as u32) & 0xFFFF
    } else {
        let w = w as u32;
        ((w & 0xFF) << 8) | ((w >> 8) & 0xFF)
    }
}

/// Possibly swap a 32-bit long between little-endian and big-endian.
/// If `norm` is nonzero (native byte order), pass through unchanged.
/// If `norm` is 0 (BYTE_SWAP), swap bytes.
// Reference: utility.c conv4()
pub fn conv4(norm: i32, x: i64) -> i64 {
    if norm != 0 {
        x
    } else {
        let lo = conv2(FALSE, (x as i32) & 0xFFFF);
        let hi = conv2(FALSE, ((x >> 16) as i32) & 0xFFFF);
        ((lo as i64) << 16) | (hi as i64)
    }
}

/// Return the minimum of two unsigned values.
// Reference: utility.c mfs_min()
pub fn min_u(l: usize, r: usize) -> usize {
    if r >= l { l } else { r }
}

/// Default handler for unimplemented / invalid system calls.
/// Returns EINVAL.
// Reference: utility.c no_sys()
pub fn no_sys() -> i32 {
    EINVAL
}

/// NUL-termination check (from C macro NUL → mfs_nul_f).
/// Logs a warning if the string was not null-terminated within maxlen.
// Reference: utility.c mfs_nul_f()
pub fn mfs_nul_f(file: &str, line: u32, _str: &[u8], len: usize, maxlen: usize) {
    if len < maxlen && len > 0 && _str[len - 1] != 0 {
        // In Minix this would print a warning. Silently ignore in no_std.
        // printf!("MFS {}:{} string (length {}, maxlen {}) not null-terminated\n",
        //         file, line, len, maxlen);
        let _ = file;
        let _ = line;
    }
}

/// Sanity check assertion (from C SANITYCHECK macro).
/// In Minix this panics on failure.
// Reference: utility.c sanitycheck()
pub fn sanitycheck(file: &str, line: u32) {
    // Placeholder — actual checking logic would be wired here.
    let _ = file;
    let _ = line;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conv2_native() {
        assert_eq!(conv2(1, 0x1234), 0x1234);
    }

    #[test]
    fn test_conv2_swap() {
        assert_eq!(conv2(0, 0x1234), 0x3412);
    }

    #[test]
    fn test_conv4_native() {
        assert_eq!(conv4(1, 0x12345678), 0x12345678);
    }

    #[test]
    fn test_conv4_swap() {
        // conv4(BYTE_SWAP, 0x12345678):
        // lo = conv2(0, 0x5678) = 0x7856
        // hi = conv2(0, 0x1234) = 0x3412
        // result = (0x7856 << 16) | 0x3412 = 0x78563412
        assert_eq!(conv4(0, 0x12345678), 0x78563412);
    }

    #[test]
    fn test_min_u() {
        assert_eq!(min_u(5, 10), 5);
        assert_eq!(min_u(10, 10), 10);
        assert_eq!(min_u(10, 5), 5);
    }

    #[test]
    fn test_no_sys() {
        assert_eq!(no_sys(), EINVAL);
    }
}
