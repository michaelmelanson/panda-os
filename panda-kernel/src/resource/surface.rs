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

impl Rect {
    /// Check if two rectangles intersect
    pub fn intersects(&self, other: &Rect) -> bool {
        !(self.x >= other.x + other.width
            || self.x + self.width <= other.x
            || self.y >= other.y + other.height
            || self.y + self.height <= other.y)
    }

    /// Calculate intersection of two rectangles
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

    /// Calculate union of two rectangles
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

    /// Check if rectangles are adjacent (touching)
    pub fn is_adjacent(&self, other: &Rect) -> bool {
        let h_adjacent = (self.x + self.width == other.x || other.x + other.width == self.x)
            && !(self.y >= other.y + other.height || other.y >= self.y + self.height);

        let v_adjacent = (self.y + self.height == other.y || other.y + other.height == self.y)
            && !(self.x >= other.x + other.width || other.x >= self.x + self.width);

        h_adjacent || v_adjacent
    }

    /// Check if a point is contained in the rectangle
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
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
/// Uses interior mutability via raw pointers.
pub trait Surface: Send + Sync {
    /// Get surface dimensions and pixel format.
    fn info(&self) -> SurfaceInfo;

    /// Blit pixels to surface (copy rectangle from pixel buffer).
    /// Uses interior mutability.
    fn blit(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<(), SurfaceError>;

    /// Fill rectangle with solid color.
    /// Uses interior mutability.
    fn fill(&self, x: u32, y: u32, width: u32, height: u32, color: u32)
    -> Result<(), SurfaceError>;

    /// Flush updates to display (for double-buffering).
    /// Uses interior mutability.
    fn flush(&self, region: Option<Rect>) -> Result<(), SurfaceError>;
}

/// Blend source pixel over destination using alpha compositing.
/// Both src and dst are in BGRA format (little-endian ARGB8888).
/// Returns the blended pixel.
pub fn alpha_blend(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let src_alpha = src[3] as u32;

    if src_alpha == 255 {
        return src; // Opaque
    }

    if src_alpha == 0 {
        return dst; // Transparent
    }

    let inv_alpha = 255 - src_alpha;

    [
        ((src[0] as u32 * src_alpha + dst[0] as u32 * inv_alpha) / 255) as u8,
        ((src[1] as u32 * src_alpha + dst[1] as u32 * inv_alpha) / 255) as u8,
        ((src[2] as u32 * src_alpha + dst[2] as u32 * inv_alpha) / 255) as u8,
        255, // Result is opaque (composited onto background)
    ]
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

    /// Get a pixel at the specified coordinates.
    /// Returns pixel in BGRA format (little-endian ARGB8888).
    pub fn get_pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.info.width || y >= self.info.height {
            return [0, 0, 0, 0];
        }
        unsafe {
            let offset = (y * self.info.stride + x * 4) as isize;
            let ptr = self.framebuffer.offset(offset);
            [*ptr, *ptr.offset(1), *ptr.offset(2), *ptr.offset(3)]
        }
    }

    /// Set a pixel at the specified coordinates.
    /// Pixel should be in BGRA format (little-endian ARGB8888).
    pub fn set_pixel(&mut self, x: u32, y: u32, pixel: [u8; 4]) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        unsafe {
            let offset = (y * self.info.stride + x * 4) as isize;
            let ptr = self.framebuffer.offset(offset);
            *ptr = pixel[0]; // B
            *ptr.offset(1) = pixel[1]; // G
            *ptr.offset(2) = pixel[2]; // R
            *ptr.offset(3) = pixel[3]; // A
        }
    }

    /// Get framebuffer width
    pub fn width(&self) -> u32 {
        self.info.width
    }

    /// Get framebuffer height
    pub fn height(&self) -> u32 {
        self.info.height
    }
}

impl Surface for FramebufferSurface {
    fn info(&self) -> SurfaceInfo {
        self.info
    }

    fn blit(
        &self,
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

        // Blit with alpha blending
        unsafe {
            for row in 0..height {
                for col in 0..width {
                    let dst_y = y + row;
                    let dst_x = x + col;
                    let dst_offset = (dst_y * self.info.stride + dst_x * bytes_per_pixel) as isize;
                    let dst_ptr = self.framebuffer.offset(dst_offset);

                    let src_idx = ((row * width + col) * bytes_per_pixel) as usize;

                    // Read source pixel (BGRA byte order for little-endian ARGB8888)
                    let src_b = pixels[src_idx] as u32;
                    let src_g = pixels[src_idx + 1] as u32;
                    let src_r = pixels[src_idx + 2] as u32;
                    let src_a = pixels[src_idx + 3] as u32;

                    if src_a == 0 {
                        // Fully transparent, skip
                        continue;
                    } else if src_a == 255 {
                        // Fully opaque, direct write - framebuffer is [B,G,R,A]
                        *dst_ptr = src_b as u8;
                        *dst_ptr.offset(1) = src_g as u8;
                        *dst_ptr.offset(2) = src_r as u8;
                        *dst_ptr.offset(3) = src_a as u8;
                    } else {
                        // Alpha blend: dst = src * alpha + dst * (1 - alpha)
                        let dst_b = *dst_ptr as u32;
                        let dst_g = *dst_ptr.offset(1) as u32;
                        let dst_r = *dst_ptr.offset(2) as u32;

                        let inv_alpha = 255 - src_a;

                        let final_b = ((src_b * src_a + dst_b * inv_alpha) / 255) as u8;
                        let final_g = ((src_g * src_a + dst_g * inv_alpha) / 255) as u8;
                        let final_r = ((src_r * src_a + dst_r * inv_alpha) / 255) as u8;

                        *dst_ptr = final_b;
                        *dst_ptr.offset(1) = final_g;
                        *dst_ptr.offset(2) = final_r;
                        *dst_ptr.offset(3) = 255; // Keep dest alpha at full opacity
                    }
                }
            }
        }

        Ok(())
    }

    fn fill(
        &self,
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
        // Color is already in ARGB format, which becomes BGRA in little-endian memory
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

    fn flush(&self, _region: Option<Rect>) -> Result<(), SurfaceError> {
        // Tell virtio-gpu to update the display
        crate::devices::virtio_gpu::flush_framebuffer();
        Ok(())
    }
}

impl Resource for FramebufferSurface {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Surface
    }

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
