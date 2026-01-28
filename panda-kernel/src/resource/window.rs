//! Window resource for compositor-managed windows.

use alloc::sync::Arc;
use spinning_top::Spinlock;

use crate::compositor::Window;

use super::{PixelFormat, Resource, Surface, SurfaceInfo};

/// Resource representing a compositor window
pub struct WindowResource {
    pub(crate) window: Arc<Spinlock<Window>>,
}

impl Resource for WindowResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Surface
    }

    fn as_surface(&self) -> Option<&dyn Surface> {
        Some(self)
    }

    fn as_surface_mut(&mut self) -> Option<&mut dyn Surface> {
        Some(self)
    }

    fn as_window(&self) -> Option<Arc<Spinlock<crate::compositor::Window>>> {
        Some(self.window.clone())
    }
}

impl Surface for WindowResource {
    fn info(&self) -> SurfaceInfo {
        let w = self.window.lock();
        SurfaceInfo {
            width: w.size.0,
            height: w.size.1,
            format: PixelFormat::ARGB8888,
            stride: w.size.0 * 4,
        }
    }

    fn blit(
        &self,
        _x: u32,
        _y: u32,
        _width: u32,
        _height: u32,
        _pixels: &[u8],
    ) -> Result<(), super::SurfaceError> {
        // Blit is handled via handle_blit syscall which stores buffer directly
        // This method is not used for windows
        Err(super::SurfaceError::Unsupported)
    }

    fn fill(
        &self,
        _x: u32,
        _y: u32,
        _width: u32,
        _height: u32,
        _color: u32,
    ) -> Result<(), super::SurfaceError> {
        // Fill is not supported on windows (userspace fills buffer)
        Err(super::SurfaceError::Unsupported)
    }

    fn flush(&self, _region: Option<super::Rect>) -> Result<(), super::SurfaceError> {
        // Flush is handled via handle_flush syscall
        // This method is not used for windows
        Err(super::SurfaceError::Unsupported)
    }
}

impl Drop for WindowResource {
    fn drop(&mut self) {
        let window_id = self.window.lock().id;
        crate::compositor::destroy_window(window_id);
    }
}
