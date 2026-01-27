//! Graphics types and abstractions.
//!
//! This module provides high-level abstractions for graphics operations,
//! including colours, rectangles, surfaces, and pixel buffers.

mod pixels;
mod surface;

pub use pixels::PixelBuffer;
pub use surface::{Surface, SurfaceInfo, Window, WindowBuilder};

/// A 32-bit ARGB colour.
///
/// The colour is stored as `0xAARRGGBB`:
/// - `AA` - Alpha (0 = transparent, 255 = opaque)
/// - `RR` - Red
/// - `GG` - Green
/// - `BB` - Blue
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct Colour(pub u32);

impl Colour {
    /// Transparent (alpha = 0).
    pub const TRANSPARENT: Self = Self(0x00000000);
    /// Black.
    pub const BLACK: Self = Self(0xFF000000);
    /// White.
    pub const WHITE: Self = Self(0xFFFFFFFF);
    /// Red.
    pub const RED: Self = Self(0xFFFF0000);
    /// Green.
    pub const GREEN: Self = Self(0xFF00FF00);
    /// Blue.
    pub const BLUE: Self = Self(0xFF0000FF);
    /// Yellow.
    pub const YELLOW: Self = Self(0xFFFFFF00);
    /// Cyan.
    pub const CYAN: Self = Self(0xFF00FFFF);
    /// Magenta.
    pub const MAGENTA: Self = Self(0xFFFF00FF);

    /// Create a colour from RGBA components.
    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    /// Create a colour from RGB components (fully opaque).
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    /// Get the red component.
    #[inline]
    pub const fn r(self) -> u8 {
        ((self.0 >> 16) & 0xFF) as u8
    }

    /// Get the green component.
    #[inline]
    pub const fn g(self) -> u8 {
        ((self.0 >> 8) & 0xFF) as u8
    }

    /// Get the blue component.
    #[inline]
    pub const fn b(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// Get the alpha component.
    #[inline]
    pub const fn a(self) -> u8 {
        ((self.0 >> 24) & 0xFF) as u8
    }

    /// Create a colour with the given alpha value.
    #[inline]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self((self.0 & 0x00FFFFFF) | ((a as u32) << 24))
    }

    /// Get the raw 32-bit ARGB value.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl From<u32> for Colour {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Colour> for u32 {
    fn from(colour: Colour) -> u32 {
        colour.0
    }
}

/// A rectangle defined by position and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// X coordinate of the top-left corner.
    pub x: u32,
    /// Y coordinate of the top-left corner.
    pub y: u32,
    /// Width of the rectangle.
    pub width: u32,
    /// Height of the rectangle.
    pub height: u32,
}

impl Rect {
    /// Create a new rectangle.
    #[inline]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Create a rectangle at the origin with the given size.
    #[inline]
    pub const fn from_size(width: u32, height: u32) -> Self {
        Self::new(0, 0, width, height)
    }

    /// Check if a point is inside the rectangle.
    #[inline]
    pub const fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// Get the right edge (x + width).
    #[inline]
    pub const fn right(&self) -> u32 {
        self.x + self.width
    }

    /// Get the bottom edge (y + height).
    #[inline]
    pub const fn bottom(&self) -> u32 {
        self.y + self.height
    }
}
