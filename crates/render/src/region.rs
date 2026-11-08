//! Damage as a set of rectangles.
//!
//! A [`Region`] is the union of its rectangles, and the rectangles are kept
//! pairwise disjoint: adding a rectangle subtracts what is already there, and
//! subtracting one splits the pieces it overlaps. Disjointness is not an
//! optimisation, it is the contract the renderer relies on, because a region is
//! spent as a scissor one rectangle at a time and overlapping rectangles would
//! draw a translucent element twice.
//!
//! Adjacent pieces are not coalesced. Fragmentation costs draw calls, never
//! coverage, and a coalescing pass is only worth adding once a measurement asks
//! for it.

use alloc::vec::Vec;

use crate::geom::{Point, Rect};

/// A set of disjoint rectangles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Region {
    rects: Vec<Rect>,
}

impl Region {
    pub const fn new() -> Self {
        Self { rects: Vec::new() }
    }

    pub fn from_rect(rect: Rect) -> Self {
        let mut region = Self::new();
        region.add(rect);
        region
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rects.len()
    }

    pub fn rects(&self) -> impl Iterator<Item = Rect> + '_ {
        self.rects.iter().copied()
    }

    /// Total covered area. Exact because the rectangles are disjoint.
    pub fn area(&self) -> i64 {
        self.rects.iter().map(|rect| rect.area()).sum()
    }

    /// Add a rectangle, keeping the pieces disjoint.
    pub fn add(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }

        let mut pieces = Vec::new();
        pieces.push(rect);
        for existing in &self.rects {
            if pieces.is_empty() {
                break;
            }
            let mut remainder = Vec::new();
            for piece in pieces.drain(..) {
                subtract_rect(piece, *existing, &mut remainder);
            }
            pieces = remainder;
        }
        self.rects.extend(pieces);
    }

    pub fn add_region(&mut self, other: &Self) {
        for rect in other.rects() {
            self.add(rect);
        }
    }

    /// The smallest rectangle containing the whole region.
    pub fn bounds(&self) -> Option<Rect> {
        let mut rects = self.rects();
        let first = rects.next()?;
        Some(rects.fold(first, Rect::union))
    }

    pub fn contains(&self, point: Point) -> bool {
        self.rects.iter().any(|rect| rect.contains_point(point))
    }

    pub fn intersects(&self, rect: Rect) -> bool {
        !rect.is_empty()
            && self
                .rects
                .iter()
                .any(|own| own.intersection(rect).is_some())
    }

    /// The part of this region inside `rect`.
    pub fn intersect(&self, rect: Rect) -> Self {
        let mut out = Self::new();
        for own in &self.rects {
            if let Some(clipped) = own.intersection(rect) {
                out.add(clipped);
            }
        }
        out
    }

    /// Remove `rect` from every piece, splitting where it cuts through.
    pub fn subtract(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }

        let mut out = Vec::new();
        for own in self.rects.drain(..) {
            subtract_rect(own, rect, &mut out);
        }
        self.rects = out;
    }
}

/// Push the parts of `rect` not covered by `cut`.
fn subtract_rect(rect: Rect, cut: Rect, out: &mut Vec<Rect>) {
    let Some(overlap) = rect.intersection(cut) else {
        out.push(rect);
        return;
    };

    if cut.y > rect.y {
        out.push(Rect::new(rect.x, rect.y, rect.w, cut.y - rect.y));
    }
    if cut.bottom() < rect.bottom() {
        out.push(Rect::new(
            rect.x,
            cut.bottom(),
            rect.w,
            rect.bottom() - cut.bottom(),
        ));
    }
    if cut.x > rect.x {
        out.push(Rect::new(rect.x, overlap.y, cut.x - rect.x, overlap.h));
    }
    if cut.right() < rect.right() {
        out.push(Rect::new(
            cut.right(),
            overlap.y,
            rect.right() - cut.right(),
            overlap.h,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn a_new_region_is_empty() {
        let region = Region::new();
        assert!(region.is_empty());
        assert_eq!(region.len(), 0);
        assert_eq!(region.area(), 0);
        assert_eq!(region.bounds(), None);
        assert!(!region.contains(Point::new(0, 0)));
    }

    #[test]
    fn an_empty_rect_adds_nothing() {
        let mut region = Region::new();
        region.add(Rect::default());
        assert!(region.is_empty());
    }

    #[test]
    fn overlapping_adds_stay_disjoint_and_keep_their_area() {
        let mut region = Region::from_rect(rect(0, 0, 10, 10));
        region.add(rect(5, 5, 10, 10));

        assert_eq!(region.area(), 100 + 100 - 25);
        assert!(region.contains(Point::new(0, 0)));
        assert!(region.contains(Point::new(14, 14)));
        assert!(!region.contains(Point::new(4, 10)));

        let pieces: Vec<Rect> = region.rects().collect();
        for (i, a) in pieces.iter().enumerate() {
            for b in &pieces[i + 1..] {
                assert_eq!(a.intersection(*b), None, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_covered_add_contributes_nothing() {
        let mut region = Region::from_rect(rect(0, 0, 10, 10));
        region.add(rect(2, 2, 4, 4));
        assert_eq!(region.area(), 100);
        assert_eq!(region.len(), 1);
    }

    #[test]
    fn subtracting_a_hole_leaves_four_pieces() {
        let mut region = Region::from_rect(rect(0, 0, 100, 100));
        region.subtract(rect(25, 25, 50, 50));

        assert_eq!(region.len(), 4);
        assert_eq!(region.area(), 10_000 - 2_500);
        assert!(!region.contains(Point::new(50, 50)));
        assert!(region.contains(Point::new(0, 0)));
        assert!(region.contains(Point::new(99, 0)));
        assert!(region.contains(Point::new(0, 99)));
        assert!(region.contains(Point::new(99, 99)));
    }

    #[test]
    fn subtracting_the_whole_region_empties_it() {
        let mut region = Region::from_rect(rect(0, 0, 10, 10));
        region.subtract(rect(-5, -5, 20, 20));
        assert!(region.is_empty());
        assert_eq!(region.area(), 0);
    }

    #[test]
    fn subtracting_a_disjoint_rect_changes_nothing() {
        let mut region = Region::from_rect(rect(0, 0, 10, 10));
        region.subtract(rect(50, 50, 10, 10));
        assert_eq!(region.area(), 100);
    }

    #[test]
    fn subtract_covers_exactly_the_original_minus_the_cut() {
        let mut region = Region::from_rect(rect(0, 0, 16, 12));
        let cut = rect(3, 5, 7, 4);
        region.subtract(cut);

        for y in -1..14 {
            for x in -1..18 {
                let point = Point::new(x, y);
                let inside_original = rect(0, 0, 16, 12).contains_point(point);
                let inside_cut = cut.contains_point(point);
                assert_eq!(
                    region.contains(point),
                    inside_original && !inside_cut,
                    "point ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn intersect_clips_every_piece() {
        let region = Region::from_rect(rect(0, 0, 100, 100));
        let clipped = region.intersect(rect(50, 50, 100, 100));

        assert_eq!(clipped.area(), 2_500);
        assert!(clipped.contains(Point::new(50, 50)));
        assert!(!clipped.contains(Point::new(49, 50)));
        assert_eq!(region.intersect(rect(200, 200, 10, 10)).area(), 0);
    }

    #[test]
    fn bounds_encloses_all_pieces() {
        let mut region = Region::from_rect(rect(10, 10, 5, 5));
        region.add(rect(30, 40, 5, 5));
        assert_eq!(region.bounds(), Some(rect(10, 10, 25, 35)));
    }

    #[test]
    fn intersects_uses_half_open_edges() {
        let region = Region::from_rect(rect(0, 0, 10, 10));
        assert!(region.intersects(rect(9, 9, 5, 5)));
        assert!(!region.intersects(rect(10, 0, 5, 5)));
        assert!(!region.intersects(Rect::default()));
    }
}
