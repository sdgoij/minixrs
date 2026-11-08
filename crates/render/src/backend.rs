//! The seam between the pass list and a renderer.
//!
//! [`Backend`] is the whole vocabulary a renderer has to implement: one element
//! and one damage rectangle per call. [`crate::Pass::submit`] walks the plan and
//! issues them in draw order, which keeps the ordering and scissor rules in one
//! host-testable place instead of in every renderer.
//!
//! This is the counterpart of Hyprland's `IElementRenderer` (`src/render/
//! ElementRenderer.hpp`, BSD-3-Clause): one dispatch per element type, so a
//! renderer cannot invent its own ordering. The difference is that the
//! rectangle is passed in rather than held by the renderer, because damage is
//! disjoint and a backend never has to reason about a region as a whole.

use crate::geom::Rect;
use crate::pass::Element;

/// A sink for the elements a pass decided to draw.
pub trait Backend {
    type Error;

    /// Draw `element`, clipped to `scissor`.
    ///
    /// Every rectangle of an element's damage arrives as its own call. The
    /// damage rectangles are disjoint (see [`crate::Region`]), so no pixel is
    /// drawn twice for one element, which is what keeps a translucent element
    /// from blending with itself.
    fn draw(&mut self, element: &Element, scissor: Rect) -> Result<(), Self::Error>;
}
