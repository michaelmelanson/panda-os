//! Rectangle geometry shared by the protocol and the compositor.
//!
//! Ported from the in-kernel `resource::Rect` (deleted in Phase 5 of
//! plans/userspace-compositor.md) so that damage tracking is expressed in
//! userspace types on both sides of the wire.

/// An axis-aligned rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Whether two rectangles overlap.
    pub fn intersects(&self, other: &Rect) -> bool {
        !(self.x >= other.x + other.width
            || self.x + self.width <= other.x
            || self.y >= other.y + other.height
            || self.y + self.height <= other.y)
    }

    /// The overlapping area of two rectangles, if any.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        if !self.intersects(other) {
            return None;
        }

        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let x2 = (self.x + self.width).min(other.x + other.width);
        let y2 = (self.y + self.height).min(other.y + other.height);

        Some(Rect {
            x,
            y,
            width: x2 - x,
            height: y2 - y,
        })
    }

    /// The smallest rectangle containing both rectangles.
    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let x2 = (self.x + self.width).max(other.x + other.width);
        let y2 = (self.y + self.height).max(other.y + other.height);

        Rect {
            x,
            y,
            width: x2 - x,
            height: y2 - y,
        }
    }

    /// Whether two rectangles share an edge.
    pub fn is_adjacent(&self, other: &Rect) -> bool {
        let h_adjacent = (self.x + self.width == other.x || other.x + other.width == self.x)
            && !(self.y >= other.y + other.height || other.y >= self.y + self.height);

        let v_adjacent = (self.y + self.height == other.y || other.y + other.height == self.y)
            && !(self.x >= other.x + other.width || other.x >= self.x + self.width);

        h_adjacent || v_adjacent
    }

    /// Whether a point lies inside the rectangle.
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// Whether the rectangle covers no pixels.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}
