//! Pixel buffer for graphics rendering.

use crate::error::{Error, Result};
use crate::graphics::{Colour, Rect};
use crate::handle::Handle;
use crate::sys;
use panda_abi::BufferAllocInfo;

/// A pixel buffer for graphics operations.
///
/// PixelBuffer provides a convenient way to create and manipulate pixel data
/// that can be blitted to surfaces. It handles buffer allocation and provides
/// helper methods for drawing operations.
///
/// # Example
/// ```no_run
/// use libpanda::graphics::{Colour, PixelBuffer, Rect, Surface};
///
/// let mut buffer = PixelBuffer::new(100, 100).unwrap();
/// buffer.clear(Colour::BLUE);
/// buffer.fill_rect(Rect::new(10, 10, 20, 20), Colour::RED);
/// let mut surface = Surface::open("surface:/pci/display/0").unwrap();
/// surface.blit(&buffer, 0, 0).unwrap();
/// ```
pub struct PixelBuffer {
    handle: Handle,
    ptr: *mut u32,
    width: u32,
    height: u32,
}

impl PixelBuffer {
    /// Create a new pixel buffer with the given dimensions.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let size = (width as usize) * (height as usize) * 4;
        let mut info = BufferAllocInfo { addr: 0, size: 0 };

        let result = sys::buffer::alloc(size, Some(&mut info));
        if result < 0 {
            return Err(Error::from_code(result));
        }

        Ok(Self {
            handle: Handle::from(result as u64),
            ptr: info.addr as *mut u32,
            width,
            height,
        })
    }

    /// Get the buffer width in pixels.
    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Get the buffer height in pixels.
    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Get the underlying buffer handle (for blitting).
    #[inline]
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Get the pixel data as a slice.
    pub fn pixels(&self) -> &[u32] {
        unsafe { core::slice::from_raw_parts(self.ptr, (self.width * self.height) as usize) }
    }

    /// Get the pixel data as a mutable slice.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, (self.width * self.height) as usize) }
    }

    /// Get the raw bytes of the buffer.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self.ptr as *const u8,
                (self.width * self.height * 4) as usize,
            )
        }
    }

    /// Get the raw bytes of the buffer mutably.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self.ptr as *mut u8,
                (self.width * self.height * 4) as usize,
            )
        }
    }

    /// Clear the entire buffer with a colour.
    pub fn clear(&mut self, colour: Colour) {
        let pixels = self.pixels_mut();
        let value = colour.as_u32();
        for pixel in pixels.iter_mut() {
            *pixel = value;
        }
    }

    /// Set a single pixel.
    ///
    /// Does nothing if the coordinates are out of bounds.
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, colour: Colour) {
        if x < self.width && y < self.height {
            let index = (y * self.width + x) as usize;
            unsafe {
                *self.ptr.add(index) = colour.as_u32();
            }
        }
    }

    /// Get a single pixel.
    ///
    /// Returns transparent black if coordinates are out of bounds.
    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> Colour {
        if x < self.width && y < self.height {
            let index = (y * self.width + x) as usize;
            unsafe { Colour(*self.ptr.add(index)) }
        } else {
            Colour::TRANSPARENT
        }
    }

    /// Fill a rectangle with a colour.
    pub fn fill_rect(&mut self, rect: Rect, colour: Colour) {
        let value = colour.as_u32();

        // Clip to buffer bounds
        let x_start = rect.x.min(self.width);
        let y_start = rect.y.min(self.height);
        let x_end = rect.right().min(self.width);
        let y_end = rect.bottom().min(self.height);

        for y in y_start..y_end {
            let row_start = (y * self.width + x_start) as usize;
            let row_end = (y * self.width + x_end) as usize;
            unsafe {
                for i in row_start..row_end {
                    *self.ptr.add(i) = value;
                }
            }
        }
    }

    /// Draw a horizontal line.
    pub fn draw_hline(&mut self, x: u32, y: u32, length: u32, colour: Colour) {
        if y >= self.height {
            return;
        }
        let x_end = (x + length).min(self.width);
        let x_start = x.min(self.width);
        let value = colour.as_u32();

        for xi in x_start..x_end {
            let index = (y * self.width + xi) as usize;
            unsafe {
                *self.ptr.add(index) = value;
            }
        }
    }

    /// Draw a vertical line.
    pub fn draw_vline(&mut self, x: u32, y: u32, length: u32, colour: Colour) {
        if x >= self.width {
            return;
        }
        let y_end = (y + length).min(self.height);
        let y_start = y.min(self.height);
        let value = colour.as_u32();

        for yi in y_start..y_end {
            let index = (yi * self.width + x) as usize;
            unsafe {
                *self.ptr.add(index) = value;
            }
        }
    }

    /// Draw a rectangle outline.
    pub fn draw_rect(&mut self, rect: Rect, colour: Colour) {
        // Top
        self.draw_hline(rect.x, rect.y, rect.width, colour);
        // Bottom
        if rect.height > 0 {
            self.draw_hline(rect.x, rect.y + rect.height - 1, rect.width, colour);
        }
        // Left
        self.draw_vline(rect.x, rect.y, rect.height, colour);
        // Right
        if rect.width > 0 {
            self.draw_vline(rect.x + rect.width - 1, rect.y, rect.height, colour);
        }
    }

    /// Blend a pixel with alpha compositing.
    ///
    /// Uses source-over compositing: result = src + dst * (1 - src_alpha)
    pub fn blend_pixel(&mut self, x: u32, y: u32, colour: Colour) {
        if x >= self.width || y >= self.height {
            return;
        }

        let src_a = colour.a() as u32;
        if src_a == 255 {
            // Fully opaque, just set
            self.set_pixel(x, y, colour);
            return;
        }
        if src_a == 0 {
            // Fully transparent, do nothing
            return;
        }

        let dst = self.get_pixel(x, y);
        let inv_a = 255 - src_a;

        let r = ((colour.r() as u32 * src_a + dst.r() as u32 * inv_a) / 255) as u8;
        let g = ((colour.g() as u32 * src_a + dst.g() as u32 * inv_a) / 255) as u8;
        let b = ((colour.b() as u32 * src_a + dst.b() as u32 * inv_a) / 255) as u8;
        let a = ((src_a * 255 + dst.a() as u32 * inv_a) / 255) as u8;

        self.set_pixel(x, y, Colour::rgba(r, g, b, a));
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        let _ = sys::buffer::free(self.handle);
    }
}
