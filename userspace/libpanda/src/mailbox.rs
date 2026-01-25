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

/// Events that can be received from handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Key event available (for keyboard handles).
    Key(KeyEvent),
    /// Message available to receive (for channel handles).
    ChannelReadable,
    /// Space available to send (for channel handles).
    ChannelWritable,
    /// Peer closed their endpoint (for channel handles).
    ChannelClosed,
    /// Child process has exited (for spawn handles).
    ProcessExited,
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
            return Event::ChannelClosed;
        }
        if flags & EVENT_CHANNEL_READABLE != 0 {
            return Event::ChannelReadable;
        }
        if flags & EVENT_CHANNEL_WRITABLE != 0 {
            return Event::ChannelWritable;
        }
        // Check process events
        if flags & EVENT_PROCESS_EXITED != 0 {
            return Event::ProcessExited;
        }
        // Check keyboard events - key data is packed in the flags
        if flags & EVENT_KEYBOARD_KEY != 0 {
            return Event::Key(KeyEvent {
                code: panda_abi::decode_key_code(flags),
                value: KeyValue::from(panda_abi::decode_key_value(flags) as u32),
            });
        }
        // Fallback for unknown events
        Event::Raw(flags)
    }
}

/// A keyboard key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// Key code (Linux input event code).
    pub code: u16,
    /// Key state (press/release/repeat).
    pub value: KeyValue,
}

/// Key event value (press/release/repeat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyValue {
    /// Key was released.
    Release = 0,
    /// Key was pressed.
    Press = 1,
    /// Key is being held (repeat).
    Repeat = 2,
}

impl From<u32> for KeyValue {
    fn from(v: u32) -> Self {
        match v {
            0 => KeyValue::Release,
            1 => KeyValue::Press,
            2 => KeyValue::Repeat,
            _ => KeyValue::Press, // Default to press for unknown
        }
    }
}
