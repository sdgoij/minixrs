//! Ext2 utility functions — adapted from `minix/fs/ext2/utility.c`

use crate::ext2::consts::*;
use crate::ext2::types::{BitchunkT, FS_BITCHUNK_BITS, fs_bitmap_chunks};

/// Return current time in seconds since epoch.
pub fn clock_time() -> i64 {
    // TODO: Wire up to system clock when kernel interface is available.
    0
}

/// Possibly swap a 16-bit word between little-endian and big-endian.
/// If `norm` is nonzero (native byte order), pass through unchanged.
/// If `norm` is 0 (BYTE_SWAP), swap bytes.
pub fn conv2(norm: i32, w: u32) -> u32 {
    if norm != 0 {
        w & 0xFFFF
    } else {
        ((w & 0xFF) << 8) | ((w >> 8) & 0xFF)
    }
}

/// Possibly swap a 32-bit value between little-endian and big-endian.
pub fn conv4(norm: i32, x: u32) -> u32 {
    if norm != 0 {
        x
    } else {
        let lo = conv2(FALSE, x & 0xFFFF);
        let hi = conv2(FALSE, (x >> 16) & 0xFFFF);
        (lo << 16) | hi
    }
}

/// Return the minimum of two unsigned values.
pub fn min_u(l: usize, r: usize) -> usize {
    if r >= l { l } else { r }
}

/// Default handler for unimplemented / invalid system calls.
pub fn no_sys() -> i32 {
    EINVAL
}

/// Compare non-null-terminated string with C-string.
pub fn ansi_strcmp(ansi_s: &[u8], s2: &[u8]) -> i32 {
    let len = ansi_s.len();
    if len == 0 {
        return 0;
    }
    for i in 0..len {
        if i >= s2.len() || s2[i] == 0 {
            return -1;
        }
        if ansi_s[i] != s2[i] {
            return -1;
        }
    }
    // All ansi_s characters matched. s2 is a C-string; check if
    // we're at the end (null terminator or end of slice).
    if len < s2.len() && s2[len] == 0 || len == s2.len() {
        0
    } else {
        -1
    }
}

/// Set a bit in bitmap, return its position or -1 on failure.
pub fn setbit(bitmap: &mut [BitchunkT], max_bits: u32, word: u32) -> i64 {
    let wlim_idx = fs_bitmap_chunks((max_bits >> 3) as usize);
    let wlim = if wlim_idx <= bitmap.len() {
        wlim_idx
    } else {
        bitmap.len()
    };

    for wptr_idx in (word as usize)..wlim {
        let k = bitmap[wptr_idx];
        if k == !0u32 {
            continue;
        }
        let mut k_mut = k;
        let mut i = 0;
        while (k_mut & (1u32 << i)) != 0 {
            i += 1;
        }
        let b = (wptr_idx as u32) * (FS_BITCHUNK_BITS as u32) + i;
        if b >= max_bits {
            continue;
        }
        k_mut |= 1u32 << i;
        bitmap[wptr_idx] = k_mut;
        return b as i64;
    }
    -1
}

/// Find and set a free byte in bitmap, return starting bit number or -1.
pub fn setbyte(bitmap: &mut [BitchunkT], max_bits: u32) -> i64 {
    let bmap = unsafe {
        core::slice::from_raw_parts_mut(
            bitmap.as_mut_ptr() as *mut u8,
            bitmap.len() * core::mem::size_of::<BitchunkT>(),
        )
    };
    let wlim = (max_bits >> 3) as usize;
    if wlim > bmap.len() {
        return -1;
    }
    for wptr_idx in 0..wlim {
        if bmap[wptr_idx] != 0 {
            continue;
        }
        let b = (wptr_idx as u32) * 8;
        if b + 8 >= max_bits {
            continue;
        }
        bmap[wptr_idx] = !0u8;
        return b as i64;
    }
    -1
}

/// Unset a bit in bitmap. Returns -1 if already free, 0 on success.
pub fn unsetbit(bitmap: &mut [BitchunkT], bit: u32) -> i32 {
    let word = (bit / (FS_BITCHUNK_BITS as u32)) as usize;
    let bit_in_word = bit % (FS_BITCHUNK_BITS as u32);
    let mask = 1u32 << bit_in_word;

    if word >= bitmap.len() {
        return -1;
    }
    let k = bitmap[word];
    if (k & mask) == 0 {
        return -1;
    }
    bitmap[word] = k & !mask;
    0
}

/// NUL-termination check (from C macro NUL → mfs_nul_f).
pub fn mfs_nul_f(_file: &str, _line: u32, _str: &[u8], _len: usize, _maxlen: usize) {
    // In Minix this would print a warning. Silently ignore in no_std.
}

/// Sanity check assertion (from C SANITYCHECK macro).
pub fn sanitycheck(_file: &str, _line: u32) {
    // Placeholder — actual checking logic would be wired here.
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

    #[test]
    fn test_ansi_strcmp_equal() {
        assert_eq!(ansi_strcmp(b"hello", b"hello"), 0);
    }

    #[test]
    fn test_ansi_strcmp_not_equal() {
        assert_eq!(ansi_strcmp(b"hello", b"world"), -1);
    }

    #[test]
    fn test_setbit_basic() {
        let mut bitmap = [0u32; 4];
        let r = setbit(&mut bitmap, 128, 0);
        assert!(r >= 0);
        assert_eq!(bitmap[0], 1);
    }

    #[test]
    fn test_setbit_already_set() {
        let mut bitmap = [!0u32; 4];
        bitmap[3] = 0;
        let r = setbit(&mut bitmap, 128, 0);
        // Should skip first 3 full words and allocate in 4th
        assert!(r >= 96); // 3 * 32
    }

    #[test]
    fn test_unsetbit_free() {
        let mut bitmap = [!0u32; 4];
        assert_eq!(unsetbit(&mut bitmap, 0), 0);
        assert_eq!(bitmap[0], !0u32 ^ 1);
    }

    #[test]
    fn test_unsetbit_already_free() {
        let mut bitmap = [0u32; 4];
        assert_eq!(unsetbit(&mut bitmap, 0), -1);
    }

    #[test]
    fn test_setbyte_finds_free_byte() {
        let mut bitmap = [0u32; 4];
        let r = setbyte(&mut bitmap, 128);
        assert_eq!(r, 0);
    }
}
