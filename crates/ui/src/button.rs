//! A push button: a label, a focus ring, a pressed state and a count of fires.

use render::{Point, Rect};

use crate::event::{Event, Key, Response};
use crate::{Painter, Theme};

pub struct Button<'a> {
    label: &'a [u8],
    bounds: Rect,
    focused: bool,
    pressed: bool,
    activations: u32,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a [u8]) -> Self {
        Self {
            label,
            bounds: Rect::default(),
            focused: false,
            pressed: false,
            activations: 0,
        }
    }

    pub fn label(&self) -> &'a [u8] {
        self.label
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// How many times the button has fired. A demo reports it; a real caller
    /// would act in `handle` instead.
    pub fn activations(&self) -> u32 {
        self.activations
    }

    pub fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    /// The width the label needs, padding and border included.
    pub fn preferred_width(&self, theme: &Theme) -> i32 {
        render::text::line_width(self.label, theme.scale) + 2 * (theme.padding + theme.border)
    }

    /// The height every widget of this theme uses.
    pub fn height(theme: &Theme) -> i32 {
        theme.row_height
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.bounds.contains_point(Point::new(x, y))
    }

    pub fn handle(&mut self, event: Event) -> Response {
        match event {
            Event::Focus { focused } => {
                if self.focused == focused {
                    return Response::IGNORED;
                }
                self.focused = focused;
                self.pressed = false;
                Response::CHANGED
            }
            Event::Key { key, pressed } if self.focused => match key {
                // Enter is the default action and fires on the way down; Space
                // shows the pressed state first, the way a held button does.
                Key::Enter if pressed => self.fire(),
                Key::Char(b' ') if pressed => {
                    self.pressed = true;
                    Response::CHANGED
                }
                Key::Char(b' ') => {
                    if self.pressed {
                        self.pressed = false;
                        self.fire()
                    } else {
                        Response::IGNORED
                    }
                }
                _ => Response::IGNORED,
            },
            Event::Button { x, y, pressed } => {
                if pressed {
                    if self.contains(x, y) {
                        self.pressed = true;
                        Response::CHANGED
                    } else {
                        Response::IGNORED
                    }
                } else if self.pressed {
                    // The press was a grab, so the release arrives here even if
                    // the pointer left: releasing outside cancels.
                    self.pressed = false;
                    if self.contains(x, y) {
                        self.fire()
                    } else {
                        Response::CHANGED
                    }
                } else {
                    Response::IGNORED
                }
            }
            _ => Response::IGNORED,
        }
    }

    fn fire(&mut self) -> Response {
        self.activations += 1;
        Response::ACTIVATED
    }

    pub fn paint(&self, painter: &mut Painter<'_>, theme: &Theme) {
        painter.fill(
            self.bounds,
            if self.pressed {
                theme.surface_active
            } else {
                theme.surface
            },
        );
        painter.frame(
            self.bounds,
            theme.border,
            if self.focused {
                theme.accent
            } else {
                theme.line
            },
        );

        let inset = theme.border + theme.padding;
        let inner = Rect::new(
            self.bounds.x + inset,
            self.bounds.y + inset,
            self.bounds.w - 2 * inset,
            self.bounds.h - 2 * inset,
        );
        let x = inner.x + (inner.w - painter.measure(self.label)).max(0) / 2;
        let y = inner.y + (inner.h - painter.text_height()).max(0) / 2;
        painter.text(self.label, Point::new(x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    fn button() -> Button<'static> {
        let mut b = Button::new(b"OK");
        b.layout(Rect::new(10, 10, 60, 24));
        b
    }

    #[test]
    fn a_press_and_release_inside_activate() {
        let mut b = button();
        assert_eq!(
            b.handle(Event::Button {
                x: 20,
                y: 20,
                pressed: true
            }),
            Response::CHANGED
        );
        assert_eq!(
            b.handle(Event::Button {
                x: 20,
                y: 20,
                pressed: false
            }),
            Response::ACTIVATED
        );
        assert_eq!(b.activations(), 1);
    }

    #[test]
    fn a_release_outside_cancels() {
        let mut b = button();
        b.handle(Event::Button {
            x: 20,
            y: 20,
            pressed: true,
        });
        assert_eq!(
            b.handle(Event::Button {
                x: 200,
                y: 20,
                pressed: false
            }),
            Response::CHANGED
        );
        assert_eq!(b.activations(), 0);
    }

    #[test]
    fn a_press_outside_is_ignored() {
        let mut b = button();
        assert_eq!(
            b.handle(Event::Button {
                x: 200,
                y: 20,
                pressed: true
            }),
            Response::IGNORED
        );
    }

    #[test]
    fn enter_fires_and_space_fires_on_release() {
        let mut b = button();
        b.handle(Event::Focus { focused: true });

        assert_eq!(
            b.handle(Event::Key {
                key: Key::Enter,
                pressed: true
            }),
            Response::ACTIVATED
        );

        assert_eq!(
            b.handle(Event::Key {
                key: Key::Char(b' '),
                pressed: true
            }),
            Response::CHANGED
        );
        assert_eq!(b.activations(), 1);
        assert_eq!(
            b.handle(Event::Key {
                key: Key::Char(b' '),
                pressed: false
            }),
            Response::ACTIVATED
        );
        assert_eq!(b.activations(), 2);
    }

    #[test]
    fn an_unfocused_button_ignores_keys() {
        let mut b = button();
        assert_eq!(
            b.handle(Event::Key {
                key: Key::Enter,
                pressed: true
            }),
            Response::IGNORED
        );
        assert_eq!(b.activations(), 0);
    }

    #[test]
    fn losing_the_focus_drops_a_press() {
        let mut b = button();
        b.handle(Event::Focus { focused: true });
        b.handle(Event::Key {
            key: Key::Char(b' '),
            pressed: true,
        });
        assert_eq!(b.handle(Event::Focus { focused: false }), Response::CHANGED);
        assert_eq!(
            b.handle(Event::Key {
                key: Key::Char(b' '),
                pressed: false
            }),
            Response::IGNORED
        );
        assert_eq!(b.activations(), 0);
    }
}
