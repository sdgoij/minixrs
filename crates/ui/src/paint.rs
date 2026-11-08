//! Turning widgets into pass elements.

use render::pass::{Color, Element, Pass, TextureId};
use render::text::{Atlas, FONT_HEIGHT, layout_line, line_width};
use render::{Point, Rect};

/// Where a widget's elements go.
///
/// The caller owns the frame's [`Pass`] and the atlas texture: the GL and
/// software backends upload it differently, and the painter only names it. Text
/// is emitted as one quad per glyph, so a widget never has to know how a backend
/// draws a line.
pub struct Painter<'a> {
    pass: &'a mut Pass,
    atlas: &'a Atlas,
    texture: TextureId,
    scale: i32,
}

impl<'a> Painter<'a> {
    pub fn new(pass: &'a mut Pass, atlas: &'a Atlas, texture: TextureId, scale: i32) -> Self {
        Self {
            pass,
            atlas,
            texture,
            scale: scale.max(1),
        }
    }

    pub fn scale(&self) -> i32 {
        self.scale
    }

    /// The height of one line of text.
    pub fn text_height(&self) -> i32 {
        FONT_HEIGHT as i32 * self.scale
    }

    /// The width `bytes` occupies, trailing advance included.
    pub fn measure(&self, bytes: &[u8]) -> i32 {
        line_width(bytes, self.scale)
    }

    /// Fill the whole output; normally the first thing a frame does.
    pub fn clear(&mut self, color: Color) {
        self.pass.add(Element::Clear { color });
    }

    pub fn fill(&mut self, rect: Rect, color: Color) {
        if !rect.is_empty() {
            self.pass.add(Element::Rect { rect, color });
        }
    }

    /// One line of text with its top-left at `origin`.
    ///
    /// Glyphs are translucent, so every quad says `opaque: false`: the pixels
    /// between the strokes must let the widget's surface show through. The
    /// colour tints the atlas, which is white, so it is the text colour.
    pub fn text(&mut self, bytes: &[u8], origin: Point, color: Color) {
        for glyph in layout_line(self.atlas, bytes, origin, self.scale) {
            self.pass.add(Element::Texture {
                rect: glyph.rect,
                source: glyph.source,
                texture: self.texture,
                color,
                opaque: false,
            });
        }
    }

    /// A ring `width` thick just inside `rect`, as four rectangles.
    ///
    /// Four rather than one because the inside must keep whatever the widget
    /// painted there: a frame is an edge, not a background.
    pub fn frame(&mut self, rect: Rect, width: i32, color: Color) {
        if rect.is_empty() || width <= 0 {
            return;
        }
        let t = width.min(rect.w).min(rect.h);
        self.fill(Rect::new(rect.x, rect.y, rect.w, t), color);
        self.fill(Rect::new(rect.x, rect.bottom() - t, rect.w, t), color);
        if rect.h > 2 * t {
            self.fill(Rect::new(rect.x, rect.y + t, t, rect.h - 2 * t), color);
            self.fill(
                Rect::new(rect.right() - t, rect.y + t, t, rect.h - 2 * t),
                color,
            );
        }
    }
}
