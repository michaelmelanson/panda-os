//! Surface and window abstractions.

use crate::error::{self, Result};
use crate::graphics::{Colour, PixelBuffer, Rect};
use crate::handle::Handle;
use crate::sys;
use panda_abi::{BlitParams, FillParams, SurfaceRect, UpdateParamsIn};

/// Information about a surface.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Stride (bytes per row).
    pub stride: u32,
}

/// A graphics surface for rendering.
///
/// Surfaces can be standalone (for direct framebuffer access) or wrapped
/// in a `Window` for windowed rendering.
pub struct Surface {
    handle: Handle,
}

impl Surface {
    /// Open a surface by device path.
    ///
    /// # Example
    /// ```no_run
    /// use libpanda::graphics::Surface;
    ///
    /// let surface = Surface::open("surface:/pci/display/0").unwrap();
    /// ```
    pub fn open(path: &str) -> Result<Self> {
        let result = sys::env::open(path, 0, 0);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(Self {
                handle: Handle::from(result as u64),
            })
        }
    }

    /// Create a Surface from an existing handle.
    pub fn from_handle(handle: Handle) -> Self {
        Self { handle }
    }

    /// Get the underlying handle.
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Get surface information (dimensions, stride).
    pub fn info(&self) -> Result<SurfaceInfo> {
        let mut info = panda_abi::SurfaceInfoOut {
            width: 0,
            height: 0,
            format: 0,
            stride: 0,
        };
        let result = sys::surface::info(self.handle, &mut info);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(SurfaceInfo {
                width: info.width,
                height: info.height,
                stride: info.stride,
            })
        }
    }

    /// Fill a rectangle with a solid colour.
    ///
    /// # Example
    /// ```no_run
    /// use libpanda::graphics::{Colour, Rect, Surface};
    ///
    /// let mut surface = Surface::open("surface:/pci/display/0").unwrap();
    /// surface.fill(Rect::new(10, 10, 100, 100), Colour::RED).unwrap();
    /// ```
    pub fn fill(&mut self, rect: Rect, colour: Colour) -> Result<()> {
        let params = FillParams {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            colour: colour.as_u32(),
        };
        let result = sys::surface::fill(self.handle, &params);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Fill the entire surface with a solid colour.
    pub fn clear(&mut self, colour: Colour) -> Result<()> {
        let info = self.info()?;
        self.fill(Rect::from_size(info.width, info.height), colour)
    }

    /// Blit a pixel buffer to the surface.
    ///
    /// # Example
    /// ```no_run
    /// use libpanda::graphics::{Colour, PixelBuffer, Surface};
    ///
    /// let mut surface = Surface::open("surface:/pci/display/0").unwrap();
    /// let mut buffer = PixelBuffer::new(100, 100).unwrap();
    /// buffer.clear(Colour::BLUE);
    /// surface.blit(&buffer, 10, 10).unwrap();
    /// ```
    pub fn blit(&mut self, buffer: &PixelBuffer, x: u32, y: u32) -> Result<()> {
        let params = BlitParams {
            x,
            y,
            width: buffer.width(),
            height: buffer.height(),
            buffer_handle: buffer.handle().as_raw(),
            src_x: 0,
            src_y: 0,
            src_stride: 0,
        };
        let result = sys::surface::blit(self.handle, &params);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Flush surface updates to the display.
    ///
    /// Call this after drawing operations to make them visible.
    pub fn flush(&mut self) -> Result<()> {
        let result = sys::surface::flush(self.handle, None);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Flush a specific region of the surface.
    pub fn flush_rect(&mut self, rect: Rect) -> Result<()> {
        let surface_rect = SurfaceRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        };
        let result = sys::surface::flush(self.handle, Some(&surface_rect));
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Consume the surface and return the underlying handle.
    pub fn into_handle(self) -> Handle {
        let handle = self.handle;
        core::mem::forget(self);
        handle
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        let _ = sys::file::close(self.handle);
    }
}

/// A window with position and visibility control.
///
/// Windows are surfaces with additional positioning and visibility
/// management capabilities.
pub struct Window {
    surface: Surface,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    visible: bool,
}

impl Window {
    /// Create a new window with the given dimensions.
    ///
    /// # Example
    /// ```no_run
    /// use libpanda::graphics::Window;
    ///
    /// let window = Window::new(800, 600).unwrap();
    /// ```
    pub fn new(width: u32, height: u32) -> Result<Self> {
        Self::builder().size(width, height).build()
    }

    /// Create a window builder for more options.
    pub fn builder() -> WindowBuilder {
        WindowBuilder::new()
    }

    /// Get the underlying surface.
    pub fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Get a mutable reference to the underlying surface.
    pub fn surface_mut(&mut self) -> &mut Surface {
        &mut self.surface
    }

    /// Get the window position.
    pub fn position(&self) -> (u32, u32) {
        (self.x, self.y)
    }

    /// Get the window size.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Check if the window is visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Set the window position.
    pub fn set_position(&mut self, x: u32, y: u32) -> Result<()> {
        self.x = x;
        self.y = y;
        self.update_params()
    }

    /// Set the window size.
    pub fn set_size(&mut self, width: u32, height: u32) -> Result<()> {
        self.width = width;
        self.height = height;
        self.update_params()
    }

    /// Set the window visibility.
    pub fn set_visible(&mut self, visible: bool) -> Result<()> {
        self.visible = visible;
        self.update_params()
    }

    /// Show the window.
    pub fn show(&mut self) -> Result<()> {
        self.set_visible(true)
    }

    /// Hide the window.
    pub fn hide(&mut self) -> Result<()> {
        self.set_visible(false)
    }

    fn update_params(&mut self) -> Result<()> {
        let params = UpdateParamsIn {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            visible: if self.visible { 1 } else { 0 },
        };
        let result = sys::surface::update_params(self.surface.handle, &params);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(())
        }
    }

    // Delegate surface methods

    /// Get surface information.
    pub fn info(&self) -> Result<SurfaceInfo> {
        self.surface.info()
    }

    /// Fill a rectangle with a solid colour.
    pub fn fill(&mut self, rect: Rect, colour: Colour) -> Result<()> {
        self.surface.fill(rect, colour)
    }

    /// Fill the entire window with a solid colour.
    pub fn clear(&mut self, colour: Colour) -> Result<()> {
        self.surface.clear(colour)
    }

    /// Blit a pixel buffer to the window.
    pub fn blit(&mut self, buffer: &PixelBuffer, x: u32, y: u32) -> Result<()> {
        self.surface.blit(buffer, x, y)
    }

    /// Flush window updates to the display.
    pub fn flush(&mut self) -> Result<()> {
        self.surface.flush()
    }

    /// Flush a specific region of the window.
    pub fn flush_rect(&mut self, rect: Rect) -> Result<()> {
        self.surface.flush_rect(rect)
    }
}

/// Builder for creating windows with custom options.
pub struct WindowBuilder {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    visible: bool,
    path: Option<&'static str>,
}

impl WindowBuilder {
    /// Create a new window builder with default options.
    pub fn new() -> Self {
        Self {
            width: 640,
            height: 480,
            x: 0,
            y: 0,
            visible: true,
            path: None,
        }
    }

    /// Set the window size.
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set the window position.
    pub fn position(mut self, x: u32, y: u32) -> Self {
        self.x = x;
        self.y = y;
        self
    }

    /// Set whether the window is initially visible.
    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Set a custom surface path.
    pub fn path(mut self, path: &'static str) -> Self {
        self.path = Some(path);
        self
    }

    /// Build the window.
    pub fn build(self) -> Result<Window> {
        let path = self.path.unwrap_or("surface:/window");
        let surface = Surface::open(path)?;

        let mut window = Window {
            surface,
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            visible: self.visible,
        };

        // Apply initial parameters
        window.update_params()?;

        Ok(window)
    }
}

impl Default for WindowBuilder {
    fn default() -> Self {
        Self::new()
    }
}
