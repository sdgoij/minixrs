//! Geometry shared by the scene: points and integer rectangles.
//!
//! Rectangles are half-open on the right and bottom edge (`x .. x + w`,
//! `y .. y + h`), which is what a scissor and a texture upload both want and
//! what makes adjacent rectangles tile without a shared pixel.

/// A point in logical output coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in logical output coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// The right edge, exclusive. Empty rectangles may report an edge at or
    /// left of `x`.
    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    /// The bottom edge, exclusive.
    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    /// A rectangle with no pixels. This is the only empty shape; there is no
    /// separate flag and no normalization of negative extents.
    pub const fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub const fn area(self) -> i64 {
        if self.is_empty() {
            0
        } else {
            self.w as i64 * self.h as i64
        }
    }

    pub const fn contains_point(self, point: Point) -> bool {
        !self.is_empty()
            && point.x >= self.x
            && point.x < self.right()
            && point.y >= self.y
            && point.y < self.bottom()
    }

    /// The overlap of two rectangles, or `None` when they do not overlap.
    pub fn intersection(self, other: Self) -> Option<Self> {
        if self.is_empty() || other.is_empty() {
            return None;
        }

        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            return None;
        }

        Some(Self::new(x, y, right - x, bottom - y))
    }

    /// The smallest rectangle containing both. An empty operand is ignored.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }

        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::new(x, y, right - x, bottom - y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_when_either_extent_is_not_positive() {
        assert!(Rect::new(0, 0, 0, 10).is_empty());
        assert!(Rect::new(0, 0, 10, 0).is_empty());
        assert!(Rect::new(0, 0, -1, 10).is_empty());
        assert!(!Rect::new(0, 0, 1, 1).is_empty());
    }

    #[test]
    fn edges_are_half_open() {
        let r = Rect::new(10, 20, 30, 40);
        assert_eq!(r.right(), 40);
        assert_eq!(r.bottom(), 60);
        assert!(r.contains_point(Point::new(10, 20)));
        assert!(r.contains_point(Point::new(39, 59)));
        assert!(!r.contains_point(Point::new(40, 20)));
        assert!(!r.contains_point(Point::new(10, 60)));
    }

    #[test]
    fn adjacent_rectangles_share_an_edge_but_no_pixel() {
        let left = Rect::new(0, 0, 10, 10);
        let right = Rect::new(10, 0, 10, 10);
        assert_eq!(left.intersection(right), None);
        assert!(!left.contains_point(Point::new(10, 5)));
        assert!(right.contains_point(Point::new(10, 5)));
    }

    #[test]
    fn intersection_clips_both_ways() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersection(b), Some(Rect::new(5, 5, 5, 5)));
        assert_eq!(a.intersection(Rect::new(20, 20, 4, 4)), None);
    }

    #[test]
    fn union_ignores_empty_operands() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.union(Rect::default()), a);
        assert_eq!(Rect::default().union(a), a);
        assert_eq!(
            Rect::new(0, 0, 10, 10).union(Rect::new(20, 20, 5, 5)),
            Rect::new(0, 0, 25, 25)
        );
    }
}
