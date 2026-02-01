//! Window compositor for multi-window display management.
//!
//! The compositor uses a single-owner model where only the compositor task
//! calls composite(). Flush syscalls mark regions dirty and block until the
//! next compositor tick completes. The compositor task runs at ~60fps and
//! processes all dirty regions on each tick.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::Waker;
use spinning_top::Spinlock;

use crate::resource::{FramebufferSurface, Rect, SharedBuffer, Surface, alpha_blend};

/// Background color (Nord dark gray)
const BACKGROUND_COLOR: u32 = 0xFF2E3440;

/// Refresh interval in milliseconds (~60fps)
const REFRESH_INTERVAL_MS: u64 = 16;

/// Global compositor instance
static COMPOSITOR: Spinlock<Option<WindowManager>> = Spinlock::new(None);

/// Frame counter - incremented after each compositor tick completes
static FRAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Wakers waiting for the next frame to complete
static FRAME_WAITERS: Spinlock<Vec<Waker>> = Spinlock::new(Vec::new());

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

        let start_ms = crate::time::uptime_ms();
        let mut total_pixels: u64 = 0;

        let fb_width = self.framebuffer.width();
        let fb_height = self.framebuffer.height();
        let fb_bounds = Rect {
            x: 0,
            y: 0,
            width: fb_width,
            height: fb_height,
        };

        // Process each dirty region
        for i in 0..self.dirty_regions.len() {
            // Clip dirty region to framebuffer bounds
            let Some(dirty_rect) = self.dirty_regions[i].intersection(&fb_bounds) else {
                continue;
            };

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

                total_pixels += clip_rect.width as u64 * clip_rect.height as u64;

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

        let elapsed_ms = crate::time::uptime_ms().saturating_sub(start_ms);
        let num_regions = self.dirty_regions.len();
        self.dirty_regions.clear();

        // Log composite time for performance measurement (only when non-trivial)
        if total_pixels > 0 {
            log::debug!(
                "composite: {}px across {} region(s) in {}ms",
                total_pixels,
                num_regions,
                elapsed_ms,
            );
        }
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

        // Check if the clipped region is fully opaque — if so, use row memcpy
        let opaque = Self::is_region_opaque(buffer, src_x, src_y, clip_rect.width, clip_rect.height, window_size.0);

        if opaque {
            // Fast path: copy entire rows directly into the framebuffer
            let row_bytes = clip_rect.width as usize * 4;
            for y in 0..clip_rect.height {
                let src_row_start = ((src_y + y) * window_size.0 + src_x) as usize * 4;
                let src_row_end = src_row_start + row_bytes;
                if src_row_end > buffer.len() {
                    continue;
                }
                framebuffer.write_row(
                    clip_rect.x,
                    clip_rect.y + y,
                    clip_rect.width,
                    &buffer[src_row_start..src_row_end],
                );
            }
        } else {
            // Slow path: per-pixel alpha blending
            for y in 0..clip_rect.height {
                for x in 0..clip_rect.width {
                    let src_offset = (((src_y + y) * window_size.0 + (src_x + x)) * 4) as usize;

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

                    let dst_pixel = framebuffer.get_pixel(dst_x, dst_y);
                    let blended = alpha_blend(src_pixel, dst_pixel);
                    framebuffer.set_pixel(dst_x, dst_y, blended);
                }
            }
        }
    }

    /// Check if all pixels in a rectangular region have alpha == 255.
    /// Bails early on the first non-opaque pixel.
    fn is_region_opaque(
        data: &[u8],
        src_x: u32,
        src_y: u32,
        width: u32,
        height: u32,
        stride: u32,
    ) -> bool {
        for row in 0..height {
            let row_start = ((src_y + row) * stride + src_x) as usize * 4;
            for col in 0..width as usize {
                match data.get(row_start + col * 4 + 3) {
                    Some(&255) => {}
                    _ => return false,
                }
            }
        }
        true
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

/// Replace the compositor's framebuffer at runtime (e.g. after a resolution change).
///
/// Clears the new framebuffer, replaces the old one, discards stale dirty regions,
/// and marks the entire screen dirty to force a full repaint.
pub fn replace_framebuffer(new_framebuffer: FramebufferSurface) {
    let mut compositor = COMPOSITOR.lock();
    let compositor = compositor.as_mut().expect("Compositor not initialized");

    let width = new_framebuffer.width();
    let height = new_framebuffer.height();
    new_framebuffer
        .fill(0, 0, width, height, BACKGROUND_COLOR)
        .ok();

    compositor.framebuffer = new_framebuffer;
    compositor.dirty_regions.clear();
    compositor.mark_dirty(Rect {
        x: 0,
        y: 0,
        width,
        height,
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

/// Future that waits for the next compositor frame to complete.
pub struct WaitForNextFrame {
    start_frame: u64,
    registered: bool,
}

impl WaitForNextFrame {
    fn new() -> Self {
        Self {
            start_frame: FRAME_COUNTER.load(Ordering::Acquire),
            registered: false,
        }
    }
}

impl core::future::Future for WaitForNextFrame {
    type Output = ();

    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        // Check if a frame has completed since we started waiting
        if FRAME_COUNTER.load(Ordering::Acquire) > self.start_frame {
            return core::task::Poll::Ready(());
        }

        // Register waker if not already registered
        if !self.registered {
            FRAME_WAITERS.lock().push(cx.waker().clone());
            self.registered = true;
        }

        core::task::Poll::Pending
    }
}

/// Wait until the compositor has completed the next frame.
pub fn wait_for_next_frame() -> WaitForNextFrame {
    WaitForNextFrame::new()
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
        {
            let mut compositor = COMPOSITOR.lock();
            if let Some(compositor) = compositor.as_mut() {
                compositor.composite();
            }
        }

        // Increment frame counter to signal waiters that this tick is complete
        FRAME_COUNTER.fetch_add(1, Ordering::Release);

        // Wake all waiters - they'll check the frame counter and complete
        let waiters: Vec<Waker> = FRAME_WAITERS.lock().drain(..).collect();
        for waker in waiters {
            waker.wake();
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
