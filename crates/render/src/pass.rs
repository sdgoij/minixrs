//! The per-frame pass: a command list plus the damage each command still owns.
//!
//! The shape follows Hyprland's `CRenderPass` (`src/render/pass/Pass.cpp`, at
//! commit `a8d9a4e`, BSD-3-Clause), which `.notes/gui.md` §9 names as the
//! reference. Elements are recorded bottom to top. The damage plan is computed
//! by walking them top to bottom, so an opaque element removes from the damage
//! of everything beneath it. The Rust form is one enum and one exhaustive
//! function rather than a class hierarchy, because the element set is closed.
//!
//! A caller records a frame with [`Pass::add`] and reads it back with
//! [`Pass::plan`]. Each [`DrawOp`] carries the damage still visible through the
//! elements above it, and the renderer must clip to that region. That contract
//! is what keeps a partly covered element from repainting the whole output.

use alloc::vec::Vec;

use crate::backend::Backend;
use crate::geom::Rect;
use crate::region::Region;

/// Non-premultiplied 8-bit RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }
}

/// An identifier the renderer resolves to a texture it owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureId(pub u32);

/// One command in a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Element {
    /// Fill everything in the op's damage, normally the first element.
    Clear { color: Color },
    /// A solid rectangle.
    Rect { rect: Rect, color: Color },
    /// A rectangle of a texture, sampled from `source` in texture pixels.
    Texture {
        rect: Rect,
        source: Rect,
        texture: TextureId,
        /// What the sampled texel is multiplied by. [`Color::WHITE`] leaves the
        /// texture as it is; anything else tints it, which is how the white
        /// glyph atlas becomes text of a chosen colour.
        color: Color,
        /// Set only when every pixel the rectangle samples is opaque, the tint
        /// included. A translucent or unknown buffer must say `false`, or it
        /// will erase what is underneath it from the damage plan.
        opaque: bool,
    },
}

impl Element {
    /// The logical extent of the element, clipped by nothing.
    pub fn bounds(self, output: Rect) -> Option<Rect> {
        match self {
            Self::Clear { .. } => (!output.is_empty()).then_some(output),
            Self::Rect { rect, .. } | Self::Texture { rect, .. } => {
                (!rect.is_empty()).then_some(rect)
            }
        }
    }

    /// The part of the element that lets nothing below it show through.
    pub fn opaque_region(self, output: Rect) -> Region {
        let opaque = match self {
            Self::Clear { color } | Self::Rect { color, .. } => color.is_opaque(),
            // A tinted texture is only as opaque as its tint.
            Self::Texture { opaque, color, .. } => opaque && color.is_opaque(),
        };
        if opaque {
            self.bounds(output)
                .map(Region::from_rect)
                .unwrap_or_default()
        } else {
            Region::new()
        }
    }
}

/// One element to draw, with the damage still visible to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawOp<'a> {
    pub element: &'a Element,
    pub damage: Region,
}

/// The elements recorded for one frame.
#[derive(Clone, Debug)]
pub struct Pass {
    output: Rect,
    damage: Region,
    elements: Vec<Element>,
}

impl Pass {
    /// A pass over the whole output, with the whole output damaged.
    pub fn new(output: Rect) -> Self {
        Self {
            output,
            damage: Region::from_rect(output),
            elements: Vec::new(),
        }
    }

    /// A pass whose damage starts as `damage` rather than the whole output.
    pub fn with_damage(output: Rect, damage: Region) -> Self {
        Self {
            output,
            damage,
            elements: Vec::new(),
        }
    }

    pub fn output(&self) -> Rect {
        self.output
    }

    pub fn damage(&self) -> &Region {
        &self.damage
    }

    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    pub fn add(&mut self, element: Element) {
        self.elements.push(element);
    }

    pub fn add_damage(&mut self, rect: Rect) {
        self.damage.add(rect);
    }

    pub fn clear_damage(&mut self) {
        self.damage = Region::new();
    }

    /// The elements to draw, bottom to top, each with the damage still visible
    /// to it. Elements hidden behind damage already claimed by an opaque
    /// element are omitted, and so are elements outside the damage.
    pub fn plan(&self) -> Vec<DrawOp<'_>> {
        let mut remaining = self.damage.intersect(self.output);
        let mut ops = Vec::with_capacity(self.elements.len());

        for element in self.elements.iter().rev() {
            if remaining.is_empty() {
                break;
            }

            let Some(bounds) = element.bounds(self.output) else {
                continue;
            };
            let damage = remaining.intersect(bounds);
            if damage.is_empty() {
                continue;
            }

            ops.push(DrawOp { element, damage });
            for rect in element.opaque_region(self.output).rects() {
                remaining.subtract(rect);
            }
        }

        ops.reverse();
        ops
    }

    /// Hand the plan to a backend, one damage rectangle at a time.
    ///
    /// Draw order is the plan's order, bottom to top, and the rectangles of one
    /// element arrive consecutively. A backend that clips to the rectangle it is
    /// given will paint each pixel at most once per element.
    pub fn submit<B: Backend>(&self, backend: &mut B) -> Result<(), B::Error> {
        for op in self.plan() {
            for scissor in op.damage.rects() {
                backend.draw(op.element, scissor)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    const OUTPUT: Rect = Rect::new(0, 0, 100, 100);

    fn solid(rect: Rect, color: Color) -> Element {
        Element::Rect { rect, color }
    }

    #[test]
    fn a_new_pass_damages_the_whole_output() {
        let pass = Pass::new(OUTPUT);
        assert_eq!(pass.damage().area(), 10_000);
        assert!(pass.plan().is_empty());
    }

    #[test]
    fn an_opaque_element_hides_the_one_below_it() {
        let mut pass = Pass::new(OUTPUT);
        pass.add(solid(OUTPUT, Color::WHITE));
        pass.add(solid(OUTPUT, Color::BLACK));

        let plan = pass.plan();
        assert_eq!(plan.len(), 1);
        assert_eq!(*plan[0].element, solid(OUTPUT, Color::BLACK));
        assert_eq!(plan[0].damage.area(), 10_000);
    }

    #[test]
    fn a_partially_covering_element_keeps_the_rest_damaged() {
        let top = Rect::new(40, 40, 20, 20);
        let mut pass = Pass::new(OUTPUT);
        pass.add(solid(OUTPUT, Color::WHITE));
        pass.add(solid(top, Color::BLACK));

        let plan = pass.plan();
        assert_eq!(plan.len(), 2);

        assert_eq!(*plan[0].element, solid(OUTPUT, Color::WHITE));
        assert_eq!(plan[0].damage.area(), 10_000 - 400);
        assert!(plan[0].damage.contains(Point::new(0, 0)));
        assert!(!plan[0].damage.contains(Point::new(50, 50)));

        assert_eq!(*plan[1].element, solid(top, Color::BLACK));
        assert_eq!(plan[1].damage.area(), 400);
        assert!(plan[1].damage.contains(Point::new(50, 50)));
    }

    #[test]
    fn a_translucent_element_leaves_the_one_below_damaged() {
        let mut pass = Pass::new(OUTPUT);
        pass.add(solid(OUTPUT, Color::WHITE));
        pass.add(solid(OUTPUT, Color::rgba(0, 0, 0, 128)));

        let plan = pass.plan();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].damage.area(), 10_000);
        assert_eq!(plan[1].damage.area(), 10_000);
    }

    #[test]
    fn an_opaque_texture_hides_but_a_translucent_one_does_not() {
        let texture = |opaque| Element::Texture {
            rect: OUTPUT,
            source: Rect::new(0, 0, 64, 64),
            texture: TextureId(1),
            color: Color::WHITE,
            opaque,
        };

        let mut opaque_pass = Pass::new(OUTPUT);
        opaque_pass.add(solid(OUTPUT, Color::WHITE));
        opaque_pass.add(texture(true));
        assert_eq!(opaque_pass.plan().len(), 1);

        let mut translucent_pass = Pass::new(OUTPUT);
        translucent_pass.add(solid(OUTPUT, Color::WHITE));
        translucent_pass.add(texture(false));
        assert_eq!(translucent_pass.plan().len(), 2);
    }

    #[test]
    fn a_translucent_tint_leaves_the_element_below_damaged() {
        let mut pass = Pass::new(OUTPUT);
        pass.add(solid(OUTPUT, Color::WHITE));
        pass.add(Element::Texture {
            rect: OUTPUT,
            source: Rect::new(0, 0, 64, 64),
            texture: TextureId(1),
            color: Color::rgba(255, 255, 255, 128),
            // The caller believes the texels are opaque; the tint still is not.
            opaque: true,
        });

        assert_eq!(pass.plan().len(), 2);
    }

    #[test]
    fn elements_outside_the_damage_are_dropped() {
        let mut pass = Pass::with_damage(OUTPUT, Region::from_rect(Rect::new(0, 0, 10, 10)));
        pass.add(solid(Rect::new(50, 50, 10, 10), Color::WHITE));
        assert!(pass.plan().is_empty());
    }

    #[test]
    fn a_clear_covers_the_output_and_is_scissored_like_any_element() {
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Clear {
            color: Color::BLACK,
        });
        pass.add(solid(Rect::new(2, 2, 4, 4), Color::WHITE));

        let plan = pass.plan();
        assert_eq!(plan.len(), 2);
        assert_eq!(
            *plan[0].element,
            Element::Clear {
                color: Color::BLACK
            }
        );
        assert_eq!(plan[0].damage.area(), 10_000 - 16);
        assert_eq!(plan[1].damage.area(), 16);
    }

    #[test]
    fn an_empty_damage_produces_no_ops() {
        let mut pass = Pass::with_damage(OUTPUT, Region::new());
        pass.add(Element::Clear {
            color: Color::BLACK,
        });
        assert!(pass.plan().is_empty());
    }

    #[test]
    fn clearing_the_damage_empties_the_plan() {
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Clear {
            color: Color::BLACK,
        });
        pass.clear_damage();
        assert!(pass.plan().is_empty());
    }

    #[test]
    fn added_damage_is_unioned_with_the_rest() {
        let mut pass = Pass::with_damage(OUTPUT, Region::from_rect(Rect::new(0, 0, 10, 10)));
        pass.add_damage(Rect::new(0, 0, 20, 20));
        assert_eq!(pass.damage().area(), 400);
    }

    #[derive(Default)]
    struct Recorder {
        calls: Vec<(Element, Rect)>,
    }

    impl Backend for Recorder {
        type Error = core::convert::Infallible;

        fn draw(&mut self, element: &Element, scissor: Rect) -> Result<(), Self::Error> {
            self.calls.push((*element, scissor));
            Ok(())
        }
    }

    fn recorded_area(recorder: &Recorder) -> i64 {
        recorder.calls.iter().map(|call| call.1.area()).sum()
    }

    #[test]
    fn submit_draws_bottom_to_top() {
        let top = Rect::new(40, 40, 20, 20);
        let mut pass = Pass::new(OUTPUT);
        pass.add(Element::Clear {
            color: Color::BLACK,
        });
        pass.add(solid(top, Color::WHITE));

        let mut recorder = Recorder::default();
        pass.submit(&mut recorder).unwrap();

        assert!(recorder.calls.len() > 1);
        assert_eq!(recorder.calls.last().unwrap().0, solid(top, Color::WHITE));
        assert_eq!(recorded_area(&recorder), 10_000);
    }

    #[test]
    fn submit_issues_one_call_per_damage_rect() {
        let top = Rect::new(40, 40, 20, 20);
        let mut pass = Pass::new(OUTPUT);
        pass.add(solid(OUTPUT, Color::WHITE));
        pass.add(solid(top, Color::BLACK));

        let mut recorder = Recorder::default();
        pass.submit(&mut recorder).unwrap();

        // The bottom element's damage is the ring around the top one, which is
        // four rectangles; the top element is one.
        assert_eq!(recorder.calls.len(), 5);
        assert_eq!(recorded_area(&recorder), 10_000);
        assert_eq!(recorder.calls.last().unwrap().0, solid(top, Color::BLACK));

        let rects: Vec<Rect> = recorder.calls.iter().map(|call| call.1).collect();
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                // Nothing here is translucent, so a pixel drawn twice would be a
                // bug rather than a blend.
                assert_eq!(a.intersection(*b), None, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn submit_skips_occluded_elements() {
        let mut pass = Pass::new(OUTPUT);
        pass.add(solid(OUTPUT, Color::WHITE));
        pass.add(solid(OUTPUT, Color::BLACK));

        let mut recorder = Recorder::default();
        pass.submit(&mut recorder).unwrap();
        assert_eq!(recorder.calls.len(), 1);
        assert_eq!(recorder.calls[0].0, solid(OUTPUT, Color::BLACK));
    }

    #[test]
    fn submit_without_damage_makes_no_calls() {
        let mut pass = Pass::with_damage(OUTPUT, Region::new());
        pass.add(Element::Clear {
            color: Color::BLACK,
        });

        let mut recorder = Recorder::default();
        pass.submit(&mut recorder).unwrap();
        assert!(recorder.calls.is_empty());
    }
}
