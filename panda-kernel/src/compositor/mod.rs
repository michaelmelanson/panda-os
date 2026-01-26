//! Window compositor for multi-window display management.
//!
//! The compositor uses a single-owner model where only the compositor task
//! calls composite(). Flush syscalls just mark regions dirty and return
//! immediately. The compositor task runs at ~60fps and processes all dirty
//! regions on each tick.

use alloc::sync::Arc;
use alloc::vec::Vec;
use spinning_top::Spinlock;

use crate::resource::{FramebufferSurface, Rect, SharedBuffer, Surface, alpha_blend};

/// Background color (Nord dark gray)
const BACKGROUND_COLOR: u32 = 0xFF2E3440;

/// Refresh interval in milliseconds (~60fps)
const REFRESH_INTERVAL_MS: u64 = 16;

/// Global compositor instance
static COMPOSITOR: Spinlock<Option<WindowManager>> = Spinlock::new(None);

/// Window manager state
pub struct WindowManager {
    windows: Vec<Arc<Spinlock<Window>>>,
    dirty_regions: Vec<Rect>,
    framebuffer: FramebufferSurface,
    next_window_id: u64,
}

/// A window in the compositor
pub struct Window {
    pub id: u64,
    pub visible: bool,
    pub position: (u32, u32),
    pub size: (u32, u32),
    pub content_buffer: Option<Arc<SharedBuffer>>,
    pub pixel_data: Vec<u8>, // Own persistent pixel buffer (BGRA format)
}

impl WindowManager {
    /// Mark a screen-space rectangle as dirty
    pub fn mark_dirty(&mut self, rect: Rect) {
        // Coalesce with existing dirty regions
        for existing in &mut self.dirty_regions {
            if existing.intersects(&rect) || existing.is_adjacent(&rect) {
                *existing = existing.union(&rect);
                return;
            }
        }
        self.dirty_regions.push(rect);
    }

    /// Composite all dirty regions to the framebuffer.
    /// This should ONLY be called from the compositor task.
    fn composite(&mut self) {
        if self.dirty_regions.is_empty() {
            return;
        }

        // Process each dirty region
        for i in 0..self.dirty_regions.len() {
            let dirty_rect = self.dirty_regions[i];

            // Clear region to background
            self.clear_region(&dirty_rect, BACKGROUND_COLOR);

            // Composite windows back-to-front with alpha blending
            for window_arc in &self.windows {
                let window = window_arc.lock();

                if !window.visible || window.size.0 == 0 || window.size.1 == 0 {
                    continue;
                }

                // Skip if window has no pixel data
                if window.pixel_data.is_empty() {
                    continue;
                }

                let window_rect = Rect {
                    x: window.position.0,
                    y: window.position.1,
                    width: window.size.0,
                    height: window.size.1,
                };

                // Skip if no intersection
                let Some(clip_rect) = window_rect.intersection(&dirty_rect) else {
                    continue;
                };

                // Composite clipped region with alpha blending
                Self::composite_window(
                    &mut self.framebuffer,
                    window.position,
                    window.size,
                    &window.pixel_data,
                    &clip_rect,
                );
            }

            // Flush this dirty region to display
            self.framebuffer.flush(Some(dirty_rect)).ok();
        }

        self.dirty_regions.clear();
    }

    fn clear_region(&mut self, rect: &Rect, color: u32) {
        self.framebuffer
            .fill(rect.x, rect.y, rect.width, rect.height, color)
            .ok();
    }

    fn composite_window(
        framebuffer: &mut FramebufferSurface,
        window_pos: (u32, u32),
        window_size: (u32, u32),
        buffer: &[u8],
        clip_rect: &Rect,
    ) {
        // Calculate source offset within window
        let src_x = clip_rect.x.saturating_sub(window_pos.0);
        let src_y = clip_rect.y.saturating_sub(window_pos.1);

        // Composite pixel by pixel with alpha blending
        for y in 0..clip_rect.height {
            for x in 0..clip_rect.width {
                let src_offset = (((src_y + y) * window_size.0 + (src_x + x)) * 4) as usize;

                // Bounds check
                if src_offset + 4 > buffer.len() {
                    continue;
                }

                let src_pixel = [
                    buffer[src_offset],
                    buffer[src_offset + 1],
                    buffer[src_offset + 2],
                    buffer[src_offset + 3],
                ];

                // Skip fully transparent pixels
                if src_pixel[3] == 0 {
                    continue;
                }

                let dst_x = clip_rect.x + x;
                let dst_y = clip_rect.y + y;

                // Get destination pixel
                let dst_pixel = framebuffer.get_pixel(dst_x, dst_y);

                // Blend and write back
                let blended = alpha_blend(src_pixel, dst_pixel);
                framebuffer.set_pixel(dst_x, dst_y, blended);
            }
        }
    }
}

/// Initialize the compositor with the framebuffer
pub fn init(framebuffer: FramebufferSurface) {
    // Clear entire framebuffer to background color
    let width = framebuffer.width();
    let height = framebuffer.height();
    framebuffer.fill(0, 0, width, height, BACKGROUND_COLOR).ok();
    framebuffer.flush(None).ok();

    let mut compositor = COMPOSITOR.lock();
    *compositor = Some(WindowManager {
        windows: Vec::new(),
        dirty_regions: Vec::new(),
        framebuffer,
        next_window_id: 1,
    });
}

/// Create a new window
pub fn create_window() -> Arc<Spinlock<Window>> {
    let mut compositor = COMPOSITOR.lock();
    let compositor = compositor.as_mut().expect("Compositor not initialized");

    let id = compositor.next_window_id;
    compositor.next_window_id += 1;

    let window = Arc::new(Spinlock::new(Window {
        id,
        visible: false,
        position: (0, 0),
        size: (0, 0),
        content_buffer: None,
        pixel_data: Vec::new(), // Will be allocated when size is set
    }));

    // Add to top of stack
    compositor.windows.push(window.clone());
    window
}

/// Destroy a window
pub fn destroy_window(window_id: u64) {
    let mut compositor = COMPOSITOR.lock();
    let compositor = compositor.as_mut().expect("Compositor not initialized");

    if let Some(pos) = compositor
        .windows
        .iter()
        .position(|w| w.lock().id == window_id)
    {
        let dirty_rect = {
            let window = &compositor.windows[pos];
            let w = window.lock();

            // Calculate dirty rect if visible
            if w.visible && w.size.0 > 0 && w.size.1 > 0 {
                Some(Rect {
                    x: w.position.0,
                    y: w.position.1,
                    width: w.size.0,
                    height: w.size.1,
                })
            } else {
                None
            }
        };

        // Remove window
        compositor.windows.remove(pos);

        // Mark dirty after window is removed
        if let Some(rect) = dirty_rect {
            compositor.mark_dirty(rect);
        }
    }
}

/// Mark entire window dirty (screen-space)
pub fn mark_window_dirty(window_id: u64) {
    let mut compositor = COMPOSITOR.lock();
    let compositor = compositor.as_mut().expect("Compositor not initialized");

    if let Some(window_arc) = compositor.windows.iter().find(|w| w.lock().id == window_id) {
        let rect = {
            let w = window_arc.lock();
            if !w.visible || w.size.0 == 0 || w.size.1 == 0 {
                return;
            }
            Rect {
                x: w.position.0,
                y: w.position.1,
                width: w.size.0,
                height: w.size.1,
            }
        };
        compositor.mark_dirty(rect);
    }
}

/// Mark window-relative region dirty (translates to screen-space)
pub fn mark_window_region_dirty(window_id: u64, region: Rect) {
    let mut compositor = COMPOSITOR.lock();
    let compositor = compositor.as_mut().expect("Compositor not initialized");

    if let Some(window_arc) = compositor.windows.iter().find(|w| w.lock().id == window_id) {
        let rect = {
            let w = window_arc.lock();
            if !w.visible {
                return;
            }
            Rect {
                x: w.position.0 + region.x,
                y: w.position.1 + region.y,
                width: region.width,
                height: region.height,
            }
        };
        compositor.mark_dirty(rect);
    }
}

/// Mark screen-space rect dirty directly
pub fn mark_dirty_direct(rect: Rect) {
    let mut compositor = COMPOSITOR.lock();
    let compositor = compositor.as_mut().expect("Compositor not initialized");
    compositor.mark_dirty(rect);
}

/// Called from flush syscall - just marks dirty, compositor task does the work.
/// This function returns immediately; actual compositing happens on next tick.
pub fn force_composite() {
    // No-op: dirty regions are already marked by the caller.
    // The compositor task will process them on its next tick.
}

/// Compositor async task - the ONLY place that calls composite().
///
/// This task runs at ~60fps and processes all dirty regions each tick.
/// Flush syscalls just mark regions dirty and return immediately.
async fn compositor_task() {
    use crate::executor::sleep::sleep_ms;

    log::info!("Compositor task started");

    loop {
        // Sleep until next frame
        sleep_ms(REFRESH_INTERVAL_MS).await;

        // Composite any dirty regions
        let mut compositor = COMPOSITOR.lock();
        if let Some(compositor) = compositor.as_mut() {
            compositor.composite();
        }
    }
}

/// Spawn the compositor task
pub fn spawn_compositor_task() {
    crate::executor::spawn(compositor_task());
}

/// Check if the compositor has been initialized
pub fn is_initialized() -> bool {
    COMPOSITOR.lock().is_some()
}
