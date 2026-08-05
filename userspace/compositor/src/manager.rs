//! Window management: buffer lifecycle, damage tracking and compositing.
//!
//! This is the in-kernel `compositor::WindowManager` moved to userspace
//! (plans/userspace-compositor.md, Phase 3). The compositing algorithm is
//! unchanged — dirty-region coalescing, clear to background, blend
//! back-to-front with an opaque row-copy fast path — but a window's pixels
//! now live in a buffer the *client* allocated and the compositor mapped,
//! rather than in a `Vec<u8>` the kernel owned and clients copied into.

use alloc::vec::Vec;
use compositor_protocol::{Event, FORMAT_BGRA8888, Rect, Request, alpha_blend, is_region_opaque};

use crate::target::Target;

/// Background colour (Nord dark grey), as in the kernel compositor.
pub const BACKGROUND_COLOUR: u32 = 0xFF2E3440;

/// A client buffer mapped into the compositor's address space.
///
/// The compositor holds its own handle to the underlying shared buffer, so
/// the memory stays valid even if the client exits (Phase 1 refcounts the
/// frames by handle); the mapping simply goes stale.
pub struct Attachment {
    /// Per-window attach sequence number, reported by `BufferReleased`.
    pub id: u64,
    data: *mut u8,
    len: usize,
    pub width: u32,
    pub height: u32,
}

impl Attachment {
    /// Wrap a mapped client buffer, rejecting one that is too small for the
    /// geometry it claims or that uses an unsupported format.
    ///
    /// # Safety
    ///
    /// `data` must point to `len` bytes mapped writable into this process
    /// for at least as long as the returned `Attachment` lives.
    pub unsafe fn new(
        id: u64,
        data: *mut u8,
        len: usize,
        width: u32,
        height: u32,
        format: u8,
    ) -> Option<Self> {
        if format != FORMAT_BGRA8888 {
            return None;
        }
        let needed = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
        if needed == 0 || len < needed {
            return None;
        }
        Some(Self {
            id,
            data,
            len,
            width,
            height,
        })
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: `new` is an unsafe constructor whose contract is that
        // `data`/`len` describe a live mapping for this object's lifetime.
        unsafe { core::slice::from_raw_parts(self.data, self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as `as_slice`, and `&mut self` gives exclusive access.
        unsafe { core::slice::from_raw_parts_mut(self.data, self.len) }
    }
}

/// A window tracked by the compositor.
pub struct Window {
    pub id: u64,
    pub visible: bool,
    pub position: (u32, u32),
    /// Size of the latched buffer; `(0, 0)` until the first commit.
    pub size: (u32, u32),
    /// Attached but not yet committed.
    pending: Option<Attachment>,
    /// The buffer being composited.
    latched: Option<Attachment>,
    /// Window-relative damage accumulated since the last commit.
    pending_damage: Vec<Rect>,
    /// A commit is waiting for the tick that consumes it.
    awaiting_frame: bool,
}

impl Window {
    fn rect(&self) -> Rect {
        Rect {
            x: self.position.0,
            y: self.position.1,
            width: self.size.0,
            height: self.size.1,
        }
    }

    fn content_mut(&mut self) -> Option<&mut Attachment> {
        self.pending.as_mut().or(self.latched.as_mut())
    }
}

/// The compositor's window stack and damage state.
pub struct WindowManager<T: Target> {
    windows: Vec<Window>,
    dirty_regions: Vec<Rect>,
    /// `None` when no display could be claimed: the compositor still serves
    /// windows, it just has nowhere to present them (Risk 1 of the plan).
    target: Option<T>,
    next_window_id: u64,
    frame: u64,
}

impl<T: Target> WindowManager<T> {
    pub fn new(target: Option<T>) -> Self {
        let mut manager = Self {
            windows: Vec::new(),
            dirty_regions: Vec::new(),
            target,
            next_window_id: 1,
            frame: 0,
        };

        if let Some(target) = manager.target.as_mut() {
            let screen = Rect {
                x: 0,
                y: 0,
                width: target.width(),
                height: target.height(),
            };
            target.fill(&screen, BACKGROUND_COLOUR);
            target.flush(&screen);
        }

        manager
    }

    /// Screen geometry, or `(0, 0)` when running without a display.
    pub fn screen_size(&self) -> (u32, u32) {
        match self.target.as_ref() {
            Some(target) => (target.width(), target.height()),
            None => (0, 0),
        }
    }

    /// The number of completed frames.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    fn window_mut(&mut self, id: u64) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// Mark a screen-space rectangle as dirty, coalescing it with an
    /// overlapping or touching region if there is one.
    pub fn mark_dirty(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        for existing in &mut self.dirty_regions {
            if existing.intersects(&rect) || existing.is_adjacent(&rect) {
                *existing = existing.union(&rect);
                return;
            }
        }
        self.dirty_regions.push(rect);
    }

    /// Handle one client request.
    ///
    /// `attachment` carries the buffer that came with an `AttachBuffer`
    /// message, already mapped and validated; it is `None` for every other
    /// request (and for an `AttachBuffer` the caller rejected).
    pub fn handle_request(
        &mut self,
        request: Request,
        attachment: Option<Attachment>,
    ) -> Vec<Event<'static>> {
        let mut events = Vec::new();

        match request {
            Request::CreateWindow => {
                let id = self.next_window_id;
                self.next_window_id += 1;
                self.windows.push(Window {
                    id,
                    visible: false,
                    position: (0, 0),
                    size: (0, 0),
                    pending: None,
                    latched: None,
                    pending_damage: Vec::new(),
                    awaiting_frame: false,
                });
                events.push(Event::WindowCreated { window: id });
            }

            Request::AttachBuffer { window, .. } => {
                let Some(attachment) = attachment else {
                    return events;
                };
                if let Some(w) = self.window_mut(window) {
                    // Replacing an uncommitted attachment releases it
                    // immediately: it was never composited from.
                    if let Some(replaced) = w.pending.replace(attachment) {
                        events.push(Event::BufferReleased {
                            window,
                            buffer: replaced.id,
                        });
                    }
                }
            }

            Request::Damage { window, rect } => {
                if let Some(w) = self.window_mut(window) {
                    w.pending_damage.push(rect);
                }
            }

            Request::Commit { window } => {
                let Some(w) = self.window_mut(window) else {
                    return events;
                };
                if w.pending.is_none() && w.latched.is_none() {
                    // A commit with nothing attached has nothing to show.
                    return events;
                }

                if let Some(pending) = w.pending.take() {
                    w.size = (pending.width, pending.height);
                    if let Some(released) = w.latched.replace(pending) {
                        events.push(Event::BufferReleased {
                            window,
                            buffer: released.id,
                        });
                    }
                }

                w.awaiting_frame = true;

                let damage: Vec<Rect> = w.pending_damage.drain(..).collect();
                let (origin_x, origin_y) = w.position;
                let visible = w.visible;
                let window_rect = w.rect();

                if visible {
                    if damage.is_empty() {
                        self.mark_dirty(window_rect);
                    } else {
                        for rect in damage {
                            self.mark_dirty(Rect {
                                x: origin_x + rect.x,
                                y: origin_y + rect.y,
                                width: rect.width,
                                height: rect.height,
                            });
                        }
                    }
                }
            }

            Request::Fill {
                window,
                rect,
                colour,
            } => {
                if let Some(w) = self.window_mut(window) {
                    if let Some(content) = w.content_mut() {
                        fill_buffer(content, &rect, colour);
                        w.pending_damage.push(rect);
                    }
                }
            }

            Request::SetVisible { window, visible } => {
                if let Some(w) = self.window_mut(window) {
                    if w.visible != visible {
                        w.visible = visible;
                        let rect = w.rect();
                        self.mark_dirty(rect);
                    }
                }
            }

            Request::Move { window, x, y } => {
                if let Some(w) = self.window_mut(window) {
                    let old = w.rect();
                    w.position = (x, y);
                    let new = w.rect();
                    if w.visible {
                        self.mark_dirty(old);
                        self.mark_dirty(new);
                    }
                }
            }

            Request::DestroyWindow { window } => {
                let Some(index) = self.windows.iter().position(|w| w.id == window) else {
                    return events;
                };
                let removed = self.windows.remove(index);
                for buffer in [removed.pending.as_ref(), removed.latched.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    events.push(Event::BufferReleased {
                        window,
                        buffer: buffer.id,
                    });
                }
                events.push(Event::Closed { window });
                if removed.visible && !removed.rect().is_empty() {
                    let rect = removed.rect();
                    self.mark_dirty(rect);
                }
            }
        }

        events
    }

    /// Run one compositor tick: composite every dirty region, present it,
    /// and report the commits the tick consumed.
    pub fn tick(&mut self) -> Vec<Event<'static>> {
        self.composite();
        self.frame += 1;

        let frame = self.frame;
        let mut events = Vec::new();
        for window in &mut self.windows {
            if window.awaiting_frame {
                window.awaiting_frame = false;
                events.push(Event::FrameDone {
                    window: window.id,
                    frame,
                });
            }
        }
        events
    }

    fn composite(&mut self) {
        if self.dirty_regions.is_empty() {
            return;
        }

        let Some(target) = self.target.as_mut() else {
            // No display: drop the damage rather than accumulating it
            // forever. Whatever a client draws while headless is simply
            // never presented.
            self.dirty_regions.clear();
            return;
        };

        let screen = Rect {
            x: 0,
            y: 0,
            width: target.width(),
            height: target.height(),
        };

        for i in 0..self.dirty_regions.len() {
            let Some(dirty_rect) = self.dirty_regions[i].intersection(&screen) else {
                continue;
            };

            target.fill(&dirty_rect, BACKGROUND_COLOUR);

            for window in &self.windows {
                if !window.visible || window.size.0 == 0 || window.size.1 == 0 {
                    continue;
                }

                let Some(buffer) = window.latched.as_ref() else {
                    continue;
                };

                let Some(clip_rect) = window.rect().intersection(&dirty_rect) else {
                    continue;
                };

                composite_window(
                    target,
                    window.position,
                    window.size,
                    buffer.as_slice(),
                    &clip_rect,
                );
            }

            target.flush(&dirty_rect);
        }

        self.dirty_regions.clear();
    }
}

fn fill_buffer(buffer: &mut Attachment, rect: &Rect, colour: u32) {
    let stride = buffer.width;
    let bounds = Rect {
        x: 0,
        y: 0,
        width: buffer.width,
        height: buffer.height,
    };
    let Some(rect) = rect.intersection(&bounds) else {
        return;
    };
    let bytes = colour.to_le_bytes();
    let data = buffer.as_mut_slice();
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            let offset = ((y * stride + x) * 4) as usize;
            if offset + 4 > data.len() {
                continue;
            }
            data[offset..offset + 4].copy_from_slice(&bytes);
        }
    }
}

fn composite_window<T: Target>(
    target: &mut T,
    window_pos: (u32, u32),
    window_size: (u32, u32),
    buffer: &[u8],
    clip_rect: &Rect,
) {
    let src_x = clip_rect.x.saturating_sub(window_pos.0);
    let src_y = clip_rect.y.saturating_sub(window_pos.1);

    let opaque = is_region_opaque(
        buffer,
        src_x,
        src_y,
        clip_rect.width,
        clip_rect.height,
        window_size.0,
    );

    if opaque {
        let row_bytes = clip_rect.width as usize * 4;
        for y in 0..clip_rect.height {
            let src_row_start = ((src_y + y) * window_size.0 + src_x) as usize * 4;
            let src_row_end = src_row_start + row_bytes;
            if src_row_end > buffer.len() {
                continue;
            }
            target.write_row(
                clip_rect.x,
                clip_rect.y + y,
                clip_rect.width,
                &buffer[src_row_start..src_row_end],
            );
        }
    } else {
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

                if src_pixel[3] == 0 {
                    continue;
                }

                let dst_x = clip_rect.x + x;
                let dst_y = clip_rect.y + y;

                let dst_pixel = target.get_pixel(dst_x, dst_y);
                target.set_pixel(dst_x, dst_y, alpha_blend(src_pixel, dst_pixel));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::MemoryTarget;
    use alloc::vec;

    /// Backing store for a client buffer, kept alive for the duration of a
    /// test the way a real client's shared buffer is kept alive by the
    /// compositor's handle.
    struct ClientBuffer {
        pixels: Vec<u8>,
        width: u32,
        height: u32,
    }

    impl ClientBuffer {
        fn new(width: u32, height: u32, pixel: [u8; 4]) -> Self {
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            for chunk in pixels.chunks_exact_mut(4) {
                chunk.copy_from_slice(&pixel);
            }
            Self {
                pixels,
                width,
                height,
            }
        }

        fn attach(&mut self, id: u64) -> Attachment {
            // SAFETY: the buffer outlives every Attachment made from it —
            // each test drops the manager before the ClientBuffer.
            unsafe {
                Attachment::new(
                    id,
                    self.pixels.as_mut_ptr(),
                    self.pixels.len(),
                    self.width,
                    self.height,
                    FORMAT_BGRA8888,
                )
            }
            .expect("valid attachment")
        }
    }

    fn manager(width: u32, height: u32) -> WindowManager<MemoryTarget> {
        WindowManager::new(Some(MemoryTarget::new(width, height)))
    }

    fn create_window<T: Target>(manager: &mut WindowManager<T>) -> u64 {
        match manager.handle_request(Request::CreateWindow, None)[..] {
            [Event::WindowCreated { window }] => window,
            ref other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn dirty_regions_coalesce_when_overlapping_or_adjacent() {
        let mut manager = manager(100, 100);
        manager.mark_dirty(Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        });
        manager.mark_dirty(Rect {
            x: 10,
            y: 0,
            width: 10,
            height: 10,
        });
        assert_eq!(manager.dirty_regions.len(), 1);
        assert_eq!(manager.dirty_regions[0].width, 20);

        manager.mark_dirty(Rect {
            x: 50,
            y: 50,
            width: 10,
            height: 10,
        });
        assert_eq!(manager.dirty_regions.len(), 2);
    }

    #[test]
    fn empty_damage_is_ignored() {
        let mut manager = manager(100, 100);
        manager.mark_dirty(Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 10,
        });
        assert!(manager.dirty_regions.is_empty());
    }

    #[test]
    fn an_opaque_window_is_copied_over_the_background() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut client = ClientBuffer::new(16, 16, [10, 20, 30, 255]);

        manager.handle_request(
            Request::AttachBuffer {
                window,
                width: 16,
                height: 16,
                format: FORMAT_BGRA8888,
            },
            Some(client.attach(0)),
        );
        manager.handle_request(
            Request::SetVisible {
                window,
                visible: true,
            },
            None,
        );
        manager.handle_request(
            Request::Move {
                window,
                x: 8,
                y: 4,
            },
            None,
        );
        manager.handle_request(Request::Commit { window }, None);
        manager.tick();

        let target = manager.target.as_ref().unwrap();
        assert_eq!(target.get_pixel(8, 4), [10, 20, 30, 255]);
        assert_eq!(target.get_pixel(23, 19), [10, 20, 30, 255]);
        // Just outside the window: still background.
        assert_eq!(
            target.get_pixel(24, 20),
            BACKGROUND_COLOUR.to_le_bytes()
        );
    }

    #[test]
    fn a_translucent_window_is_blended_with_the_background() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut client = ClientBuffer::new(8, 8, [0, 0, 0, 128]);

        manager.handle_request(
            Request::AttachBuffer {
                window,
                width: 8,
                height: 8,
                format: FORMAT_BGRA8888,
            },
            Some(client.attach(0)),
        );
        manager.handle_request(
            Request::SetVisible {
                window,
                visible: true,
            },
            None,
        );
        manager.handle_request(Request::Commit { window }, None);
        manager.tick();

        let background = BACKGROUND_COLOUR.to_le_bytes();
        let expected = alpha_blend([0, 0, 0, 128], background);
        assert_eq!(manager.target.as_ref().unwrap().get_pixel(0, 0), expected);
        assert_ne!(expected, background);
    }

    #[test]
    fn only_committed_damage_is_composited_and_flushed() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut client = ClientBuffer::new(16, 16, [1, 2, 3, 255]);

        manager.handle_request(
            Request::AttachBuffer {
                window,
                width: 16,
                height: 16,
                format: FORMAT_BGRA8888,
            },
            Some(client.attach(0)),
        );
        manager.handle_request(
            Request::SetVisible {
                window,
                visible: true,
            },
            None,
        );
        manager.handle_request(Request::Commit { window }, None);
        manager.tick();
        manager.target.as_mut().unwrap().flushed.clear();

        // Damage alone must not present anything.
        manager.handle_request(
            Request::Damage {
                window,
                rect: Rect {
                    x: 2,
                    y: 2,
                    width: 4,
                    height: 4,
                },
            },
            None,
        );
        manager.tick();
        assert!(manager.target.as_ref().unwrap().flushed.is_empty());

        manager.handle_request(Request::Commit { window }, None);
        manager.tick();
        assert_eq!(
            manager.target.as_ref().unwrap().flushed,
            vec![Rect {
                x: 2,
                y: 2,
                width: 4,
                height: 4
            }]
        );
    }

    #[test]
    fn a_commit_is_reported_by_the_tick_that_consumes_it() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut client = ClientBuffer::new(4, 4, [1, 1, 1, 255]);

        manager.handle_request(
            Request::AttachBuffer {
                window,
                width: 4,
                height: 4,
                format: FORMAT_BGRA8888,
            },
            Some(client.attach(0)),
        );
        assert!(manager.tick().is_empty());

        manager.handle_request(Request::Commit { window }, None);
        assert_eq!(
            manager.tick(),
            vec![Event::FrameDone { window, frame: 2 }]
        );
        assert!(manager.tick().is_empty());
    }

    #[test]
    fn latching_a_new_buffer_releases_the_previous_one() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut first = ClientBuffer::new(4, 4, [1, 1, 1, 255]);
        let mut second = ClientBuffer::new(4, 4, [2, 2, 2, 255]);

        let attach = Request::AttachBuffer {
            window,
            width: 4,
            height: 4,
            format: FORMAT_BGRA8888,
        };

        manager.handle_request(attach, Some(first.attach(0)));
        assert!(manager.handle_request(Request::Commit { window }, None).is_empty());

        manager.handle_request(attach, Some(second.attach(1)));
        // Still nothing released: the compositor is reading buffer 0 until
        // the commit that latches buffer 1.
        assert_eq!(
            manager.handle_request(Request::Commit { window }, None),
            vec![Event::BufferReleased { window, buffer: 0 }]
        );
    }

    #[test]
    fn replacing_an_uncommitted_buffer_releases_it_immediately() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut first = ClientBuffer::new(4, 4, [1, 1, 1, 255]);
        let mut second = ClientBuffer::new(4, 4, [2, 2, 2, 255]);

        let attach = Request::AttachBuffer {
            window,
            width: 4,
            height: 4,
            format: FORMAT_BGRA8888,
        };
        manager.handle_request(attach, Some(first.attach(0)));
        assert_eq!(
            manager.handle_request(attach, Some(second.attach(1))),
            vec![Event::BufferReleased { window, buffer: 0 }]
        );
    }

    #[test]
    fn destroying_a_window_releases_its_buffers_and_repaints() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut client = ClientBuffer::new(8, 8, [5, 5, 5, 255]);

        manager.handle_request(
            Request::AttachBuffer {
                window,
                width: 8,
                height: 8,
                format: FORMAT_BGRA8888,
            },
            Some(client.attach(0)),
        );
        manager.handle_request(
            Request::SetVisible {
                window,
                visible: true,
            },
            None,
        );
        manager.handle_request(Request::Commit { window }, None);
        manager.tick();

        assert_eq!(
            manager.handle_request(Request::DestroyWindow { window }, None),
            vec![
                Event::BufferReleased { window, buffer: 0 },
                Event::Closed { window }
            ]
        );
        manager.tick();
        assert_eq!(
            manager.target.as_ref().unwrap().get_pixel(0, 0),
            BACKGROUND_COLOUR.to_le_bytes()
        );
    }

    #[test]
    fn moving_a_window_repaints_both_the_old_and_new_positions() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut client = ClientBuffer::new(8, 8, [7, 7, 7, 255]);

        manager.handle_request(
            Request::AttachBuffer {
                window,
                width: 8,
                height: 8,
                format: FORMAT_BGRA8888,
            },
            Some(client.attach(0)),
        );
        manager.handle_request(
            Request::SetVisible {
                window,
                visible: true,
            },
            None,
        );
        manager.handle_request(Request::Commit { window }, None);
        manager.tick();

        manager.handle_request(
            Request::Move {
                window,
                x: 32,
                y: 32,
            },
            None,
        );
        manager.tick();

        let target = manager.target.as_ref().unwrap();
        assert_eq!(target.get_pixel(0, 0), BACKGROUND_COLOUR.to_le_bytes());
        assert_eq!(target.get_pixel(32, 32), [7, 7, 7, 255]);
    }

    #[test]
    fn fill_writes_into_the_client_buffer() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        let mut client = ClientBuffer::new(8, 8, [0, 0, 0, 255]);

        manager.handle_request(
            Request::AttachBuffer {
                window,
                width: 8,
                height: 8,
                format: FORMAT_BGRA8888,
            },
            Some(client.attach(0)),
        );
        manager.handle_request(
            Request::SetVisible {
                window,
                visible: true,
            },
            None,
        );
        manager.handle_request(
            Request::Fill {
                window,
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 4,
                },
                colour: 0xFF804020,
            },
            None,
        );
        manager.handle_request(
            Request::Damage {
                window,
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
            },
            None,
        );
        manager.handle_request(Request::Commit { window }, None);
        manager.tick();

        let target = manager.target.as_ref().unwrap();
        assert_eq!(target.get_pixel(0, 0), 0xFF804020u32.to_le_bytes());
        // Outside the filled rectangle the client's own pixels survive.
        assert_eq!(target.get_pixel(5, 5), [0, 0, 0, 255]);
    }

    #[test]
    fn a_buffer_too_small_for_its_geometry_is_rejected() {
        let mut pixels = vec![0u8; 4 * 4 * 4];
        // SAFETY: the pointer and length describe `pixels`, which outlives
        // this call.
        let attachment = unsafe {
            Attachment::new(
                0,
                pixels.as_mut_ptr(),
                pixels.len(),
                8,
                8,
                FORMAT_BGRA8888,
            )
        };
        assert!(attachment.is_none());
    }

    #[test]
    fn an_unsupported_format_is_rejected() {
        let mut pixels = vec![0u8; 4 * 4 * 4];
        // SAFETY: as above.
        let attachment =
            unsafe { Attachment::new(0, pixels.as_mut_ptr(), pixels.len(), 4, 4, 99) };
        assert!(attachment.is_none());
    }

    #[test]
    fn a_commit_with_no_attached_buffer_does_nothing() {
        let mut manager = manager(64, 64);
        let window = create_window(&mut manager);
        assert!(manager.handle_request(Request::Commit { window }, None).is_empty());
        assert!(manager.tick().is_empty());
    }

    #[test]
    fn requests_for_an_unknown_window_are_ignored() {
        let mut manager = manager(64, 64);
        assert!(manager.handle_request(Request::Commit { window: 99 }, None).is_empty());
        assert!(
            manager
                .handle_request(Request::DestroyWindow { window: 99 }, None)
                .is_empty()
        );
    }

    #[test]
    fn without_a_display_windows_are_still_served() {
        let mut manager: WindowManager<MemoryTarget> = WindowManager::new(None);
        let window = create_window(&mut manager);
        let mut client = ClientBuffer::new(4, 4, [1, 1, 1, 255]);

        manager.handle_request(
            Request::AttachBuffer {
                window,
                width: 4,
                height: 4,
                format: FORMAT_BGRA8888,
            },
            Some(client.attach(0)),
        );
        manager.handle_request(
            Request::SetVisible {
                window,
                visible: true,
            },
            None,
        );
        manager.handle_request(Request::Commit { window }, None);

        assert_eq!(
            manager.tick(),
            vec![Event::FrameDone { window, frame: 1 }]
        );
        assert_eq!(manager.screen_size(), (0, 0));
    }
}
