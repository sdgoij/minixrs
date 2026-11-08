//! A window's widgets, which of them has the keyboard, and where a click went.
//!
//! Focus movement lives here rather than in each widget because Tab has to know
//! the order the widgets were added in, and a pointer press has to hand the
//! release back to the widget the press landed on even if the pointer has left
//! it. Both are properties of the collection, not of one widget.

use render::Rect;
use render::pass::{Pass, TextureId};
use render::text::Atlas;

use crate::button::Button;
use crate::event::{Event, Key, Response};
use crate::field::Field;
use crate::{Painter, Theme};

/// One widget of a window.
pub enum Widget<'a> {
    Button(Button<'a>),
    Field(Field<'a>),
}

impl<'a> Widget<'a> {
    pub fn bounds(&self) -> Rect {
        match self {
            Self::Button(button) => button.bounds(),
            Self::Field(field) => field.bounds(),
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.bounds().contains_point(render::Point::new(x, y))
    }

    /// The height this widget wants in its theme.
    pub fn preferred_height(&self, theme: &Theme) -> i32 {
        match self {
            Self::Button(_) => Button::height(theme),
            Self::Field(_) => Field::height(theme),
        }
    }

    pub fn layout(&mut self, bounds: Rect, theme: &Theme) {
        match self {
            Self::Button(button) => button.layout(bounds),
            Self::Field(field) => field.layout(bounds, theme),
        }
    }

    pub fn handle(&mut self, event: Event) -> Response {
        match self {
            Self::Button(button) => button.handle(event),
            Self::Field(field) => field.handle(event),
        }
    }

    pub fn paint(&self, painter: &mut Painter<'_>, theme: &Theme) {
        match self {
            Self::Button(button) => button.paint(painter, theme),
            Self::Field(field) => field.paint(painter, theme),
        }
    }
}

/// The widgets of one window, plus the focus and the pointer grab.
pub struct Form<'a, const N: usize> {
    widgets: [Widget<'a>; N],
    focus: Option<usize>,
    grab: Option<usize>,
    theme: Theme,
}

impl<'a, const N: usize> Form<'a, N> {
    /// A form whose first widget has the focus.
    pub fn new(widgets: [Widget<'a>; N], theme: Theme) -> Self {
        let mut form = Self {
            widgets,
            focus: None,
            grab: None,
            theme,
        };
        // The focus is a handover like any other, so the first widget hears
        // about it the way a later one does.
        if N > 0 {
            form.focus_on(Some(0));
        }
        form
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn focus(&self) -> Option<usize> {
        self.focus
    }

    pub fn widget(&self, index: usize) -> Option<&Widget<'a>> {
        self.widgets.get(index)
    }

    pub fn widget_mut(&mut self, index: usize) -> Option<&mut Widget<'a>> {
        self.widgets.get_mut(index)
    }

    /// Stack the widgets down `area`, each at its preferred height, separated by
    /// `gap` pixels.
    pub fn stack(&mut self, area: Rect, gap: i32) {
        let theme = self.theme;
        let mut y = area.y;
        for widget in self.widgets.iter_mut() {
            let height = widget.preferred_height(&theme);
            widget.layout(Rect::new(area.x, y, area.w, height), &theme);
            y += height + gap;
        }
    }

    /// Hand the focus to widget `next`, telling both ends of the move.
    ///
    /// Returning `CHANGED` even when the widgets report nothing is deliberate:
    /// the ring that disappeared has to be repainted too.
    pub fn focus_on(&mut self, next: Option<usize>) -> Response {
        if self.focus == next {
            return Response::IGNORED;
        }
        let mut response = Response::IGNORED;
        if let Some(old) = self.focus {
            response = response.or(self.widgets[old].handle(Event::Focus { focused: false }));
        }
        self.focus = next;
        if let Some(new) = next {
            response = response.or(self.widgets[new].handle(Event::Focus { focused: true }));
        }
        response.or(Response::CHANGED)
    }

    /// The topmost widget under a point.
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        self.widgets
            .iter()
            .rposition(|widget| widget.contains(x, y))
    }

    pub fn handle(&mut self, event: Event) -> Response {
        match event {
            // Tab moves the focus and wraps; every other key goes to the widget
            // that holds it.
            Event::Key {
                key: Key::Tab,
                pressed: true,
            } => {
                if N == 0 {
                    return Response::IGNORED;
                }
                let next = match self.focus {
                    Some(index) => (index + 1) % N,
                    None => 0,
                };
                self.focus_on(Some(next))
            }
            Event::Key { .. } => match self.focus {
                Some(index) => self.widgets[index].handle(event),
                None => Response::IGNORED,
            },
            Event::Pointer { x, y } => match self.hit(x, y) {
                Some(index) => self.widgets[index].handle(event),
                None => Response::IGNORED,
            },
            // A press focuses the widget under it and is a grab, so the release
            // comes back here even after the pointer leaves the widget.
            Event::Button {
                x,
                y,
                pressed: true,
            } => {
                let Some(index) = self.hit(x, y) else {
                    return Response::IGNORED;
                };
                self.grab = Some(index);
                let focused = self.focus_on(Some(index));
                focused.or(self.widgets[index].handle(event))
            }
            Event::Button { pressed: false, .. } => match self.grab.take() {
                Some(index) => self.widgets[index].handle(event),
                None => Response::IGNORED,
            },
            Event::Focus { .. } => match self.focus {
                Some(index) => self.widgets[index].handle(event),
                None => Response::IGNORED,
            },
        }
    }

    /// Record the whole window: the background, then the widgets bottom to top.
    pub fn paint(&mut self, pass: &mut Pass, atlas: &Atlas, texture: TextureId) {
        let mut painter = Painter::new(pass, atlas, texture, self.theme.scale);
        painter.clear(self.theme.background);
        for widget in self.widgets.iter() {
            widget.paint(&mut painter, &self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, Key, Response};
    use minix_std::font::FONT_8X16;
    use render::Rect;
    use render::pass::{Element, Pass};
    use render::text::Atlas;

    const AREA: Rect = Rect::new(4, 4, 100, 100);
    const TEXTURE: TextureId = TextureId(0);

    fn form() -> Form<'static, 2> {
        let mut form = Form::new(
            [
                Widget::Button(Button::new(b"one")),
                Widget::Button(Button::new(b"two")),
            ],
            Theme::DARK,
        );
        form.stack(AREA, 4);
        form
    }

    fn button<'a>(form: &'a Form<'static, 2>, index: usize) -> &'a Button<'static> {
        match form.widget(index).expect("the widget exists") {
            Widget::Button(button) => button,
            Widget::Field(_) => panic!("widget {index} is not a button"),
        }
    }

    fn bounds_of(form: &Form<'static, 2>, index: usize) -> Rect {
        form.widget(index).expect("the widget exists").bounds()
    }

    fn field<'a>(form: &'a Form<'static, 2>, index: usize) -> &'a Field<'static> {
        match form.widget(index).expect("the widget exists") {
            Widget::Field(field) => field,
            Widget::Button(_) => panic!("widget {index} is not a field"),
        }
    }

    /// A field above a button, the order Tab visits them in.
    fn mixed() -> Form<'static, 2> {
        let mut form = Form::new(
            [
                Widget::Field(Field::new().placeholder(b"name")),
                Widget::Button(Button::new(b"OK")),
            ],
            Theme::DARK,
        );
        form.stack(AREA, 4);
        form
    }

    fn focused(form: &Form<'static, 2>) -> [bool; 2] {
        [button(form, 0).is_focused(), button(form, 1).is_focused()]
    }

    fn press(x: i32, y: i32) -> Event {
        Event::Button {
            x,
            y,
            pressed: true,
        }
    }

    fn release(x: i32, y: i32) -> Event {
        Event::Button {
            x,
            y,
            pressed: false,
        }
    }

    fn key(key: Key, pressed: bool) -> Event {
        Event::Key { key, pressed }
    }

    #[test]
    fn tab_leads_from_a_field_to_a_button_and_typing_stays_in_the_field() {
        let mut form = mixed();
        assert_eq!(form.focus(), Some(0));
        for byte in b"hi" {
            form.handle(key(Key::Char(*byte), true));
        }
        assert_eq!(field(&form, 0).text(), b"hi");

        form.handle(key(Key::Tab, true));
        assert_eq!(form.focus(), Some(1));

        // The field is no longer focused, so the byte goes nowhere.
        form.handle(key(Key::Char(b'x'), true));
        assert_eq!(field(&form, 0).text(), b"hi");

        form.handle(key(Key::Enter, true));
        assert_eq!(button(&form, 1).activations(), 1);
    }

    #[test]
    fn a_field_keeps_its_text_across_a_focus_round_trip() {
        let mut form = mixed();
        for byte in b"ab" {
            form.handle(key(Key::Char(*byte), true));
        }
        form.handle(key(Key::Tab, true));
        form.handle(key(Key::Tab, true));
        assert_eq!(form.focus(), Some(0));
        assert_eq!(field(&form, 0).text(), b"ab");
        assert_eq!(field(&form, 0).cursor(), 2);
    }

    #[test]
    fn the_first_widget_starts_focused() {
        let form = form();
        assert_eq!(form.focus(), Some(0));
        assert_eq!(focused(&form), [true, false]);
    }

    #[test]
    fn tab_moves_the_focus_and_wraps() {
        let mut form = form();
        assert_eq!(form.handle(key(Key::Tab, true)), Response::CHANGED);
        assert_eq!(form.focus(), Some(1));
        assert_eq!(focused(&form), [false, true]);
        assert_eq!(form.handle(key(Key::Tab, true)), Response::CHANGED);
        assert_eq!(form.focus(), Some(0));
    }

    #[test]
    fn a_key_reaches_only_the_focused_widget() {
        let mut form = form();
        form.handle(key(Key::Enter, true));
        assert_eq!(button(&form, 0).activations(), 1);
        assert_eq!(button(&form, 1).activations(), 0);

        form.handle(key(Key::Tab, true));
        form.handle(key(Key::Enter, true));
        assert_eq!(button(&form, 0).activations(), 1);
        assert_eq!(button(&form, 1).activations(), 1);
    }

    #[test]
    fn a_pointer_press_focuses_the_widget_beneath_it() {
        let mut form = form();
        let target = bounds_of(&form, 1);
        let (x, y) = (target.x + 2, target.y + 2);

        form.handle(press(x, y));
        assert_eq!(form.focus(), Some(1));
        form.handle(release(x, y));
        assert_eq!(button(&form, 1).activations(), 1);
    }

    #[test]
    fn a_release_goes_back_to_the_widget_that_was_pressed() {
        let mut form = form();
        let under = bounds_of(&form, 1);
        let first = bounds_of(&form, 0);

        form.handle(press(under.x + 2, under.y + 2));
        // Released over the other widget: the grab must route it to the second.
        form.handle(release(first.x + 2, first.y + 2));
        assert_eq!(button(&form, 1).activations(), 0);
        assert_eq!(button(&form, 0).activations(), 0);
    }

    #[test]
    fn a_click_outside_every_widget_is_ignored() {
        let mut form = form();
        assert_eq!(form.handle(press(1000, 1000)), Response::IGNORED);
        assert_eq!(form.focus(), Some(0));
    }

    #[test]
    fn stacking_places_widgets_one_height_apart() {
        let form = form();
        let height = Theme::DARK.row_height;
        assert_eq!(
            bounds_of(&form, 0),
            Rect::new(AREA.x, AREA.y, AREA.w, height)
        );
        assert_eq!(
            bounds_of(&form, 1),
            Rect::new(AREA.x, AREA.y + height + 4, AREA.w, height)
        );
    }

    #[test]
    fn paint_emits_the_background_the_surfaces_the_ring_and_the_glyphs() {
        let theme = Theme::DARK;
        let mut form = form();
        let atlas = Atlas::from_bitmap_font(&FONT_8X16);
        let mut pass = Pass::new(Rect::new(0, 0, 120, 120));
        form.paint(&mut pass, &atlas, TEXTURE);

        let elements = pass.elements();
        assert_eq!(
            elements[0],
            Element::Clear {
                color: theme.background
            }
        );

        assert!(
            elements.contains(&Element::Rect {
                rect: bounds_of(&form, 0),
                color: theme.surface,
            }),
            "the first widget's surface is missing"
        );

        // The focus ring is the accent colour on all four edges.
        let ring = elements
            .iter()
            .filter(|element| {
                matches!(
                    element,
                    Element::Rect {
                        color,
                        ..
                    } if *color == theme.accent
                )
            })
            .count();
        assert_eq!(ring, 4);

        let glyphs = elements
            .iter()
            .filter(|element| {
                matches!(
                    element,
                    Element::Texture {
                        texture,
                        opaque,
                        ..
                    } if *texture == TEXTURE && !*opaque
                )
            })
            .count();
        // "one" and "two" are six printable bytes with no spaces.
        assert_eq!(glyphs, 6);
    }

    #[test]
    fn the_label_is_centred_in_its_widget() {
        let theme = Theme::DARK;
        let mut form = form();
        let atlas = Atlas::from_bitmap_font(&FONT_8X16);
        let mut pass = Pass::new(Rect::new(0, 0, 120, 120));
        form.paint(&mut pass, &atlas, TEXTURE);

        let bounds = bounds_of(&form, 0);
        let label_width = render::text::line_width(b"one", theme.scale);
        let expected = bounds.x + (bounds.w - label_width) / 2;
        let first_glyph = pass
            .elements()
            .iter()
            .find_map(|element| match element {
                Element::Texture { rect, .. } => Some(rect.x),
                _ => None,
            })
            .expect("a glyph");
        assert_eq!(first_glyph, expected);
    }
}
