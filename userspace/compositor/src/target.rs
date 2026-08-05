//! The surface the compositor composites into.

use compositor_protocol::Rect;

/// A BGRA render target with a flushable damage region.
///
/// The compositor's real target is the mapped framebuffer; the trait exists
/// so the window-management logic can be exercised against plain memory on
/// the host, where no display exists.
pub trait Target {
    fn width(&self) -> u32;
    fn height(&self) -> u32;

    /// Fill a rectangle with a solid BGRA colour, clipped to the target.
    fn fill(&mut self, rect: &Rect, colour: u32);

    /// Copy `width` BGRA pixels into a row. `src` is exactly `width * 4`
    /// bytes.
    fn write_row(&mut self, x: u32, y: u32, width: u32, src: &[u8]);

    /// Read a pixel; out-of-bounds reads yield a transparent pixel.
    fn get_pixel(&self, x: u32, y: u32) -> [u8; 4];

    /// Write a pixel; out-of-bounds writes are dropped.
    fn set_pixel(&mut self, x: u32, y: u32, pixel: [u8; 4]);

    /// Present a damaged region.
    fn flush(&mut self, rect: &Rect);
}

/// A `Target` over a plain byte buffer, used by the unit tests and by
/// integration tests that need to inspect composited pixels without a real
/// display (Phase 4 of plans/userspace-compositor.md: the in-kernel
/// compositor still holds the only display claim, so `display:` is `Busy`
/// for the userspace compositor throughout this phase).
#[cfg(any(test, feature = "test-target"))]
pub struct MemoryTarget {
    pub pixels: alloc::vec::Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub flushed: alloc::vec::Vec<Rect>,
}

#[cfg(any(test, feature = "test-target"))]
impl MemoryTarget {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pixels: alloc::vec![0u8; (width * height * 4) as usize],
            width,
            height,
            flushed: alloc::vec::Vec::new(),
        }
    }

    fn offset(&self, x: u32, y: u32) -> usize {
        ((y * self.width + x) * 4) as usize
    }
}

#[cfg(any(test, feature = "test-target"))]
impl Target for MemoryTarget {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn fill(&mut self, rect: &Rect, colour: u32) {
        let bytes = colour.to_le_bytes();
        for y in rect.y..(rect.y + rect.height).min(self.height) {
            for x in rect.x..(rect.x + rect.width).min(self.width) {
                let offset = self.offset(x, y);
                self.pixels[offset..offset + 4].copy_from_slice(&bytes);
            }
        }
    }

    fn write_row(&mut self, x: u32, y: u32, width: u32, src: &[u8]) {
        if y >= self.height || x + width > self.width {
            return;
        }
        let offset = self.offset(x, y);
        self.pixels[offset..offset + (width as usize * 4)].copy_from_slice(src);
    }

    fn get_pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let offset = self.offset(x, y);
        self.pixels[offset..offset + 4].try_into().unwrap()
    }

    fn set_pixel(&mut self, x: u32, y: u32, pixel: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = self.offset(x, y);
        self.pixels[offset..offset + 4].copy_from_slice(&pixel);
    }

    fn flush(&mut self, rect: &Rect) {
        self.flushed.push(*rect);
    }
}
