//! Mailbox abstraction for event multiplexing.
//!
//! A mailbox aggregates events from multiple handles, allowing a process
//! to wait on any of them with a single blocking call.

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
    pub fn create() -> Result<Self, isize> {
        let result = sys::mailbox::create();
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
    /// Returns `(handle, events)` when an event is available.
    /// The `Events` struct may contain multiple event flags.
    #[inline(always)]
    pub fn recv(&self) -> (Handle, Events) {
        let result = sys::mailbox::wait(self.handle);
        let (handle_id, flags) = sys::mailbox::unpack_result(result);
        (Handle::from(handle_id), Events(flags))
    }

    /// Poll for an event (non-blocking).
    ///
    /// Returns `Some((handle, events))` if available, `None` otherwise.
    #[inline(always)]
    pub fn try_recv(&self) -> Option<(Handle, Events)> {
        let result = sys::mailbox::poll(self.handle);
        if result == 0 {
            None
        } else {
            let (handle_id, flags) = sys::mailbox::unpack_result(result);
            Some((Handle::from(handle_id), Events(flags)))
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

    /// Convert to a single Event enum for simple dispatch.
    ///
    /// This returns the highest-priority event if multiple are set.
    /// Priority order: Closed > Readable > Writable > Exited > Keyboard.
    ///
    /// For handling multiple events, use the `is_*` methods instead.
    pub fn to_event(&self) -> Event {
        // Check channel events first (most common for IPC)
        if self.is_channel_closed() {
            return Event::Channel(ChannelEvent::Closed);
        }
        if self.is_channel_readable() {
            return Event::Channel(ChannelEvent::Readable);
        }
        if self.is_channel_writable() {
            return Event::Channel(ChannelEvent::Writable);
        }
        // Check process events
        if self.is_process_exited() {
            return Event::Process(ProcessEvent::Exited);
        }
        // Check input events
        if self.is_keyboard() {
            return Event::Input(InputEvent::Keyboard);
        }
        // Fallback for unknown events
        Event::Unknown(self.0)
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
