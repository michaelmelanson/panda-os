//! EventSource interface for event-producing resources (keyboard, mouse, timers).

use alloc::sync::Arc;

use crate::process::waker::Waker;

/// An event from an event source.
#[derive(Debug, Clone)]
pub enum Event {
    /// Key press/release event.
    Key(KeyEvent),
    /// Mouse movement/button event.
    Mouse(MouseEvent),
    /// Timer expiration.
    Timer,
}

/// A keyboard key event.
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    /// Key code.
    pub code: u16,
    /// Event value: 0=release, 1=press, 2=repeat.
    pub value: u32,
}

/// A mouse event.
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    /// X movement delta.
    pub dx: i32,
    /// Y movement delta.
    pub dy: i32,
    /// Button state changes.
    pub buttons: u32,
}

/// Interface for event-producing resources.
///
/// Implemented by keyboard, mouse, timers, network sockets, etc.
pub trait EventSource: Send + Sync {
    /// Poll for an available event.
    ///
    /// Returns `Some(event)` if an event is available, `None` otherwise.
    fn poll(&self) -> Option<Event>;

    /// Get a waker for blocking until an event is available.
    fn waker(&self) -> Arc<Waker>;
}
