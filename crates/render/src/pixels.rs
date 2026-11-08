//! Pixel format conversion for the readback path.
//!
//! `glReadPixels` returns rows bottom to top and 8-bit RGBA. The shm pool, the
//! framebuffer and every other surface in this tree address rows top to bottom
//! and store `XRGB8888` little-endian, so the readback is flipped and swizzled
//! here, once, in a function with no GL in it.

/// Convert a bottom-up RGBA readback into a top-down `XRGB8888` buffer.
///
/// Returns `false` when either slice is too small for `width * height`, or when
/// that product overflows. A caller must treat `false` as a size error, not as a
/// short copy.
pub fn rgba_bottom_up_to_xrgb8888(src: &[u8], dst: &mut [u8], width: usize, height: usize) -> bool {
    let Some(len) = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return false;
    };
    if src.len() < len || dst.len() < len {
        return false;
    }

    for y in 0..height {
        let source = &src[(height - 1 - y) * width * 4..][..width * 4];
        let target = &mut dst[y * width * 4..][..width * 4];
        for x in 0..width {
            target[x * 4] = source[x * 4 + 2];
            target[x * 4 + 1] = source[x * 4 + 1];
            target[x * 4 + 2] = source[x * 4];
            target[x * 4 + 3] = 0;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn flips_rows_and_swizzles_channels() {
        // One row of two pixels, bottom first: red then blue.
        let src = vec![
            10, 20, 30, 255, // pixel 0: RGB(10,20,30)
            40, 50, 60, 255, // pixel 1: RGB(40,50,60)
        ];
        let mut dst = vec![0u8; 8];

        assert!(rgba_bottom_up_to_xrgb8888(&src, &mut dst, 2, 1));
        assert_eq!(dst[0..4], [30, 20, 10, 0]);
        assert_eq!(dst[4..8], [60, 50, 40, 0]);

        // Two rows, one pixel each: the lower row first in the readback.
        let src = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let mut dst = vec![0u8; 8];
        assert!(rgba_bottom_up_to_xrgb8888(&src, &mut dst, 1, 2));
        // The row that was below is now the second row.
        assert_eq!(dst[0..4], [60, 50, 40, 0]);
        assert_eq!(dst[4..8], [30, 20, 10, 0]);
    }

    #[test]
    fn rejects_buffers_that_are_too_small() {
        let src = vec![0u8; 8];
        let mut dst = vec![0u8; 8];
        assert!(!rgba_bottom_up_to_xrgb8888(&src, &mut dst, 2, 2));

        let src = vec![0u8; 16];
        let mut dst = vec![0u8; 15];
        assert!(!rgba_bottom_up_to_xrgb8888(&src, &mut dst, 2, 2));
    }

    #[test]
    fn rejects_a_size_product_that_overflows() {
        let src = Vec::new();
        let mut dst = Vec::new();
        assert!(!rgba_bottom_up_to_xrgb8888(&src, &mut dst, usize::MAX, 2));
    }

    #[test]
    fn larger_buffers_are_fine_and_untouched_past_the_image() {
        let src = vec![1u8; 32];
        let mut dst = vec![9u8; 32];
        assert!(rgba_bottom_up_to_xrgb8888(&src, &mut dst, 1, 1));
        assert_eq!(dst[0..4], [1, 1, 1, 0]);
        assert!(dst[4..].iter().all(|byte| *byte == 9));
    }
}
