//! Mailbox abstraction for event multiplexing.
//!
//! A mailbox aggregates events from multiple handles, allowing a process
//! to wait on any of them with a single blocking call.

use crate::error::{self, Result};
use crate::handle::Handle;
use crate::sys;
use panda_abi::*;

/// A mailbox for receiving events from attached handles.
#[derive(Debug, Clone, Copy)]
pub struct Mailbox {
    handle: Handle,
}

impl Mailbox {
    /// Get the default mailbox (HANDLE_MAILBOX).
    ///
    /// Every process has a default mailbox created automatically.
    #[inline(always)]
    pub fn default() -> Self {
        Self {
            handle: Handle::from(HANDLE_MAILBOX),
        }
    }

    /// Create a new mailbox.
    #[inline(always)]
    pub fn create() -> Result<Self> {
        let handle = error::from_syscall_handle(sys::mailbox::create())?;
        Ok(Self { handle })
    }

    /// Get the raw handle for this mailbox.
    #[inline(always)]
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Wait for the next event (blocking).
    ///
    /// Returns `(handle, events)` when an event is available.
    /// The `Events` struct may contain multiple event flags.
    #[inline(always)]
    pub fn recv(&self) -> (Handle, Events) {
        let mut event_result = MailboxEventResult {
            handle_id: 0,
            events: 0,
            _pad: 0,
        };
        sys::mailbox::wait(self.handle, &mut event_result);
        (Handle::from(event_result.handle_id), Events(event_result.events))
    }

    /// Poll for an event (non-blocking).
    ///
    /// Returns `Some((handle, events))` if available, `None` otherwise.
    #[inline(always)]
    pub fn try_recv(&self) -> Option<(Handle, Events)> {
        let mut event_result = MailboxEventResult {
            handle_id: 0,
            events: 0,
            _pad: 0,
        };
        let result = sys::mailbox::poll(self.handle, &mut event_result);
        if result <= 0 {
            None
        } else {
            Some((Handle::from(event_result.handle_id), Events(event_result.events)))
        }
    }
}

/// A set of event flags from a mailbox.
///
/// Multiple events can be signalled at once, so use the `is_*` methods
/// to check for specific events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Events(u32);

impl Events {
    /// Get the raw event flags.
    #[inline(always)]
    pub fn raw(&self) -> u32 {
        self.0
    }

    /// Check if keyboard input is available.
    #[inline(always)]
    pub fn is_keyboard(&self) -> bool {
        self.0 & EVENT_KEYBOARD_KEY != 0
    }

    /// Check if a channel message is available to receive.
    #[inline(always)]
    pub fn is_channel_readable(&self) -> bool {
        self.0 & EVENT_CHANNEL_READABLE != 0
    }

    /// Check if a channel has space to send.
    #[inline(always)]
    pub fn is_channel_writable(&self) -> bool {
        self.0 & EVENT_CHANNEL_WRITABLE != 0
    }

    /// Check if a channel peer has closed their endpoint.
    #[inline(always)]
    pub fn is_channel_closed(&self) -> bool {
        self.0 & EVENT_CHANNEL_CLOSED != 0
    }

    /// Check if a child process has exited.
    #[inline(always)]
    pub fn is_process_exited(&self) -> bool {
        self.0 & EVENT_PROCESS_EXITED != 0
    }

    /// Check if a signal was received.
    #[inline(always)]
    pub fn is_signal_received(&self) -> bool {
        self.0 & EVENT_SIGNAL_RECEIVED != 0
    }

    /// Iterate over all set events.
    ///
    /// This yields each event that is set in the flags.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use libpanda::mailbox::{Mailbox, Event, ChannelEvent, ProcessEvent};
    ///
    /// let mailbox = Mailbox::default();
    /// let (handle, events) = mailbox.recv();
    /// for event in events.iter() {
    ///     match event {
    ///         Event::Channel(ChannelEvent::Readable) => { /* ... */ }
    ///         Event::Process(ProcessEvent::Exited) => { /* ... */ }
    ///         _ => {}
    ///     }
    /// }
    /// ```
    pub fn iter(&self) -> EventIter {
        EventIter {
            flags: self.0,
            index: 0,
        }
    }
}

impl IntoIterator for Events {
    type Item = Event;
    type IntoIter = EventIter;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Event types with their corresponding flag bits, in priority order.
///
/// This defines the iteration order for [`EventIter`] and the priority
/// for [`Events::to_event()`].
const EVENT_TYPES: &[(u32, Event)] = &[
    (EVENT_CHANNEL_CLOSED, Event::Channel(ChannelEvent::Closed)),
    (
        EVENT_CHANNEL_READABLE,
        Event::Channel(ChannelEvent::Readable),
    ),
    (
        EVENT_CHANNEL_WRITABLE,
        Event::Channel(ChannelEvent::Writable),
    ),
    (EVENT_PROCESS_EXITED, Event::Process(ProcessEvent::Exited)),
    (EVENT_KEYBOARD_KEY, Event::Input(InputEvent::Keyboard)),
];

/// Iterator over events in an [`Events`] set.
#[derive(Debug, Clone)]
pub struct EventIter {
    flags: u32,
    index: usize,
}

impl Iterator for EventIter {
    type Item = Event;

    fn next(&mut self) -> Option<Self::Item> {
        // Check each known event type in priority order
        while self.index < EVENT_TYPES.len() {
            let (flag, event) = EVENT_TYPES[self.index];
            self.index += 1;
            if self.flags & flag != 0 {
                self.flags &= !flag;
                return Some(event);
            }
        }
        // Any remaining unknown flags
        if self.flags != 0 {
            let unknown = self.flags;
            self.flags = 0;
            return Some(Event::Unknown(unknown));
        }
        None
    }
}

/// Input device events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Keyboard input available - read from keyboard handle to get key data.
    Keyboard,
    /// Mouse input available - read from mouse handle to get mouse data.
    Mouse,
}

/// Channel events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEvent {
    /// Message available to receive.
    Readable,
    /// Space available to send.
    Writable,
    /// Peer closed their endpoint.
    Closed,
}

/// Process events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessEvent {
    /// Child process has exited.
    Exited,
}

/// A single event type for simple dispatch.
///
/// For handling multiple simultaneous events, use [`Events`] directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Input device events (keyboard, mouse).
    Input(InputEvent),
    /// Channel events (readable, writable, closed).
    Channel(ChannelEvent),
    /// Process events (exited).
    Process(ProcessEvent),
    /// Unknown or unhandled event flags.
    Unknown(u32),
}
