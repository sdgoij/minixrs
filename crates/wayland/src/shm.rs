//! `wl_shm`: validating a shared-memory buffer and blitting it to a
//! framebuffer.
//!
//! Pure — the caller supplies the pool bytes and a closure that receives each
//! destination pixel, so the arithmetic (pool offset, row stride, and clipping a
//! buffer whose origin is partly off-screen) is host-testable without mapping
//! anything or touching a device.

use crate::protocol;

/// Every accepted format is 32-bit; `wl_shm` has no sub-32-bit format here.
pub const BYTES_PER_PIXEL: i32 = 4;

/// Why a `wl_shm` buffer is not usable. Each maps to a `wl_shm.error` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmError {
    /// The pixel format is neither `ARGB8888` nor `XRGB8888`.
    Format,
    /// The stride is smaller than a row, or not a whole number of pixels.
    Stride,
    /// The image does not fit inside its pool, or the mapping is short.
    Bounds,
    /// The pool fd was missing or negative.
    Fd,
}

impl ShmError {
    /// The `wl_shm.error` code the client is told.
    pub fn code(self) -> u32 {
        match self {
            ShmError::Format => protocol::WL_SHM_ERROR_INVALID_FORMAT,
            ShmError::Stride => protocol::WL_SHM_ERROR_INVALID_STRIDE,
            // There is no distinct "too big" code; an extent that does not fit
            // its pool is reported the way wayland reports a bad stride.
            ShmError::Bounds => protocol::WL_SHM_ERROR_INVALID_STRIDE,
            ShmError::Fd => protocol::WL_SHM_ERROR_INVALID_FD,
        }
    }

    /// The message sent alongside [`ShmError::code`].
    pub fn message(self) -> &'static [u8] {
        match self {
            ShmError::Format => b"invalid format",
            ShmError::Stride => b"invalid stride",
            ShmError::Bounds => b"buffer does not fit its pool",
            ShmError::Fd => b"invalid pool fd",
        }
    }
}

/// A `wl_buffer` made from a `wl_shm_pool`, described relative to its pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffer {
    /// Where the image starts inside the pool.
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: u32,
    /// The pool's size, from `wl_shm.create_pool`.
    pub pool_size: i32,
}

/// A rectangle, in surface or buffer coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Buffer {
    /// Check the buffer against its pool. A client that sends a bad buffer is
    /// told why, rather than being handed garbage.
    pub fn validate(&self) -> Result<(), ShmError> {
        match self.format {
            protocol::WL_SHM_FORMAT_ARGB8888 | protocol::WL_SHM_FORMAT_XRGB8888 => {}
            _ => return Err(ShmError::Format),
        }
        // Geometry that cannot describe an image is a bounds problem, not a
        // stride one — the client learns its buffer does not fit.
        if self.width <= 0 || self.height <= 0 || self.offset < 0 || self.pool_size < 0 {
            return Err(ShmError::Bounds);
        }
        if self.stride <= 0 || self.stride % BYTES_PER_PIXEL != 0 {
            return Err(ShmError::Stride);
        }
        let row = self
            .width
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or(ShmError::Bounds)?;
        if self.stride < row {
            return Err(ShmError::Stride);
        }
        if self.byte_len().ok_or(ShmError::Bounds)? > self.pool_size as usize {
            return Err(ShmError::Bounds);
        }
        Ok(())
    }

    /// The part of this buffer a damage rectangle covers, and where that part's
    /// top-left sits in the buffer's own coordinates.
    ///
    /// `wl_surface.damage` marks what a commit changed, so a compositor has only
    /// that much to recomposite (2d). A rectangle that misses the buffer clips to
    /// nothing and answers `None`.
    pub fn damage_slice(&self, r: Rect) -> Option<(Buffer, i32, i32)> {
        let x0 = r.x.max(0);
        let y0 = r.y.max(0);
        let x1 = (r.x + r.w).min(self.width);
        let y1 = (r.y + r.h).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        Some((
            Buffer {
                offset: self.offset + y0 * self.stride + x0 * BYTES_PER_PIXEL,
                width: x1 - x0,
                height: y1 - y0,
                stride: self.stride,
                format: self.format,
                pool_size: self.pool_size,
            },
            x0,
            y0,
        ))
    }

    /// How many bytes the image occupies in the pool, offset included, or `None`
    /// if the geometry overflows.
    pub fn byte_len(&self) -> Option<usize> {
        let end = self
            .height
            .checked_mul(self.stride)
            .and_then(|image| image.checked_add(self.offset))?;
        usize::try_from(end).ok()
    }
}

/// Where a blit places a buffer, and how big the destination is. `x`/`y` may be
/// negative — a buffer can hang off the left or top edge — and the part outside
/// the destination is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Destination {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Copy a validated buffer's pixels to a destination, row by row, clipping to
/// the destination's bounds.
///
/// `put` receives each pixel in destination coordinates. The value is the
/// little-endian `u32` the pool holds, which for `ARGB8888`/`XRGB8888` is the
/// framebuffer's own byte order (`B,G,R,A`); the caller decides whether to blend
/// it. The caller resolves and maps the pool fd and passes the mapping as `src`,
/// which keeps this testable without a device.
pub fn blit<F>(src: &[u8], buf: &Buffer, dst: Destination, mut put: F) -> Result<(), ShmError>
where
    F: FnMut(i32, i32, u32),
{
    buf.validate()?;
    let end = buf.byte_len().ok_or(ShmError::Bounds)?;
    if src.len() < end {
        return Err(ShmError::Bounds);
    }
    let image = &src[buf.offset as usize..end];
    let stride = buf.stride as usize;
    for row in 0..buf.height {
        let dy = dst.y as i64 + row as i64;
        if dy < 0 || dy >= dst.height as i64 {
            continue;
        }
        let row_off = row as usize * stride;
        let line = &image[row_off..row_off + stride];
        for col in 0..buf.width {
            let dx = dst.x as i64 + col as i64;
            if dx < 0 || dx >= dst.width as i64 {
                continue;
            }
            let p = col as usize * BYTES_PER_PIXEL as usize;
            let pixel = u32::from_le_bytes([line[p], line[p + 1], line[p + 2], line[p + 3]]);
            put(dx as i32, dy as i32, pixel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(offset: i32, width: i32, height: i32, stride: i32, pool_size: i32) -> Buffer {
        Buffer {
            offset,
            width,
            height,
            stride,
            format: protocol::WL_SHM_FORMAT_ARGB8888,
            pool_size,
        }
    }

    /// Blit into a fixed array and return the pixels delivered, with the count.
    fn collect<const N: usize>(
        src: &[u8],
        buf: &Buffer,
        dst: Destination,
    ) -> ([(i32, i32, u32); N], usize) {
        let mut out = [(0i32, 0i32, 0u32); N];
        let mut n = 0usize;
        blit(src, buf, dst, |x, y, p| {
            out[n] = (x, y, p);
            n += 1;
        })
        .unwrap();
        (out, n)
    }

    #[test]
    fn validate_rejects_a_bad_format() {
        let mut b = buffer(0, 2, 2, 8, 16);
        b.format = 0xdead;
        assert_eq!(b.validate(), Err(ShmError::Format));
    }

    #[test]
    fn validate_rejects_a_short_stride() {
        // A width of 4 needs 16 bytes per row; a stride of 8 under-serves it.
        assert_eq!(buffer(0, 4, 1, 8, 64).validate(), Err(ShmError::Stride));
    }

    #[test]
    fn validate_rejects_non_pixel_strides() {
        // 18 is a whole row for width 4 in bytes, but not a whole pixel.
        assert_eq!(buffer(0, 4, 1, 18, 64).validate(), Err(ShmError::Stride));
    }

    #[test]
    fn validate_rejects_an_image_past_the_pool() {
        // Two rows of 8 bytes is 16, plus an 8-byte offset is 24 > pool 16.
        assert_eq!(buffer(8, 2, 2, 8, 16).validate(), Err(ShmError::Bounds));
    }

    #[test]
    fn validate_accepts_a_well_formed_buffer() {
        assert_eq!(buffer(0, 2, 2, 8, 16).validate(), Ok(()));
        // A stride wider than the row is legal padding.
        assert_eq!(buffer(0, 2, 2, 16, 32).validate(), Ok(()));
        // So is an image that only fills part of the pool.
        assert_eq!(buffer(0, 2, 2, 8, 4096).validate(), Ok(()));
    }

    #[test]
    fn blit_copies_a_whole_image() {
        let src = [1u8, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0];
        let buf = buffer(0, 2, 2, 8, 16);
        let (out, n) = collect::<4>(
            &src,
            &buf,
            Destination {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        );
        assert_eq!(n, 4);
        assert_eq!(out[0], (0, 0, 1));
        assert_eq!(out[1], (1, 0, 2));
        assert_eq!(out[2], (0, 1, 3));
        assert_eq!(out[3], (1, 1, 4));
    }

    #[test]
    fn blit_skips_stride_padding() {
        // Two one-pixel rows, stride 8: a pixel then four bytes of padding.
        let src = [
            7u8, 0, 0, 0, 0xAA, 0xAA, 0xAA, 0xAA, 9, 0, 0, 0, 0xBB, 0xBB, 0xBB, 0xBB,
        ];
        let buf = buffer(0, 1, 2, 8, 16);
        let (out, n) = collect::<2>(
            &src,
            &buf,
            Destination {
                x: 0,
                y: 0,
                width: 1,
                height: 2,
            },
        );
        assert_eq!(n, 2);
        assert_eq!(out[0], (0, 0, 7));
        assert_eq!(out[1], (0, 1, 9));
    }

    #[test]
    fn blit_honours_the_pool_offset() {
        let mut src = [0u8; 32];
        src[8..12].copy_from_slice(&5u32.to_le_bytes());
        let buf = buffer(8, 1, 1, 4, 32);
        let (out, n) = collect::<1>(
            &src,
            &buf,
            Destination {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
        );
        assert_eq!(n, 1);
        assert_eq!(out[0], (0, 0, 5));
    }

    #[test]
    fn blit_clips_a_negative_origin() {
        // A 2x2 image at (-1,-1): only its bottom-right pixel lands, at (0,0).
        let src = [1u8, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0];
        let buf = buffer(0, 2, 2, 8, 16);
        let (out, n) = collect::<4>(
            &src,
            &buf,
            Destination {
                x: -1,
                y: -1,
                width: 4,
                height: 4,
            },
        );
        assert_eq!(n, 1);
        assert_eq!(out[0], (0, 0, 4));
    }

    #[test]
    fn blit_clips_at_the_far_edge() {
        // A 2x2 image at (1,1) in a 2x2 destination: only its top-left lands.
        let src = [1u8, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0];
        let buf = buffer(0, 2, 2, 8, 16);
        let (out, n) = collect::<4>(
            &src,
            &buf,
            Destination {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
        );
        assert_eq!(n, 1);
        assert_eq!(out[0], (1, 1, 1));
    }

    #[test]
    fn blit_places_the_buffer_at_a_positive_origin() {
        let src = [1u8, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0];
        let buf = buffer(0, 2, 2, 8, 16);
        let (out, n) = collect::<4>(
            &src,
            &buf,
            Destination {
                x: 10,
                y: 20,
                width: 64,
                height: 64,
            },
        );
        assert_eq!(n, 4);
        assert_eq!(out[0], (10, 20, 1));
        assert_eq!(out[1], (11, 20, 2));
        assert_eq!(out[2], (10, 21, 3));
        assert_eq!(out[3], (11, 21, 4));
    }

    #[test]
    fn blit_rejects_a_mapping_shorter_than_the_pool() {
        let src = [0u8; 8];
        let buf = buffer(0, 2, 2, 8, 16);
        let r = blit(
            &src,
            &buf,
            Destination {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            |_, _, _| {},
        );
        assert_eq!(r, Err(ShmError::Bounds));
    }

    #[test]
    fn damage_slice_carries_the_offset_and_the_clipped_origin() {
        // Rows of 8 bytes; the rect at (1,1) 2x2 covers bytes 12..16 and 20..24.
        let buf = buffer(0, 4, 4, 16, 64);
        let (sub, x, y) = buf
            .damage_slice(Rect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            })
            .unwrap();
        assert_eq!((x, y), (1, 1));
        assert_eq!(sub.offset, 16 + 4);
        assert_eq!((sub.width, sub.height, sub.stride), (2, 2, 16));
        assert_eq!(sub.validate(), Ok(()));
    }

    #[test]
    fn damage_slice_clips_to_the_buffer() {
        let buf = buffer(0, 4, 4, 16, 64);
        // A rect hanging off the right and bottom edges keeps the part inside.
        let (sub, x, y) = buf
            .damage_slice(Rect {
                x: 2,
                y: 2,
                w: 10,
                h: 10,
            })
            .unwrap();
        assert_eq!((x, y), (2, 2));
        assert_eq!((sub.width, sub.height), (2, 2));
        // And one that shares no pixel with the buffer answers nothing at all.
        assert!(
            buf.damage_slice(Rect {
                x: 9,
                y: 9,
                w: 4,
                h: 4
            })
            .is_none()
        );
        assert!(
            buf.damage_slice(Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0
            })
            .is_none()
        );
    }

    #[test]
    fn errors_map_to_the_wl_shm_codes() {
        assert_eq!(
            ShmError::Format.code(),
            protocol::WL_SHM_ERROR_INVALID_FORMAT
        );
        assert_eq!(
            ShmError::Stride.code(),
            protocol::WL_SHM_ERROR_INVALID_STRIDE
        );
        assert_eq!(
            ShmError::Bounds.code(),
            protocol::WL_SHM_ERROR_INVALID_STRIDE
        );
        assert_eq!(ShmError::Fd.code(), protocol::WL_SHM_ERROR_INVALID_FD);
    }
}
