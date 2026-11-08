//! The colours and metrics every widget draws with.

use render::Color;

/// One palette and one set of metrics, shared by the widget tree.
///
/// A single size for every widget is deliberate at this stage: the glyph atlas
/// is a fixed 8x16 cell, so a row is that cell plus the vertical padding, and a
/// widget that needs a different height can say so at layout time. A real style
/// engine can come later, when something asks for one.
///
/// There is no text colour: glyphs are drawn from the atlas, whose coverage is
/// white, and the pass list has no per-element tint yet. A label is therefore
/// white on whatever surface its widget painted underneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// The window behind the widgets.
    pub background: Color,
    /// A widget at rest.
    pub surface: Color,
    /// A widget under the pointer or held down.
    pub surface_active: Color,
    /// The focus ring and the selection bar.
    pub accent: Color,
    /// A widget's edge when it is not focused.
    pub line: Color,
    /// How thick a widget's edge and the focus ring are.
    pub border: i32,
    /// Space between a widget's edge and its contents.
    pub padding: i32,
    /// The height of one row, text included.
    pub row_height: i32,
    /// The text scale; the atlas cell and the advance multiply by it.
    pub scale: i32,
}

impl Theme {
    /// A dark palette for the demo and the gates.
    pub const DARK: Self = Self {
        background: Color::rgba(0x10, 0x14, 0x18, 255),
        surface: Color::rgba(0x24, 0x2A, 0x31, 255),
        surface_active: Color::rgba(0x36, 0x40, 0x4A, 255),
        accent: Color::rgba(0x30, 0x80, 0xFF, 255),
        line: Color::rgba(0x44, 0x4C, 0x56, 255),
        border: 1,
        padding: 4,
        row_height: 24,
        scale: 1,
    };
}
