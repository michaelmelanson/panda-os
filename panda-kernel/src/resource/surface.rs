//! Surface resource for display/framebuffer operations.

use alloc::boxed::Box;
use spinning_top::RwSpinlock;

use super::Resource;

/// Pixel format for surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PixelFormat {
    /// 32-bit ARGB (alpha, red, green, blue)
    ARGB8888 = 0,
}

/// Rectangle for surface operations.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Surface information.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceInfo {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub stride: u32, // bytes per row
}

/// Errors that can occur during surface operations.
#[derive(Debug, Clone, Copy)]
pub enum SurfaceError {
    /// Invalid coordinates or dimensions
    InvalidBounds,
    /// Pixel data size mismatch
    InvalidDataSize,
    /// Unsupported operation
    Unsupported,
}

/// Surface trait for display/framebuffer operations.
pub trait Surface: Send + Sync {
    /// Get surface dimensions and pixel format.
    fn info(&self) -> SurfaceInfo;

    /// Blit pixels to surface (copy rectangle from pixel buffer).
    fn blit(&mut self, x: u32, y: u32, width: u32, height: u32, pixels: &[u8])
        -> Result<(), SurfaceError>;

    /// Fill rectangle with solid color.
    fn fill(&mut self, x: u32, y: u32, width: u32, height: u32, color: u32)
        -> Result<(), SurfaceError>;

    /// Flush updates to display (for double-buffering).
    fn flush(&mut self, region: Option<Rect>) -> Result<(), SurfaceError>;
}

/// Framebuffer surface backed by virtio-gpu or similar.
pub struct FramebufferSurface {
    framebuffer: *mut u8,
    info: SurfaceInfo,
}

// Safety: We control access to the framebuffer through the resource system.
unsafe impl Send for FramebufferSurface {}
unsafe impl Sync for FramebufferSurface {}

impl FramebufferSurface {
    /// Create a new framebuffer surface.
    ///
    /// # Safety
    /// The caller must ensure that `framebuffer` points to valid, writeable memory
    /// of at least `stride * height` bytes.
    pub unsafe fn new(framebuffer: *mut u8, width: u32, height: u32, format: PixelFormat) -> Self {
        let stride = match format {
            PixelFormat::ARGB8888 => width * 4,
        };

        Self {
            framebuffer,
            info: SurfaceInfo {
                width,
                height,
                format,
                stride,
            },
        }
    }

    /// Check if coordinates and dimensions are within bounds.
    fn check_bounds(&self, x: u32, y: u32, width: u32, height: u32) -> Result<(), SurfaceError> {
        if x >= self.info.width || y >= self.info.height {
            return Err(SurfaceError::InvalidBounds);
        }

        if x.checked_add(width).is_none() || y.checked_add(height).is_none() {
            return Err(SurfaceError::InvalidBounds);
        }

        if x + width > self.info.width || y + height > self.info.height {
            return Err(SurfaceError::InvalidBounds);
        }

        Ok(())
    }
}

impl Surface for FramebufferSurface {
    fn info(&self) -> SurfaceInfo {
        self.info
    }

    fn blit(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<(), SurfaceError> {
        self.check_bounds(x, y, width, height)?;

        let bytes_per_pixel = match self.info.format {
            PixelFormat::ARGB8888 => 4,
        };

        let expected_size = (width * height * bytes_per_pixel) as usize;
        if pixels.len() < expected_size {
            return Err(SurfaceError::InvalidDataSize);
        }

        // Copy pixels row by row
        unsafe {
            for row in 0..height {
                let dst_y = y + row;
                let dst_offset = (dst_y * self.info.stride + x * bytes_per_pixel) as isize;
                let dst_ptr = self.framebuffer.offset(dst_offset);

                let src_offset = (row * width * bytes_per_pixel) as usize;
                let row_bytes = (width * bytes_per_pixel) as usize;
                let src_slice = &pixels[src_offset..src_offset + row_bytes];

                core::ptr::copy_nonoverlapping(src_slice.as_ptr(), dst_ptr, row_bytes);
            }
        }

        Ok(())
    }

    fn fill(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        color: u32,
    ) -> Result<(), SurfaceError> {
        self.check_bounds(x, y, width, height)?;

        let bytes_per_pixel = match self.info.format {
            PixelFormat::ARGB8888 => 4,
        };

        // Fill each row with the color
        unsafe {
            for row in 0..height {
                let dst_y = y + row;
                let dst_offset = (dst_y * self.info.stride + x * bytes_per_pixel) as isize;
                let mut dst_ptr = self.framebuffer.offset(dst_offset) as *mut u32;

                for _ in 0..width {
                    *dst_ptr = color;
                    dst_ptr = dst_ptr.offset(1);
                }
            }
        }

        Ok(())
    }

    fn flush(&mut self, _region: Option<Rect>) -> Result<(), SurfaceError> {
        // Tell virtio-gpu to update the display
        crate::devices::virtio_gpu::flush_framebuffer();
        Ok(())
    }
}

impl Resource for FramebufferSurface {
    fn as_surface(&self) -> Option<&dyn Surface> {
        Some(self)
    }

    fn as_surface_mut(&mut self) -> Option<&mut dyn Surface> {
        Some(self)
    }
}

/// Global framebuffer surface.
static FRAMEBUFFER_SURFACE: RwSpinlock<Option<Box<FramebufferSurface>>> = RwSpinlock::new(None);

/// Initialize the framebuffer surface.
///
/// # Safety
/// The caller must ensure that `framebuffer` points to valid, writeable memory
/// of at least `stride * height` bytes.
pub unsafe fn init_framebuffer(framebuffer: *mut u8, width: u32, height: u32) {
    let surface = unsafe {
        Box::new(FramebufferSurface::new(
            framebuffer,
            width,
            height,
            PixelFormat::ARGB8888,
        ))
    };

    let mut global = FRAMEBUFFER_SURFACE.write();
    *global = Some(surface);
}

/// Get a clone of the framebuffer surface for handle creation.
pub fn get_framebuffer_surface() -> Option<Box<FramebufferSurface>> {
    let global = FRAMEBUFFER_SURFACE.read();
    global.as_ref().map(|surface| {
        // Create a new surface pointing to the same framebuffer
        unsafe {
            Box::new(FramebufferSurface::new(
                surface.framebuffer,
                surface.info.width,
                surface.info.height,
                surface.info.format,
            ))
        }
    })
}
