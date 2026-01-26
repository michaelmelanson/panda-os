//! Mailbox abstraction for event multiplexing.
//!
//! A mailbox aggregates events from multiple handles, allowing a process
//! to wait on any of them with a single blocking call.

use crate::handle::Handle;
use crate::syscall::send;
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
    pub fn create() -> Result<Self, isize> {
        let result = send(
            Handle::from(0), // handle arg unused for create
            OP_MAILBOX_CREATE,
            0,
            0,
            0,
            0,
        );
        if result < 0 {
            Err(result)
        } else {
            Ok(Self {
                handle: Handle::from(result as u32),
            })
        }
    }

    /// Get the raw handle for this mailbox.
    #[inline(always)]
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Wait for the next event (blocking).
    ///
    /// Returns `(handle, event)` when an event is available.
    #[inline(always)]
    pub fn recv(&self) -> (Handle, Event) {
        let result = send(self.handle, OP_MAILBOX_WAIT, 0, 0, 0, 0);
        // Result is packed as (handle_id << 32) | events
        let handle_id = (result >> 32) as u32;
        let events = result as u32;
        (Handle::from(handle_id), Event::decode(events))
    }

    /// Poll for an event (non-blocking).
    ///
    /// Returns `Some((handle, event))` if available, `None` otherwise.
    #[inline(always)]
    pub fn try_recv(&self) -> Option<(Handle, Event)> {
        let result = send(self.handle, OP_MAILBOX_POLL, 0, 0, 0, 0);
        if result == 0 {
            None
        } else {
            let handle_id = (result >> 32) as u32;
            let events = result as u32;
            Some((Handle::from(handle_id), Event::decode(events)))
        }
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

/// Events that can be received from handles, organized by resource type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Input device events (keyboard, mouse).
    Input(InputEvent),
    /// Channel events (readable, writable, closed).
    Channel(ChannelEvent),
    /// Process events (exited).
    Process(ProcessEvent),
    /// Unknown or combined event flags.
    Raw(u32),
}

impl Event {
    /// Decode raw event flags into an Event.
    ///
    /// Returns the most specific event type. For combined flags,
    /// the mailbox will return them as separate recv() calls.
    pub fn decode(flags: u32) -> Self {
        // Check channel events first (most common for IPC)
        if flags & EVENT_CHANNEL_CLOSED != 0 {
            return Event::Channel(ChannelEvent::Closed);
        }
        if flags & EVENT_CHANNEL_READABLE != 0 {
            return Event::Channel(ChannelEvent::Readable);
        }
        if flags & EVENT_CHANNEL_WRITABLE != 0 {
            return Event::Channel(ChannelEvent::Writable);
        }
        // Check process events
        if flags & EVENT_PROCESS_EXITED != 0 {
            return Event::Process(ProcessEvent::Exited);
        }
        // Check input events
        if flags & EVENT_KEYBOARD_KEY != 0 {
            return Event::Input(InputEvent::Keyboard);
        }
        // Fallback for unknown events
        Event::Raw(flags)
    }
}
