//! A list of rows with one selected, scrolled so the selection stays visible.

use render::{Point, Rect};

use crate::event::{Event, Key, Response};
use crate::{Painter, Theme};

/// A list of text rows.
///
/// The rows are the caller's slice, not a copy: they are immutable, they outlive
/// the widget, and a fixed inline array would put a silent ceiling on how many
/// rows a list could show. The selection is the only state here; the scroll
/// window is derived from it.
pub struct List<'a> {
    items: &'a [&'a [u8]],
    selected: usize,
    bounds: Rect,
    inset: i32,
    row_height: i32,
    focused: bool,
}

impl<'a> List<'a> {
    pub fn new(items: &'a [&'a [u8]]) -> Self {
        Self {
            items,
            selected: 0,
            bounds: Rect::default(),
            inset: 0,
            row_height: 1,
            focused: false,
        }
    }

    pub fn items(&self) -> &'a [&'a [u8]] {
        self.items
    }

    /// The selected row, or `None` while there are no rows.
    pub fn selected(&self) -> Option<usize> {
        (!self.items.is_empty()).then_some(self.selected)
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
        self.row_height = theme.row_height.max(1);
    }

    /// Every row at the theme's row height, plus the border and padding.
    pub fn height(&self, theme: &Theme) -> i32 {
        self.items.len() as i32 * theme.row_height.max(1) + 2 * (theme.border + theme.padding)
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
            Event::Key { key, pressed: true } if self.focused => self.key(key),
            Event::Button {
                x,
                y,
                pressed: true,
            } => self.click(x, y),
            _ => Response::IGNORED,
        }
    }

    fn key(&mut self, key: Key) -> Response {
        if self.items.is_empty() {
            return Response::IGNORED;
        }
        let last = self.items.len() - 1;
        match key {
            Key::Up => self.select(self.selected.saturating_sub(1)),
            Key::Down => self.select((self.selected + 1).min(last)),
            Key::Home => self.select(0),
            Key::End => self.select(last),
            Key::Enter => Response::ACTIVATED,
            _ => Response::IGNORED,
        }
    }

    fn select(&mut self, to: usize) -> Response {
        if self.selected == to {
            return Response::IGNORED;
        }
        self.selected = to;
        Response::CHANGED
    }

    fn click(&mut self, x: i32, y: i32) -> Response {
        if !self.bounds.contains_point(Point::new(x, y)) {
            return Response::IGNORED;
        }
        let inner = self.inner();
        let row = (y - inner.y) / self.row_height;
        if row < 0 {
            return Response::IGNORED;
        }
        let index = self.top() + row as usize;
        if index >= self.items.len() {
            return Response::IGNORED;
        }
        self.select(index)
    }

    fn inner(&self) -> Rect {
        Rect::new(
            self.bounds.x + self.inset,
            self.bounds.y + self.inset,
            self.bounds.w - 2 * self.inset,
            self.bounds.h - 2 * self.inset,
        )
    }

    /// How many rows fit in the widget.
    fn rows(&self) -> usize {
        (self.inner().h / self.row_height).max(1) as usize
    }

    /// The first row shown: as late as the selection allows, and no later.
    fn top(&self) -> usize {
        self.selected.saturating_sub(self.rows() - 1)
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
        let top = self.top();
        let end = (top + self.rows()).min(self.items.len());
        let text_offset = (self.row_height - painter.text_height()).max(0) / 2;

        for (slot, item) in self.items[top..end].iter().enumerate() {
            let row = Rect::new(
                inner.x,
                inner.y + slot as i32 * self.row_height,
                inner.w,
                self.row_height,
            );
            if top + slot == self.selected {
                painter.fill(row, theme.accent);
            }
            // The glyphs come from the atlas, so their colour is the atlas's
            // white; the bar goes underneath rather than over the label.
            painter.text(item, Point::new(row.x + theme.padding, row.y + text_offset));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minix_std::font::FONT_8X16;
    use render::pass::{Element, Pass, TextureId};
    use render::text::Atlas;

    const THEME: Theme = Theme::DARK;
    const TEXTURE: TextureId = TextureId(0);
    const ITEMS: [&[u8]; 4] = [b"alpha", b"beta", b"gamma", b"delta"];

    fn list() -> List<'static> {
        let mut list = List::new(&ITEMS);
        list.layout(Rect::new(0, 0, 120, 200), &THEME);
        list.handle(Event::Focus { focused: true });
        list
    }

    fn press(key: Key) -> Event {
        Event::Key { key, pressed: true }
    }

    #[test]
    fn down_and_up_move_the_selection_within_the_rows() {
        let mut list = list();
        assert_eq!(list.selected(), Some(0));
        assert_eq!(list.handle(press(Key::Down)), Response::CHANGED);
        assert_eq!(list.selected(), Some(1));
        list.handle(press(Key::Down));
        list.handle(press(Key::Down));
        assert_eq!(list.selected(), Some(3));
        // At the last row there is nowhere to go, so nothing changed.
        assert_eq!(list.handle(press(Key::Down)), Response::IGNORED);
        list.handle(press(Key::Up));
        assert_eq!(list.selected(), Some(2));
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        let mut list = list();
        assert_eq!(list.handle(press(Key::End)), Response::CHANGED);
        assert_eq!(list.selected(), Some(3));
        assert_eq!(list.handle(press(Key::Home)), Response::CHANGED);
        assert_eq!(list.selected(), Some(0));
    }

    #[test]
    fn enter_activates_the_selected_row() {
        let mut list = list();
        assert_eq!(list.handle(press(Key::Enter)), Response::ACTIVATED);
        assert_eq!(list.handle(press(Key::Char(b'x'))), Response::IGNORED);
    }

    #[test]
    fn a_click_selects_the_row_under_it() {
        let mut list = list();
        let bounds = list.bounds();
        let y = bounds.y + THEME.border + THEME.padding + 2 * THEME.row_height + 1;
        assert_eq!(
            list.handle(Event::Button {
                x: 4,
                y,
                pressed: true
            }),
            Response::CHANGED
        );
        assert_eq!(list.selected(), Some(2));
    }

    #[test]
    fn a_click_below_the_rows_is_ignored() {
        let mut list = list();
        // The widget is taller than four rows, so the padding below them is
        // still inside the bounds but names no row.
        let y = list.bounds().y
            + THEME.border
            + THEME.padding
            + (ITEMS.len() as i32) * THEME.row_height;
        assert_eq!(
            list.handle(Event::Button {
                x: 4,
                y,
                pressed: true
            }),
            Response::IGNORED
        );
        assert_eq!(list.selected(), Some(0));
    }

    #[test]
    fn a_shorter_widget_scrolls_to_keep_the_selection_visible() {
        let mut list = List::new(&ITEMS);
        // Two rows fit.
        let height = 2 * THEME.row_height + 2 * (THEME.border + THEME.padding);
        list.layout(Rect::new(0, 0, 120, height), &THEME);
        list.handle(Event::Focus { focused: true });
        list.handle(press(Key::End));

        let atlas = Atlas::from_bitmap_font(&FONT_8X16);
        let mut pass = Pass::new(Rect::new(0, 0, 140, 80));
        let mut painter = Painter::new(&mut pass, &atlas, TEXTURE, THEME.scale);
        list.paint(&mut painter, &THEME);

        let glyphs = pass
            .elements()
            .iter()
            .filter(|element| matches!(element, Element::Texture { .. }))
            .count();
        // The last two rows are "gamma" and "delta".
        assert_eq!(glyphs, b"gamma".len() + b"delta".len());

        // Exactly one row carries the selection bar, and the focus ring is the
        // four rectangles of the frame.
        let bars = pass
            .elements()
            .iter()
            .filter(|element| {
                matches!(
                    element,
                    Element::Rect {
                        color,
                        ..
                    } if *color == THEME.accent
                )
            })
            .count();
        assert_eq!(bars, 5);
    }

    #[test]
    fn an_empty_list_ignores_keys() {
        let empty: [&[u8]; 0] = [];
        let mut list = List::new(&empty);
        list.layout(Rect::new(0, 0, 120, 40), &THEME);
        list.handle(Event::Focus { focused: true });
        assert_eq!(list.selected(), None);
        assert_eq!(list.handle(press(Key::Down)), Response::IGNORED);
        assert_eq!(list.handle(press(Key::Enter)), Response::IGNORED);
    }
}
