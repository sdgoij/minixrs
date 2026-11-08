//! A software backend: draw a pass into an `XRGB8888` buffer.
//!
//! The compositor is a static binary in the initramfs, and Mesa is only reachable
//! through the loader and the DSOs on the disk image, so the server cannot take
//! the GL backend without a boot-order change. This backend gives it the same
//! pass list and the same damage rules over plain memory, which is enough for
//! the window-server milestone: two surfaces, correct overlap, no corruption.
//!
//! Textures are `XRGB8888` and therefore opaque. The high byte of a client's
//! `wl_shm` buffer is not alpha, and treating it as such would make every window
//! transparent; the alpha-capable path belongs to the GL backend and the glyph
//! atlas.
//!
//! Nothing here touches a file descriptor or a device, so the whole backend is
//! host-tested against pixel buffers.

use alloc::vec;
use alloc::vec::Vec;

use crate::backend::Backend;
use crate::geom::Rect;
use crate::pass::{Color, Element, Pass, TextureId};

/// Why a software render failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftError {
    /// The pass was built for a different output size.
    OutputMismatch,
    /// The buffer cannot hold a row of the output at the given stride.
    BufferTooSmall,
    /// Texture data is shorter than the geometry it was given.
    TextureSize,
    /// A texture id does not name a texture this renderer owns.
    Texture,
}

impl core::fmt::Display for SoftError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutputMismatch => f.write_str("pass output size differs from the renderer's"),
            Self::BufferTooSmall => f.write_str("framebuffer is too small for the output"),
            Self::TextureSize => f.write_str("texture data is shorter than its geometry"),
            Self::Texture => f.write_str("element names a texture this renderer does not own"),
        }
    }
}

struct SoftTexture {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

/// A renderer over a caller-owned `XRGB8888` buffer.
///
/// The buffer's first pixel is the output's top-left: `stride` is the row length
/// in pixels, and a row of `output.w` pixels starts there.
pub struct Software<'a> {
    output: Rect,
    stride: usize,
    pixels: &'a mut [u32],
    textures: Vec<SoftTexture>,
}

impl<'a> Software<'a> {
    pub fn new(output: Rect, stride: usize, pixels: &'a mut [u32]) -> Result<Self, SoftError> {
        if output.is_empty() || stride < output.w as usize {
            return Err(SoftError::BufferTooSmall);
        }
        let rows = output.h as usize;
        let needed = (rows - 1) * stride + output.w as usize;
        if pixels.len() < needed {
            return Err(SoftError::BufferTooSmall);
        }
        Ok(Self {
            output,
            stride,
            pixels,
            textures: Vec::new(),
        })
    }

    pub fn output(&self) -> Rect {
        self.output
    }

    /// Copy an `XRGB8888` image and return the id elements name it by.
    pub fn create_texture(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[u32],
    ) -> Result<TextureId, SoftError> {
        if width == 0 || height == 0 {
            return Err(SoftError::TextureSize);
        }
        let required = (width as usize)
            .checked_mul(height as usize)
            .ok_or(SoftError::TextureSize)?;
        if pixels.len() < required {
            return Err(SoftError::TextureSize);
        }

        self.textures.push(SoftTexture {
            width,
            height,
            pixels: pixels[..required].to_vec(),
        });
        Ok(TextureId((self.textures.len() - 1) as u32))
    }

    /// Copy an `XRGB8888` image out of a byte buffer, honoring a row stride and a
    /// start offset. This is the shape a `wl_shm` pool has: little-endian, four
    /// bytes per pixel, and possibly a buffer that starts partway down a pool.
    pub fn create_texture_from_bytes(
        &mut self,
        width: u32,
        height: u32,
        stride: usize,
        offset: usize,
        bytes: &[u8],
    ) -> Result<TextureId, SoftError> {
        if width == 0 || height == 0 {
            return Err(SoftError::TextureSize);
        }
        let width = width as usize;
        let height = height as usize;
        if stride < width * 4 {
            return Err(SoftError::TextureSize);
        }

        let mut pixels = vec![0u32; width * height];
        for y in 0..height {
            let row = offset + y * stride;
            for x in 0..width {
                let at = row + x * 4;
                let Some(texel) = bytes.get(at..at + 4) else {
                    return Err(SoftError::TextureSize);
                };
                pixels[y * width + x] =
                    u32::from_le_bytes([texel[0], texel[1], texel[2], texel[3]]);
            }
        }

        self.textures.push(SoftTexture {
            width: width as u32,
            height: height as u32,
            pixels,
        });
        Ok(TextureId((self.textures.len() - 1) as u32))
    }

    /// Draw `pass` into the buffer. Pixels outside the pass's damage are not
    /// written.
    pub fn render(&mut self, pass: &Pass) -> Result<(), SoftError> {
        if pass.output() != self.output {
            return Err(SoftError::OutputMismatch);
        }
        pass.submit(self)
    }

    fn fill(&mut self, area: Rect, color: Color) {
        if color.a == 0 {
            return;
        }
        for y in area.y..area.bottom() {
            let row = y as usize * self.stride;
            for x in area.x..area.right() {
                let index = row + x as usize;
                self.pixels[index] = if color.is_opaque() {
                    pack(color)
                } else {
                    blend(self.pixels[index], color)
                };
            }
        }
    }

    fn blit(&mut self, rect: Rect, source: Rect, texture: usize, area: Rect) {
        if rect.w <= 0 || rect.h <= 0 || source.w <= 0 || source.h <= 0 {
            return;
        }
        let Some(object) = self.textures.get(texture) else {
            return;
        };
        let texture_width = object.width as i32;
        let texture_height = object.height as i32;

        for y in area.y..area.bottom() {
            let v = source.y + (y - rect.y) * source.h / rect.h;
            let v = v.clamp(0, texture_height - 1) as usize;
            let row = v * object.width as usize;
            let output_row = y as usize * self.stride;
            for x in area.x..area.right() {
                let u = source.x + (x - rect.x) * source.w / rect.w;
                let u = u.clamp(0, texture_width - 1) as usize;
                self.pixels[output_row + x as usize] = object.pixels[row + u];
            }
        }
    }
}

impl Backend for Software<'_> {
    type Error = SoftError;

    fn draw(&mut self, element: &Element, scissor: Rect) -> Result<(), Self::Error> {
        let Some(area) = scissor.intersection(self.output) else {
            return Ok(());
        };

        match *element {
            Element::Clear { color } => self.fill(area, color),
            Element::Rect { rect, color } => {
                if let Some(clipped) = rect.intersection(area) {
                    self.fill(clipped, color);
                }
            }
            Element::Texture {
                rect,
                source,
                texture,
                ..
            } => {
                if rect.intersection(area).is_none() {
                    return Ok(());
                }
                if texture.0 as usize >= self.textures.len() {
                    return Err(SoftError::Texture);
                }
                self.blit(rect, source, texture.0 as usize, area);
            }
        }
        Ok(())
    }
}

fn pack(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

fn blend(destination: u32, color: Color) -> u32 {
    let alpha = u32::from(color.a);
    let inverse = 255 - alpha;
    let mix =
        |source: u8, channel: u32| (u32::from(source) * alpha + channel * inverse + 127) / 255;
    let r = mix(color.r, (destination >> 16) & 0xff);
    let g = mix(color.g, (destination >> 8) & 0xff);
    let b = mix(color.b, destination & 0xff);
    (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::Region;

    const OUTPUT: Rect = Rect::new(0, 0, 8, 8);
    const BLACK: u32 = 0x0000_0000;
    const RED: u32 = 0x00FF_0000;
    const GREEN: u32 = 0x0000_FF00;
    const BLUE: u32 = 0x0000_00FF;

    fn buffer() -> Vec<u32> {
        vec![BLACK; 64]
    }

    fn renderer(buffer: &mut [u32]) -> Software<'_> {
        Software::new(OUTPUT, 8, buffer).unwrap()
    }

    fn pixel(buffer: &[u32], x: usize, y: usize) -> u32 {
        buffer[y * 8 + x]
    }

    #[test]
    fn clear_fills_the_damage_rect() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let mut pass = Pass::with_damage(OUTPUT, Region::from_rect(Rect::new(2, 2, 3, 3)));
        pass.add(Element::Clear {
            color: Color::rgb(0xFF, 0, 0),
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 2, 2), RED);
        assert_eq!(pixel(&buffer, 4, 4), RED);
        assert_eq!(pixel(&buffer, 1, 2), BLACK);
        assert_eq!(pixel(&buffer, 5, 4), BLACK);
    }

    #[test]
    fn a_rect_fills_only_its_pixels() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Rect {
            rect: Rect::new(1, 1, 2, 2),
            color: Color::rgb(0, 0xFF, 0),
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 1, 1), GREEN);
        assert_eq!(pixel(&buffer, 2, 2), GREEN);
        assert_eq!(pixel(&buffer, 0, 0), BLACK);
        assert_eq!(pixel(&buffer, 3, 3), BLACK);
    }

    #[test]
    fn a_rect_is_clipped_to_the_output() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Rect {
            rect: Rect::new(6, 6, 10, 10),
            color: Color::rgb(0, 0, 0xFF),
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 7, 7), BLUE);
        assert_eq!(pixel(&buffer, 6, 6), BLUE);
    }

    #[test]
    fn a_texture_copies_its_pixels() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let texture = software
            .create_texture(2, 1, &[RED, BLUE])
            .expect("the texture fits");

        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Texture {
            rect: Rect::new(3, 4, 2, 1),
            source: Rect::new(0, 0, 2, 1),
            texture,
            opaque: true,
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 3, 4), RED);
        assert_eq!(pixel(&buffer, 4, 4), BLUE);
        assert_eq!(pixel(&buffer, 2, 4), BLACK);
    }

    #[test]
    fn a_texture_source_offset_selects_one_texel() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let texture = software
            .create_texture(2, 2, &[RED, GREEN, BLUE, BLACK])
            .expect("the texture fits");

        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Texture {
            rect: Rect::new(0, 0, 1, 1),
            source: Rect::new(0, 1, 1, 1),
            texture,
            opaque: true,
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 0, 0), BLUE);
        assert_eq!(pixel(&buffer, 1, 0), BLACK);
    }

    #[test]
    fn damage_leaves_the_rest_untouched() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let mut pass = Pass::with_damage(OUTPUT, Region::from_rect(Rect::new(0, 0, 4, 4)));
        pass.add(Element::Clear {
            color: Color::rgb(0xFF, 0, 0),
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 3, 3), RED);
        assert_eq!(pixel(&buffer, 4, 4), BLACK);
    }

    #[test]
    fn an_opaque_top_rect_wins_the_overlap() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Rect {
            rect: Rect::new(0, 0, 4, 4),
            color: Color::rgb(0xFF, 0, 0),
        });
        pass.add(Element::Rect {
            rect: Rect::new(2, 2, 4, 4),
            color: Color::rgb(0, 0, 0xFF),
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 1, 1), RED);
        assert_eq!(pixel(&buffer, 3, 3), BLUE);
    }

    #[test]
    fn a_translucent_rect_blends() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Rect {
            rect: Rect::new(0, 0, 2, 2),
            color: Color::rgba(0xFF, 0xFF, 0xFF, 128),
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 0, 0), 0x0080_8080);
    }

    #[test]
    fn an_unknown_texture_is_an_error() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Texture {
            rect: Rect::new(0, 0, 1, 1),
            source: Rect::new(0, 0, 1, 1),
            texture: TextureId(7),
            opaque: true,
        });

        assert_eq!(software.render(&pass), Err(SoftError::Texture));
    }

    #[test]
    fn a_buffer_too_small_is_refused() {
        let mut short = vec![BLACK; 7];
        assert_eq!(
            Software::new(OUTPUT, 8, &mut short).err(),
            Some(SoftError::BufferTooSmall)
        );
        assert_eq!(
            Software::new(OUTPUT, 7, &mut short).err(),
            Some(SoftError::BufferTooSmall)
        );
    }

    #[test]
    fn a_pass_for_another_output_is_refused() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        let pass = Pass::new(Rect::new(0, 0, 4, 4));
        assert_eq!(software.render(&pass), Err(SoftError::OutputMismatch));
    }

    #[test]
    fn a_texture_shorter_than_its_geometry_is_refused() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        assert_eq!(
            software.create_texture(2, 2, &[RED]).err(),
            Some(SoftError::TextureSize)
        );
    }

    #[test]
    fn a_byte_texture_honors_stride_and_offset() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);

        // Two rows of two pixels with a 12-byte stride and a 4-byte starter
        // offset: the image is at bytes 4..16 and 16..28.
        let mut bytes = vec![0u8; 32];
        bytes[4..8].copy_from_slice(&RED.to_le_bytes());
        bytes[8..12].copy_from_slice(&GREEN.to_le_bytes());
        bytes[16..20].copy_from_slice(&BLUE.to_le_bytes());
        bytes[20..24].copy_from_slice(&BLACK.to_le_bytes());

        let texture = software
            .create_texture_from_bytes(2, 2, 12, 4, &bytes)
            .expect("the image fits");
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Texture {
            rect: Rect::new(0, 0, 2, 2),
            source: Rect::new(0, 0, 2, 2),
            texture,
            opaque: true,
        });

        software.render(&pass).unwrap();
        assert_eq!(pixel(&buffer, 0, 0), RED);
        assert_eq!(pixel(&buffer, 1, 0), GREEN);
        assert_eq!(pixel(&buffer, 0, 1), BLUE);
        assert_eq!(pixel(&buffer, 1, 1), BLACK);
    }

    #[test]
    fn a_byte_texture_that_runs_off_the_end_is_refused() {
        let mut buffer = buffer();
        let mut software = renderer(&mut buffer);
        assert_eq!(
            software
                .create_texture_from_bytes(2, 2, 8, 0, &[0u8; 12])
                .err(),
            Some(SoftError::TextureSize)
        );
    }
}
