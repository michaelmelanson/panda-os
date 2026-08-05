//! The compositing target: the framebuffer mapped from the `display:`
//! scheme.

use compositor_protocol::Rect;
use libpanda::{Handle, environment, sys};
use panda_abi::{ErrorCode, SurfaceInfoOut, SurfaceRect};

use crate::target::Target;

/// The exclusively-claimed display the compositor presents to.
pub const DISPLAY_PATH: &str = "display:/pci/display/0";

/// A mapped framebuffer.
pub struct Framebuffer {
    handle: Handle,
    pixels: *mut u8,
    width: u32,
    height: u32,
    stride: u32,
}

impl Framebuffer {
    /// Claim the display, query its mode and map its framebuffer.
    ///
    /// `Busy` here is expected until the in-kernel compositor is deleted
    /// (Phase 5 of plans/userspace-compositor.md): it holds a permanent
    /// claim on the same display.
    pub fn open() -> Result<Self, ErrorCode> {
        let handle = environment::open(DISPLAY_PATH, 0, 0)?;

        let mut info = SurfaceInfoOut {
            width: 0,
            height: 0,
            format: 0,
            stride: 0,
        };
        if sys::display::info(handle, &mut info) < 0 {
            let _ = sys::file::close(handle);
            return Err(ErrorCode::IoError);
        }

        let mapped = sys::display::map(handle);
        if mapped < 0 {
            let _ = sys::file::close(handle);
            return Err(ErrorCode::IoError);
        }

        Ok(Self {
            handle,
            pixels: mapped as *mut u8,
            width: info.width,
            height: info.height,
            stride: info.stride,
        })
    }

    fn offset(&self, x: u32, y: u32) -> isize {
        (y * self.stride + x * 4) as isize
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        let _ = sys::file::close(self.handle);
    }
}

impl Target for Framebuffer {
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
                // SAFETY: x/y are clipped to the mapped framebuffer.
                unsafe {
                    let ptr = self.pixels.offset(self.offset(x, y));
                    core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, 4);
                }
            }
        }
    }

    fn write_row(&mut self, x: u32, y: u32, width: u32, src: &[u8]) {
        if y >= self.height || x >= self.width {
            return;
        }
        let width = width.min(self.width - x);
        let bytes = width as usize * 4;
        if src.len() < bytes {
            return;
        }
        // SAFETY: the destination row is clipped to the mapped framebuffer
        // and `src` has at least `bytes` bytes.
        unsafe {
            let ptr = self.pixels.offset(self.offset(x, y));
            core::ptr::copy_nonoverlapping(src.as_ptr(), ptr, bytes);
        }
    }

    fn get_pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        // SAFETY: bounds-checked above.
        unsafe {
            let ptr = self.pixels.offset(self.offset(x, y));
            [*ptr, *ptr.offset(1), *ptr.offset(2), *ptr.offset(3)]
        }
    }

    fn set_pixel(&mut self, x: u32, y: u32, pixel: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        // SAFETY: bounds-checked above.
        unsafe {
            let ptr = self.pixels.offset(self.offset(x, y));
            core::ptr::copy_nonoverlapping(pixel.as_ptr(), ptr, 4);
        }
    }

    fn flush(&mut self, rect: &Rect) {
        let rect = SurfaceRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        };
        sys::display::flush(self.handle, Some(&rect));
    }
}
