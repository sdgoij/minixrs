//! A bitmap glyph atlas and one-line layout.
//!
//! The font is the public-domain VGA 8x16 table in `minix_std::font::FONT_8X16`,
//! the same one the window server and `fbterm` draw with. It is a placeholder for
//! the rasterizer decision in `.notes/gui.md` §8: a line of text at this
//! milestone is a row of textured quads, and replacing the source later does not
//! touch the pass list or the GL backend.
//!
//! The atlas is RGBA with white RGB and coverage in alpha, so the same textured
//! program that draws glyphs will draw a window buffer at the next milestone.
//! Nothing here touches GL; the atlas is built and laid out on the host and
//! uploaded once.

use alloc::vec;
use alloc::vec::Vec;

use crate::geom::{Point, Rect};

/// Cells per atlas row. Sixteen keeps the 95 printable ASCII glyphs in six rows.
const COLUMNS: u32 = 16;

pub const FONT_WIDTH: u32 = 8;
pub const FONT_HEIGHT: u32 = 16;
const FIRST_CHAR: u8 = 0x20;
const LAST_CHAR: u8 = 0x7E;

/// Printable ASCII glyphs as an RGBA atlas.
pub struct Atlas {
    width: u32,
    height: u32,
    glyphs: usize,
    pixels: Vec<u8>,
}

impl Atlas {
    /// Build from `[row; 16]` bitmaps, MSB of row 0 is the top-left pixel.
    pub fn from_bitmap_font(glyphs: &[[u8; 16]]) -> Self {
        let rows = (glyphs.len() as u32).div_ceil(COLUMNS).max(1);
        let width = COLUMNS * FONT_WIDTH;
        let height = rows * FONT_HEIGHT;
        let mut pixels = vec![0u8; (width * height * 4) as usize];

        for (index, glyph) in glyphs.iter().enumerate() {
            let cell_x = (index as u32 % COLUMNS) * FONT_WIDTH;
            let cell_y = (index as u32 / COLUMNS) * FONT_HEIGHT;
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..FONT_WIDTH {
                    if bits & (0x80 >> column) == 0 {
                        continue;
                    }
                    let x = cell_x + column;
                    let y = cell_y + row as u32;
                    let offset = ((y * width + x) * 4) as usize;
                    pixels[offset] = 255;
                    pixels[offset + 1] = 255;
                    pixels[offset + 2] = 255;
                    pixels[offset + 3] = 255;
                }
            }
        }

        Self {
            width,
            height,
            glyphs: glyphs.len(),
            pixels,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// RGBA8 pixels in row-major order, top row first.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The cell for a printable byte, in texture pixels.
    pub fn glyph_source(&self, byte: u8) -> Option<Rect> {
        if !(FIRST_CHAR..=LAST_CHAR).contains(&byte) {
            return None;
        }
        let index = (byte - FIRST_CHAR) as u32;
        if index as usize >= self.glyphs {
            return None;
        }

        Some(Rect::new(
            (index % COLUMNS) as i32 * FONT_WIDTH as i32,
            (index / COLUMNS) as i32 * FONT_HEIGHT as i32,
            FONT_WIDTH as i32,
            FONT_HEIGHT as i32,
        ))
    }
}

/// One glyph placed on the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub rect: Rect,
    pub source: Rect,
}

/// The width a line occupies, in logical pixels, spaces included.
pub fn line_width(text: &[u8], scale: i32) -> i32 {
    text.len() as i32 * FONT_WIDTH as i32 * scale.max(1)
}

/// Lay out one line starting at `origin`, one cell per byte.
///
/// A space and any byte the atlas has no glyph for still advance the cursor but
/// produce no quad, so a line of mixed text does not spawn transparent draws.
pub fn layout_line(atlas: &Atlas, text: &[u8], origin: Point, scale: i32) -> Vec<Glyph> {
    let scale = scale.max(1);
    let advance = FONT_WIDTH as i32 * scale;
    let mut x = origin.x;
    let mut glyphs = Vec::new();

    for byte in text {
        if *byte != b' '
            && let Some(source) = atlas.glyph_source(*byte)
        {
            glyphs.push(Glyph {
                rect: Rect::new(
                    x,
                    origin.y,
                    FONT_WIDTH as i32 * scale,
                    FONT_HEIGHT as i32 * scale,
                ),
                source,
            });
        }
        x += advance;
    }

    glyphs
}

#[cfg(test)]
mod tests {
    use super::*;
    use minix_std::font::FONT_8X16;

    fn atlas() -> Atlas {
        Atlas::from_bitmap_font(&FONT_8X16)
    }

    #[test]
    fn the_atlas_holds_the_printable_range_in_six_rows() {
        let atlas = atlas();
        assert_eq!(atlas.width(), 128);
        assert_eq!(atlas.height(), 96);
        assert_eq!(atlas.pixels().len(), (128 * 96 * 4) as usize);
    }

    #[test]
    fn glyph_cells_carry_coverage_only_where_the_bitmap_does() {
        let atlas = atlas();
        let space = atlas.glyph_source(b' ').unwrap();
        let a = atlas.glyph_source(b'A').unwrap();

        let coverage = |rect: Rect, x: i32, y: i32| {
            let offset = (((rect.y + y) as u32 * atlas.width() + (rect.x + x) as u32) * 4) as usize;
            atlas.pixels()[offset + 3]
        };

        assert_eq!(coverage(space, 0, 0), 0);
        assert_eq!(coverage(space, 7, 15), 0);

        // 'A' has ink in the middle of its cell and none in the first row.
        let inked = (0..FONT_HEIGHT as i32)
            .flat_map(|y| (0..FONT_WIDTH as i32).map(move |x| (x, y)))
            .filter(|(x, y)| coverage(a, *x, *y) != 0)
            .count();
        assert!(inked > 0);
    }

    #[test]
    fn glyph_sources_are_one_cell_and_bounded() {
        assert_eq!(atlas().glyph_source(b' '), Some(Rect::new(0, 0, 8, 16)));
        assert_eq!(
            atlas().glyph_source(b'~'),
            Some(Rect::new(14 * 8, 5 * 16, 8, 16))
        );
        assert_eq!(atlas().glyph_source(0x1F), None);
        assert_eq!(atlas().glyph_source(0x7F), None);
    }

    #[test]
    fn layout_advances_one_cell_per_byte() {
        let atlas = atlas();
        let glyphs = layout_line(&atlas, b"AB", Point::new(10, 20), 1);

        assert_eq!(glyphs.len(), 2);
        assert_eq!(glyphs[0].rect, Rect::new(10, 20, 8, 16));
        assert_eq!(glyphs[1].rect, Rect::new(18, 20, 8, 16));
        assert_eq!(glyphs[0].source, atlas.glyph_source(b'A').unwrap());
        assert_eq!(glyphs[1].source, atlas.glyph_source(b'B').unwrap());
    }

    #[test]
    fn spaces_advance_without_a_quad() {
        let glyphs = layout_line(&atlas(), b"A B", Point::new(10, 20), 1);
        assert_eq!(glyphs.len(), 2);
        assert_eq!(glyphs[0].rect.x, 10);
        assert_eq!(glyphs[1].rect.x, 26);
    }

    #[test]
    fn scale_multiplies_the_cell_and_the_advance() {
        let glyphs = layout_line(&atlas(), b"AB", Point::new(0, 0), 2);
        assert_eq!(glyphs[0].rect, Rect::new(0, 0, 16, 32));
        assert_eq!(glyphs[1].rect, Rect::new(16, 0, 16, 32));
        assert_eq!(line_width(b"AB", 2), 32);
    }

    #[test]
    fn bytes_outside_ascii_advance_without_a_quad() {
        let glyphs = layout_line(&atlas(), &[0x1F, b'A', 0x7F], Point::new(0, 0), 1);
        assert_eq!(glyphs.len(), 1);
        assert_eq!(glyphs[0].rect.x, 8);
    }
}
