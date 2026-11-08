//! Input the widget layer understands, and what a widget did with it.

/// A key the widget layer acts on, already translated from the device.
///
/// The translation from evdev codes and modifier state belongs to the client:
/// those are device details, and keeping them out of here is what makes the
/// widget tests need no keymap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A printable ASCII byte, shifted as the layout dictates.
    Char(u8),
    Backspace,
    Enter,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Escape,
}

/// One thing that can happen to the widgets of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A key went down or up while the window had the input focus.
    Key { key: Key, pressed: bool },
    /// The pointer is at a point local to the window.
    Pointer { x: i32, y: i32 },
    /// A pointer button changed state at a point local to the window.
    Button { x: i32, y: i32, pressed: bool },
    /// The window gained or lost the input focus.
    Focus { focused: bool },
}

/// What a widget did with an event.
///
/// A caller needs all three answers: whether to try the next handler, whether to
/// record another frame, and whether the widget fired. Collapsing them into one
/// bool would force every caller to guess the other two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub handled: bool,
    pub changed: bool,
    pub activated: bool,
}

impl Response {
    pub const IGNORED: Self = Self::new(false, false, false);
    pub const HANDLED: Self = Self::new(true, false, false);
    pub const CHANGED: Self = Self::new(true, true, false);
    pub const ACTIVATED: Self = Self::new(true, true, true);

    pub const fn new(handled: bool, changed: bool, activated: bool) -> Self {
        Self {
            handled,
            changed,
            activated,
        }
    }

    /// The union of two responses.
    pub const fn or(self, other: Self) -> Self {
        Self {
            handled: self.handled || other.handled,
            changed: self.changed || other.changed,
            activated: self.activated || other.activated,
        }
    }
}
