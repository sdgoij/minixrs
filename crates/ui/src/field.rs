//! A single-line text field: a fixed buffer, a caret and a horizontal scroll.

use render::{Point, Rect};

use crate::event::{Event, Key, Response};
use crate::{Painter, Theme};

/// How many bytes a field holds.
///
/// Fixed rather than a `String`: a window's widgets then need no allocator, and
/// a field that grew on its own would make the layout depend on what was typed.
pub const CAPACITY: usize = 64;

pub struct Field<'a> {
    buffer: [u8; CAPACITY],
    len: usize,
    cursor: usize,
    placeholder: &'a [u8],
    bounds: Rect,
    /// Border plus padding, kept from layout so a click can find the text origin
    /// without the theme being passed through the event.
    inset: i32,
    /// The text scale, for the same reason.
    scale: i32,
    focused: bool,
}

impl Default for Field<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Field<'a> {
    pub fn new() -> Self {
        Self {
            buffer: [0; CAPACITY],
            len: 0,
            cursor: 0,
            placeholder: b"",
            bounds: Rect::default(),
            inset: 0,
            scale: 1,
            focused: false,
        }
    }

    /// Text drawn while the field is empty.
    pub fn placeholder(mut self, text: &'a [u8]) -> Self {
        self.placeholder = text;
        self
    }

    /// The bytes typed so far.
    pub fn text(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn layout(&mut self, bounds: Rect, theme: &Theme) {
        self.bounds = bounds;
        self.inset = theme.border + theme.padding;
        self.scale = theme.scale.max(1);
    }

    pub fn height(theme: &Theme) -> i32 {
        theme.row_height
    }

    pub fn handle(&mut self, event: Event) -> Response {
        match event {
            Event::Focus { focused } => {
                if self.focused == focused {
                    return Response::IGNORED;
                }
                self.focused = focused;
                Response::CHANGED
            }
            Event::Button {
                x,
                y,
                pressed: true,
            } if self.bounds.contains_point(Point::new(x, y)) => {
                self.place_caret(x);
                Response::CHANGED
            }
            Event::Key { key, pressed: true } if self.focused => self.key(key),
            _ => Response::IGNORED,
        }
    }

    fn key(&mut self, key: Key) -> Response {
        match key {
            Key::Char(byte) if byte.is_ascii_graphic() || byte == b' ' => {
                if self.len == CAPACITY {
                    return Response::IGNORED;
                }
                self.buffer
                    .copy_within(self.cursor..self.len, self.cursor + 1);
                self.buffer[self.cursor] = byte;
                self.len += 1;
                self.cursor += 1;
                Response::CHANGED
            }
            Key::Backspace => {
                if self.cursor == 0 {
                    return Response::IGNORED;
                }
                self.buffer
                    .copy_within(self.cursor..self.len, self.cursor - 1);
                self.len -= 1;
                self.cursor -= 1;
                Response::CHANGED
            }
            Key::Left => self.move_caret(self.cursor.saturating_sub(1)),
            Key::Right => self.move_caret((self.cursor + 1).min(self.len)),
            Key::Home => self.move_caret(0),
            Key::End => self.move_caret(self.len),
            _ => Response::IGNORED,
        }
    }

    fn move_caret(&mut self, to: usize) -> Response {
        if self.cursor == to {
            return Response::IGNORED;
        }
        self.cursor = to;
        Response::CHANGED
    }

    /// Put the caret at the byte column the click landed on.
    fn place_caret(&mut self, x: i32) {
        let step = cell(self.scale);
        let column = (x - self.bounds.x - self.inset).max(0) / step;
        self.cursor = (column as usize).min(self.len);
    }

    fn inner(&self) -> Rect {
        Rect::new(
            self.bounds.x + self.inset,
            self.bounds.y + self.inset,
            self.bounds.w - 2 * self.inset,
            self.bounds.h - 2 * self.inset,
        )
    }

    pub fn paint(&self, painter: &mut Painter<'_>, theme: &Theme) {
        painter.fill(self.bounds, theme.surface);
        painter.frame(
            self.bounds,
            theme.border,
            if self.focused {
                theme.accent
            } else {
                theme.line
            },
        );

        let inner = self.inner();
        if inner.w <= 0 || inner.h <= 0 {
            return;
        }
        let step = cell(painter.scale());
        let columns = (inner.w / step).max(1) as usize;
        // The caret is always visible: the visible bytes start as late as the
        // caret minus one column's worth of text.
        let first = self.cursor.saturating_sub(columns - 1);
        let end = (first + columns).min(self.len);
        let origin = Point::new(
            inner.x,
            inner.y + (inner.h - painter.text_height()).max(0) / 2,
        );

        if self.len == 0 && !self.placeholder.is_empty() {
            painter.text(self.placeholder, origin, theme.ink_dim);
        } else {
            painter.text(&self.buffer[first..end], origin, theme.ink);
        }

        if self.focused {
            let caret = origin
                .x
                .saturating_add((self.cursor - first) as i32 * step)
                .min(inner.right() - theme.border.max(1));
            painter.fill(
                Rect::new(caret, origin.y, theme.border.max(1), painter.text_height()),
                theme.accent,
            );
        }
    }
}

/// The advance of one character cell in this scale.
fn cell(scale: i32) -> i32 {
    render::text::FONT_WIDTH as i32 * scale.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minix_std::font::FONT_8X16;
    use render::pass::{Element, Pass, TextureId};
    use render::text::Atlas;

    const AREA: Rect = Rect::new(0, 0, 96, 24);
    const TEXTURE: TextureId = TextureId(0);
    const THEME: Theme = Theme::DARK;

    fn field() -> Field<'static> {
        let mut field = Field::new();
        field.layout(AREA, &THEME);
        field.handle(Event::Focus { focused: true });
        field
    }

    fn typed(field: &mut Field<'static>, bytes: &[u8]) {
        for byte in bytes {
            field.handle(Event::Key {
                key: Key::Char(*byte),
                pressed: true,
            });
        }
    }

    fn press(key: Key) -> Event {
        Event::Key { key, pressed: true }
    }

    /// Whether any glyph quad carries `color` as its tint.
    fn has_tint(pass: &Pass, color: render::Color) -> bool {
        pass.elements().iter().any(
            |element| matches!(element, Element::Texture { color: tint, .. } if *tint == color),
        )
    }

    #[test]
    fn typing_appends_at_the_cursor() {
        let mut field = field();
        typed(&mut field, b"hi");
        assert_eq!(field.text(), b"hi");
        assert_eq!(field.cursor(), 2);
        assert_eq!(field.len(), 2);
    }

    #[test]
    fn home_and_end_move_the_caret_and_typing_inserts_there() {
        let mut field = field();
        typed(&mut field, b"bc");
        assert_eq!(field.handle(press(Key::Home)), Response::CHANGED);
        assert_eq!(field.cursor(), 0);
        typed(&mut field, b"a");
        assert_eq!(field.text(), b"abc");
        assert_eq!(field.cursor(), 1);

        field.handle(press(Key::End));
        typed(&mut field, b"d");
        assert_eq!(field.text(), b"abcd");
    }

    #[test]
    fn backspace_deletes_the_byte_before_the_caret() {
        let mut field = field();
        typed(&mut field, b"ab");
        field.handle(press(Key::Left));
        assert_eq!(field.handle(press(Key::Backspace)), Response::CHANGED);
        assert_eq!(field.text(), b"b");
        assert_eq!(field.cursor(), 0);

        assert_eq!(field.handle(press(Key::Backspace)), Response::IGNORED);
    }

    #[test]
    fn an_unprintable_byte_is_refused() {
        let mut field = field();
        assert_eq!(
            field.handle(Event::Key {
                key: Key::Char(0x1F),
                pressed: true
            }),
            Response::IGNORED
        );
        assert_eq!(field.text(), b"");
    }

    #[test]
    fn a_full_field_refuses_more() {
        let mut field = field();
        for _ in 0..CAPACITY {
            typed(&mut field, b"x");
        }
        assert_eq!(field.len(), CAPACITY);

        let before = field.text().len();
        assert_eq!(
            field.handle(Event::Key {
                key: Key::Char(b'y'),
                pressed: true
            }),
            Response::IGNORED
        );
        assert_eq!(field.text().len(), before);
        assert_eq!(field.text()[0], b'x');
    }

    #[test]
    fn an_unfocused_field_ignores_keys() {
        let mut field = Field::new();
        field.layout(AREA, &THEME);
        assert_eq!(field.handle(press(Key::Char(b'a'))), Response::IGNORED);
        assert_eq!(field.text(), b"");
    }

    #[test]
    fn a_click_places_the_caret_at_its_column() {
        let mut field = field();
        typed(&mut field, b"abcd");

        // The text starts one inset from the left edge; the third cell begins two
        // cells into it.
        let step = cell(THEME.scale);
        let inset = THEME.border + THEME.padding;
        let x = AREA.x + inset + 2 * step + 1;
        assert_eq!(
            field.handle(Event::Button {
                x,
                y: AREA.y + 4,
                pressed: true
            }),
            Response::CHANGED
        );
        assert_eq!(field.cursor(), 2);

        typed(&mut field, b"X");
        assert_eq!(field.text(), b"abXcd");
    }

    #[test]
    fn a_long_value_scrolls_so_the_caret_stays_visible() {
        let mut field = field();
        let inset = THEME.border + THEME.padding;
        let columns = (AREA.w - 2 * inset) / cell(THEME.scale);
        for _ in 0..(columns as usize + 4) {
            typed(&mut field, b"z");
        }

        let atlas = Atlas::from_bitmap_font(&FONT_8X16);
        let mut pass = Pass::new(Rect::new(0, 0, 120, 40));
        let mut painter = Painter::new(&mut pass, &atlas, TEXTURE, THEME.scale);
        field.paint(&mut painter, &THEME);

        // Only a window's worth of glyphs is drawn, not the whole value: the
        // caret takes the last column, so one byte fewer than the columns fit.
        let glyphs = pass
            .elements()
            .iter()
            .filter(|element| matches!(element, Element::Texture { .. }))
            .count();
        assert_eq!(glyphs, columns as usize - 1);

        // And the caret is still inside the field.
        let caret = pass
            .elements()
            .iter()
            .rev()
            .find_map(|element| match element {
                Element::Rect { rect, color } if *color == THEME.accent => Some(*rect),
                _ => None,
            })
            .expect("a caret");
        assert!(caret.x >= AREA.x + inset);
        assert!(caret.right() <= AREA.right() - inset);
    }

    #[test]
    fn the_placeholder_shows_only_while_empty() {
        let mut field = Field::new().placeholder(b"name");
        field.layout(AREA, &THEME);

        let atlas = Atlas::from_bitmap_font(&FONT_8X16);
        let glyphs = |pass: &Pass| {
            pass.elements()
                .iter()
                .filter(|element| matches!(element, Element::Texture { .. }))
                .count()
        };

        let mut pass = Pass::new(Rect::new(0, 0, 120, 40));
        let mut painter = Painter::new(&mut pass, &atlas, TEXTURE, THEME.scale);
        field.paint(&mut painter, &THEME);
        let placeholder = glyphs(&pass);
        assert_eq!(placeholder, 4);
        assert!(has_tint(&pass, THEME.ink_dim));

        field.handle(Event::Focus { focused: true });
        typed(&mut field, b"x");
        let mut pass = Pass::new(Rect::new(0, 0, 120, 40));
        let mut painter = Painter::new(&mut pass, &atlas, TEXTURE, THEME.scale);
        field.paint(&mut painter, &THEME);
        assert_eq!(glyphs(&pass), 1);
        assert!(has_tint(&pass, THEME.ink));
    }
}
