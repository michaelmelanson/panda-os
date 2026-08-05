//! The compositor client: `Window`, reimplemented over
//! `compositor_protocol` (plans/userspace-compositor.md, "Client library").
//!
//! A `Window` opens a channel to the compositor by connecting to the
//! `compositor:` scheme it registers on startup (`environment::connect`,
//! see `panda_abi::scheme_protocol::Request::Connect` and
//! `compositor::server::Compositor::serve_connects`) — `init` spawns the
//! compositor and its graphical clients as independent siblings, so a
//! client can't rely on `Channel::parent()` to reach it. It then allocates
//! one or two shared buffers and speaks `Request`/`Event` frames over the
//! channel. `blit` copies into the buffer currently being drawn into and
//! tracks damage; `flush` sends the accumulated `Damage` and a `Commit`,
//! then waits for `FrameDone`.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use compositor_protocol::{Event, FORMAT_BGRA8888, MAX_FRAME_SIZE, Request};

use crate::buffer::Buffer;
use crate::error::Result;
use crate::graphics::{Colour, PixelBuffer, Rect};
use crate::ipc::Channel;
use panda_abi::ErrorCode;

/// An event the connection read but that didn't match what the caller was
/// waiting for, kept around for a later wait to consume.
///
/// Owned (not borrowing from a receive buffer) so it can outlive the frame
/// it was decoded from and sit in a shared queue.
#[derive(Clone, Copy)]
enum PendingEvent {
    WindowCreated { window: u64 },
    FrameDone { window: u64, frame: u64 },
    BufferReleased { window: u64, buffer: u64 },
    Closed { window: u64 },
}

impl PendingEvent {
    fn from_event(event: Event<'_>) -> Option<Self> {
        match event {
            Event::DisplayFormats { .. } => None,
            Event::WindowCreated { window } => Some(Self::WindowCreated { window }),
            Event::FrameDone { window, frame } => Some(Self::FrameDone { window, frame }),
            Event::BufferReleased { window, buffer } => {
                Some(Self::BufferReleased { window, buffer })
            }
            Event::Closed { window } => Some(Self::Closed { window }),
        }
    }
}

/// The shared connection to the compositor: one channel, fanned out to
/// every `Window` this process has open.
///
/// Requests are answered in order (message.rs's doc comment), but with
/// several windows sharing one channel, an event read while waiting for
/// *this* window's reply may belong to another window (or to a different
/// kind of event for the same window) — those get stashed in `pending`
/// rather than dropped.
struct Connection {
    channel: Channel,
    pending: Vec<PendingEvent>,
    screen: (u32, u32),
}

/// How many times [`Connection::open`] retries `environment::connect`
/// before giving up.
///
/// `init` spawns the compositor and its graphical clients as independent
/// siblings (see the module doc comment), so there's no ordering guarantee
/// that the compositor has registered the `compositor:` scheme by the time
/// a client tries to connect — a bare `NotFound` on the first attempt is
/// expected, not exceptional. Retrying with a yield between attempts covers
/// that startup race without needing an explicit ready signal between two
/// processes that otherwise have no channel to send one over.
const CONNECT_RETRIES: u32 = 200;

impl Connection {
    fn open() -> Result<Self> {
        let mut attempt = 0;
        let channel = loop {
            match crate::environment::connect("compositor:/connect") {
                Ok(handle) => break Channel::from_handle(handle).ok_or(ErrorCode::Protocol)?,
                Err(ErrorCode::NotFound) if attempt < CONNECT_RETRIES => {
                    attempt += 1;
                    crate::process::yield_now();
                }
                Err(e) => return Err(e),
            }
        };
        Self::from_channel(channel)
    }

    /// As [`Connection::open`], but over an explicit channel rather than
    /// connecting to the `compositor:` scheme.
    ///
    /// Production code always reaches the compositor by scheme discovery
    /// (see the module doc comment), but a test that spawns the compositor
    /// itself (`compositor::server::run` bounded by a tick count, the same
    /// pattern `compositor_start_test` uses) is the compositor's *parent*,
    /// not a sibling reachable by scheme, so it must hand in the channel it
    /// already has (`Channel::parent()`, from the compositor's point of
    /// view) explicitly — see [`WindowBuilder::channel`].
    fn from_channel(channel: Channel) -> Result<Self> {
        let mut frame = [0u8; MAX_FRAME_SIZE];
        let len = channel.recv(&mut frame)?;
        let screen = match Event::decode(&frame[..len]) {
            Some(Event::DisplayFormats { width, height, .. }) => (width, height),
            _ => return Err(ErrorCode::Protocol),
        };

        Ok(Self {
            channel,
            pending: Vec::new(),
            screen,
        })
    }

    fn send(&self, request: Request) -> Result<()> {
        let mut frame = [0u8; MAX_FRAME_SIZE];
        let len = request.encode(&mut frame).ok_or(ErrorCode::InvalidArgument)?;
        self.channel.send(&frame[..len])
    }

    fn send_with_handle(&self, request: Request, handle: crate::Handle) -> Result<()> {
        let mut frame = [0u8; MAX_FRAME_SIZE];
        let len = request.encode(&mut frame).ok_or(ErrorCode::InvalidArgument)?;
        self.channel.send_with_handle(&frame[..len], handle)
    }

    /// Block until an event matching `matches` arrives, checking already
    /// -queued events first.
    fn wait_for<F: Fn(&PendingEvent) -> bool>(&mut self, matches: F) -> Result<PendingEvent> {
        if let Some(index) = self.pending.iter().position(|event| matches(event)) {
            return Ok(self.pending.remove(index));
        }

        let mut frame = [0u8; MAX_FRAME_SIZE];
        loop {
            let len = self.channel.recv(&mut frame)?;
            let Some(event) = Event::decode(&frame[..len]) else {
                continue;
            };
            let Some(event) = PendingEvent::from_event(event) else {
                continue;
            };
            if matches(&event) {
                return Ok(event);
            }
            self.pending.push(event);
        }
    }

    /// Drain any queued events without blocking, checking for a `Closed`
    /// belonging to `window`.
    fn poll_closed(&mut self, window: u64) -> bool {
        let mut frame = [0u8; MAX_FRAME_SIZE];
        while let Ok(Some(len)) = self.channel.try_recv(&mut frame) {
            let Some(event) = Event::decode(&frame[..len]) else {
                continue;
            };
            if let Some(event) = PendingEvent::from_event(event) {
                self.pending.push(event);
            }
        }
        self.pending
            .iter()
            .any(|event| matches!(event, PendingEvent::Closed { window: w } if *w == window))
    }
}

/// One shared buffer a `Window` can draw into and attach.
struct Slot {
    buffer: Buffer,
    width: u32,
    height: u32,
    /// The attach sequence number the compositor will report back in
    /// `BufferReleased`, once this slot has been attached at least once.
    attach_id: Option<u64>,
    /// `true` once the compositor has confirmed it is done reading this
    /// slot's current content (or it has never been attached, so there is
    /// nothing to wait for).
    released: bool,
}

impl Slot {
    fn new(width: u32, height: u32) -> Result<Self> {
        let size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(ErrorCode::InvalidArgument)?;
        let buffer = Buffer::alloc(size).ok_or(ErrorCode::IoError)?;
        Ok(Self {
            buffer,
            width,
            height,
            attach_id: None,
            released: true,
        })
    }
}

/// A compositor window.
///
/// Draw into it with [`Window::blit`] or [`Window::fill`], then call
/// [`Window::flush`] to commit the accumulated damage. By default a window
/// has one buffer, reused every frame (today's semantics: a frame may be
/// latched while still being drawn into by the next one). Call
/// [`WindowBuilder::double_buffered`] for tear-free rendering: `blit`/`fill`
/// then wait for the *other* buffer to be released before reusing it.
pub struct Window {
    connection: Rc<RefCell<Connection>>,
    id: u64,
    x: u32,
    y: u32,
    slots: Vec<Slot>,
    /// Index into `slots` currently being drawn into.
    current: usize,
    visible: bool,
    damage: Vec<compositor_protocol::Rect>,
}

fn to_protocol_rect(rect: Rect) -> compositor_protocol::Rect {
    compositor_protocol::Rect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

impl Window {
    /// Create a window with the given size (single-buffered, hidden until
    /// shown).
    pub fn new(width: u32, height: u32) -> Result<Self> {
        Self::builder().size(width, height).build()
    }

    /// A window builder for more options.
    pub fn builder() -> WindowBuilder {
        WindowBuilder::new()
    }

    /// The window's position.
    pub fn position(&self) -> (u32, u32) {
        (self.x, self.y)
    }

    /// The window's current buffer size.
    pub fn size(&self) -> (u32, u32) {
        let slot = &self.slots[self.current];
        (slot.width, slot.height)
    }

    /// Whether the window is visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Whether the compositor has reported this window `Closed` (e.g. the
    /// compositor process is shutting down). Polls for pending events
    /// without blocking.
    pub fn is_closed(&mut self) -> bool {
        self.connection.borrow_mut().poll_closed(self.id)
    }

    /// Move the window's top-left corner to a screen position.
    pub fn set_position(&mut self, x: u32, y: u32) -> Result<()> {
        self.x = x;
        self.y = y;
        self.connection
            .borrow()
            .send(Request::Move { window: self.id, x, y })
    }

    /// Resize the window. Takes effect on the next `flush`, which
    /// re-attaches the (now differently sized) buffer.
    pub fn set_size(&mut self, width: u32, height: u32) -> Result<()> {
        for slot in &mut self.slots {
            *slot = Slot::new(width, height)?;
        }
        Ok(())
    }

    /// Show or hide the window.
    pub fn set_visible(&mut self, visible: bool) -> Result<()> {
        self.visible = visible;
        self.connection.borrow().send(Request::SetVisible {
            window: self.id,
            visible,
        })
    }

    /// Show the window.
    pub fn show(&mut self) -> Result<()> {
        self.set_visible(true)
    }

    /// Hide the window.
    pub fn hide(&mut self) -> Result<()> {
        self.set_visible(false)
    }

    /// Fill a rectangle of the window with a solid colour.
    ///
    /// Handled compositor-side against the latched content (the plan's
    /// "Fill is handled compositor-side"), so it works even before any
    /// buffer has been attached.
    pub fn fill(&mut self, rect: Rect, colour: Colour) -> Result<()> {
        self.connection.borrow().send(Request::Fill {
            window: self.id,
            rect: to_protocol_rect(rect),
            colour: colour.as_u32(),
        })
    }

    /// Fill the entire window with a solid colour.
    pub fn clear(&mut self, colour: Colour) -> Result<()> {
        let (width, height) = self.size();
        self.fill(Rect::from_size(width, height), colour)
    }

    /// Blit a pixel buffer into the window's current draw buffer at
    /// `(x, y)`, recording the region as damaged.
    ///
    /// This is a local memcpy — nothing is sent to the compositor until
    /// [`Window::flush`].
    pub fn blit(&mut self, buffer: &PixelBuffer, x: u32, y: u32) -> Result<()> {
        let full = Rect::from_size(buffer.width(), buffer.height());
        self.blit_region(buffer, x, y, full)
    }

    /// As [`Window::blit`], but copies only `src_rect` of `buffer` (in
    /// `buffer`'s own coordinates) to `(dst_x, dst_y)`.
    ///
    /// Lets a caller with its own persistent pixel buffer — e.g. the
    /// terminal's glyph-composited framebuffer — batch many small edits and
    /// blit just the accumulated dirty rectangle, instead of recopying the
    /// whole buffer on every change.
    pub fn blit_region(
        &mut self,
        buffer: &PixelBuffer,
        dst_x: u32,
        dst_y: u32,
        src_rect: Rect,
    ) -> Result<()> {
        if src_rect.right() > buffer.width() || src_rect.bottom() > buffer.height() {
            return Err(ErrorCode::InvalidArgument);
        }

        let slot = &mut self.slots[self.current];
        let width = src_rect.width;
        let height = src_rect.height;
        if dst_x
            .checked_add(width)
            .map(|e| e > slot.width)
            .unwrap_or(true)
            || dst_y
                .checked_add(height)
                .map(|e| e > slot.height)
                .unwrap_or(true)
        {
            return Err(ErrorCode::InvalidArgument);
        }

        let src = buffer.as_bytes();
        let dst = slot.buffer.as_mut_slice();
        let dst_stride = slot.width as usize * 4;
        let src_stride = buffer.width() as usize * 4;
        let row_bytes = width as usize * 4;
        for row in 0..height as usize {
            let dst_offset = (dst_y as usize + row) * dst_stride + dst_x as usize * 4;
            let src_offset =
                (src_rect.y as usize + row) * src_stride + src_rect.x as usize * 4;
            dst[dst_offset..dst_offset + row_bytes]
                .copy_from_slice(&src[src_offset..src_offset + row_bytes]);
        }

        self.damage
            .push(to_protocol_rect(Rect::new(dst_x, dst_y, width, height)));
        Ok(())
    }

    /// Commit the accumulated damage: (re-)attach the current buffer if
    /// needed, send the damage rects, `Commit`, and wait for `FrameDone`.
    ///
    /// For a double-buffered window this also swaps to the other buffer for
    /// subsequent drawing, blocking until the compositor has released it if
    /// it is still in flight.
    pub fn flush(&mut self) -> Result<()> {
        self.attach_current_if_needed()?;

        {
            let connection = self.connection.borrow();
            for rect in self.damage.drain(..) {
                connection.send(Request::Damage {
                    window: self.id,
                    rect,
                })?;
            }
            connection.send(Request::Commit { window: self.id })?;
        }

        let window = self.id;
        let frame_event = self
            .connection
            .borrow_mut()
            .wait_for(|event| matches!(event, PendingEvent::FrameDone { window: w, .. } if *w == window))?;
        let PendingEvent::FrameDone { frame, .. } = frame_event else {
            unreachable!()
        };
        let _ = frame;

        // A single-buffered window re-attaches the same slot every flush,
        // which makes the compositor release the *previous* attachment of
        // that slot (manager.rs: committing a new pending attachment
        // releases whatever was latched). Nothing needs to happen for that
        // release — the slot is reused unconditionally — so just drop it
        // rather than let it sit in `pending` forever.
        if self.slots.len() == 1 {
            self.connection.borrow_mut().pending.retain(|event| {
                !matches!(event, PendingEvent::BufferReleased { window: w, .. } if *w == window)
            });
        }

        if self.slots.len() > 1 {
            self.current = (self.current + 1) % self.slots.len();
            self.wait_for_current_release()?;
        }

        Ok(())
    }

    /// Flush a single rectangle (a convenience over recording one damage
    /// rect and flushing).
    pub fn flush_rect(&mut self, rect: Rect) -> Result<()> {
        self.damage.push(to_protocol_rect(rect));
        self.flush()
    }

    /// Send `AttachBuffer` for the buffer about to be committed, tracking
    /// the attach sequence number `BufferReleased` will report back
    /// (message.rs: "the compositor names each attachment by a per-window
    /// sequence number, starting at 0").
    ///
    /// `AttachBuffer` has no direct reply — the next event this window
    /// waits for is the `FrameDone`/`BufferReleased` pair the following
    /// `Commit` produces — so this only sends, it doesn't block.
    fn attach_current_if_needed(&mut self) -> Result<()> {
        let slot = &mut self.slots[self.current];
        let handle = slot.buffer.handle();
        let (width, height) = (slot.width, slot.height);
        let next_id = slot.attach_id.map(|id| id + 1).unwrap_or(0);

        self.connection.borrow().send_with_handle(
            Request::AttachBuffer {
                window: self.id,
                width,
                height,
                format: FORMAT_BGRA8888,
            },
            handle,
        )?;

        slot.attach_id = Some(next_id);
        slot.released = false;
        Ok(())
    }

    fn wait_for_current_release(&mut self) -> Result<()> {
        let slot = &self.slots[self.current];
        if slot.released {
            return Ok(());
        }
        let Some(attach_id) = slot.attach_id else {
            return Ok(());
        };
        let window = self.id;
        self.connection.borrow_mut().wait_for(|event| {
            matches!(event, PendingEvent::BufferReleased { window: w, buffer } if *w == window && *buffer == attach_id)
        })?;
        self.slots[self.current].released = true;
        Ok(())
    }
}

impl Window {
    /// Create another window over the same connection this window uses.
    ///
    /// A process's windows all share one channel to the compositor (the
    /// module doc comment); this is how a multi-window client — or a test
    /// that connected its first window with [`WindowBuilder::channel`] —
    /// opens more windows without reopening the connection. `options`'
    /// own `channel`, if set, is ignored.
    pub fn create_sibling(&self, mut options: WindowBuilder) -> Result<Window> {
        options.channel = None;
        build_with_connection(self.connection.clone(), options)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self
            .connection
            .borrow()
            .send(Request::DestroyWindow { window: self.id });
    }
}

/// Builder for creating windows with custom options.
pub struct WindowBuilder {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    visible: bool,
    double_buffered: bool,
    channel: Option<Channel>,
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
            double_buffered: false,
            channel: None,
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

    /// Opt into a second buffer: `flush` swaps to the buffer the compositor
    /// isn't reading from, waiting for `BufferReleased` if it's still
    /// in flight, so a client can render tear-free instead of racing the
    /// compositor's tick.
    pub fn double_buffered(mut self, double_buffered: bool) -> Self {
        self.double_buffered = double_buffered;
        self
    }

    /// Connect over an explicit channel instead of the `compositor:` scheme.
    ///
    /// Only needed by tests that spawn the compositor themselves (see
    /// [`Connection::from_channel`]'s doc comment) — production windows
    /// always connect via `environment::connect("compositor:/connect")`,
    /// which the default (no explicit channel) leaves in place.
    pub fn channel(mut self, channel: Channel) -> Self {
        self.channel = Some(channel);
        self
    }

    /// Build the window.
    pub fn build(mut self) -> Result<Window> {
        let channel = self.channel.take();
        let connection = Rc::new(RefCell::new(match channel {
            Some(channel) => Connection::from_channel(channel)?,
            None => Connection::open()?,
        }));
        build_with_connection(connection, self)
    }
}

fn build_with_connection(
    connection: Rc<RefCell<Connection>>,
    options: WindowBuilder,
) -> Result<Window> {
    connection.borrow().send(Request::CreateWindow)?;
    let created = connection
        .borrow_mut()
        .wait_for(|event| matches!(event, PendingEvent::WindowCreated { .. }))?;
    let PendingEvent::WindowCreated { window: id } = created else {
        unreachable!()
    };

    let slot_count = if options.double_buffered { 2 } else { 1 };
    let mut slots = Vec::with_capacity(slot_count);
    for _ in 0..slot_count {
        slots.push(Slot::new(options.width, options.height)?);
    }

    let mut window = Window {
        connection,
        id,
        x: options.x,
        y: options.y,
        slots,
        current: 0,
        visible: options.visible,
        damage: Vec::new(),
    };

    window.set_position(options.x, options.y)?;
    window.set_visible(options.visible)?;

    Ok(window)
}

impl Default for WindowBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// The screen geometry reported by the compositor at connect time.
///
/// Exposed for tests and callers that want to size a window to the screen
/// without hardcoding a resolution; opens (and immediately drops) a
/// connection, so it is not free — cache the result if calling repeatedly.
pub fn screen_size() -> Result<(u32, u32)> {
    let connection = Connection::open()?;
    Ok(connection.screen)
}
