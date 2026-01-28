//! Mailbox resource for event aggregation.
//!
//! A mailbox aggregates events from multiple attached handles, allowing a process
//! to wait on any of them with a single blocking call. Similar to epoll/kqueue/select.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use spinning_top::Spinlock;

use crate::handle::HandleId;
use crate::process::waker::Waker;
use crate::resource::Resource;

/// A mailbox that aggregates events from attached handles.
pub struct Mailbox {
    inner: Arc<Spinlock<MailboxInner>>,
}

struct MailboxInner {
    /// Handles attached to this mailbox, with their event masks.
    /// Maps handle_id -> event_mask (which events to listen for).
    attached: BTreeMap<HandleId, u32>,

    /// Pending events queue: (handle_id, event_flags).
    pending: VecDeque<(HandleId, u32)>,

    /// Waker for process blocked on wait().
    waker: Arc<Waker>,
}

impl Mailbox {
    /// Create a new mailbox.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Spinlock::new(MailboxInner {
                attached: BTreeMap::new(),
                pending: VecDeque::new(),
                waker: Waker::new(),
            })),
        })
    }

    /// Attach a handle to this mailbox with an event mask.
    pub fn attach(&self, handle_id: HandleId, event_mask: u32) {
        let mut inner = self.inner.lock();
        inner.attached.insert(handle_id, event_mask);
    }

    /// Detach a handle from this mailbox.
    pub fn detach(&self, handle_id: HandleId) {
        let mut inner = self.inner.lock();
        inner.attached.remove(&handle_id);
        // Remove any pending events for this handle
        inner.pending.retain(|(h, _)| *h != handle_id);
    }

    /// Post an event to this mailbox.
    /// Called by resources when events occur.
    /// The event is filtered by the handle's event mask.
    pub fn post_event(&self, handle_id: HandleId, events: u32) {
        let mut inner = self.inner.lock();

        // Check if handle is attached and filter by mask
        if let Some(&mask) = inner.attached.get(&handle_id) {
            let masked = events & mask;
            if masked != 0 {
                inner.pending.push_back((handle_id, masked));
                // Wake the waiting process
                inner.waker.wake();
            }
        }
    }

    /// Wait for the next event (blocking).
    /// Returns (handle_id, event_flags) when an event is available.
    /// Returns None if the mailbox should block.
    pub fn wait(&self) -> Option<(HandleId, u32)> {
        let mut inner = self.inner.lock();
        inner.pending.pop_front()
    }

    /// Poll for an event (non-blocking).
    /// Returns Some((handle_id, event_flags)) if available, None otherwise.
    pub fn poll(&self) -> Option<(HandleId, u32)> {
        let mut inner = self.inner.lock();
        inner.pending.pop_front()
    }

    /// Check if there are pending events.
    pub fn has_pending(&self) -> bool {
        let inner = self.inner.lock();
        !inner.pending.is_empty()
    }

    /// Get the waker for blocking operations.
    pub fn waker(&self) -> Arc<Waker> {
        let inner = self.inner.lock();
        inner.waker.clone()
    }

    /// Clear the waker's signaled state.
    pub fn clear_waker(&self) {
        let inner = self.inner.lock();
        inner.waker.clear();
    }
}

impl Default for Mailbox {
    fn default() -> Self {
        Self {
            inner: Arc::new(Spinlock::new(MailboxInner {
                attached: BTreeMap::new(),
                pending: VecDeque::new(),
                waker: Waker::new(),
            })),
        }
    }
}

impl Resource for Mailbox {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Mailbox
    }

    fn waker(&self) -> Option<Arc<Waker>> {
        Some(self.waker())
    }

    fn as_mailbox(&self) -> Option<&Mailbox> {
        Some(self)
    }
}

/// A reference to a mailbox held by resources for posting events.
/// Uses a weak reference to avoid reference cycles.
#[derive(Clone)]
pub struct MailboxRef {
    inner: Weak<Spinlock<MailboxInner>>,
    handle_id: HandleId,
}

impl MailboxRef {
    /// Create a new mailbox reference.
    pub fn new(mailbox: &Mailbox, handle_id: HandleId) -> Self {
        Self {
            inner: Arc::downgrade(&mailbox.inner),
            handle_id,
        }
    }

    /// Post an event to the mailbox.
    /// Does nothing if the mailbox has been dropped.
    pub fn post_event(&self, events: u32) {
        if let Some(inner) = self.inner.upgrade() {
            let mut inner = inner.lock();

            // Check if handle is attached and filter by mask.
            // The mask determines which event TYPES to accept (bits 0-7),
            // but we deliver the full event including any encoded data (e.g., key codes).
            if let Some(&mask) = inner.attached.get(&self.handle_id) {
                // Check if any requested event type is present
                if events & mask != 0 {
                    // Deliver the full event, not the masked version
                    inner.pending.push_back((self.handle_id, events));
                    inner.waker.wake();
                }
            }
        }
    }
}
