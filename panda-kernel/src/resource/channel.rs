//! Channel resource for message-based IPC.
//!
//! Channels are bidirectional message-based communication endpoints.
//! Messages are atomic byte blocks up to MAX_MESSAGE_SIZE bytes.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spinning_top::Spinlock;

use panda_abi::{DEFAULT_QUEUE_CAPACITY, MAX_MESSAGE_SIZE};

use crate::process::waker::Waker;
use crate::resource::{MailboxRef, Resource};

/// Error type for channel operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    /// Message exceeds MAX_MESSAGE_SIZE.
    MessageTooLarge,
    /// Buffer too small for message.
    BufferTooSmall,
    /// Queue is full (non-blocking send).
    QueueFull,
    /// Queue is empty (non-blocking recv).
    QueueEmpty,
    /// Peer has closed their endpoint.
    PeerClosed,
}

/// Which side of the channel this endpoint represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    A,
    B,
}

/// One half of a channel's state (one direction of communication).
struct ChannelHalf {
    /// Outgoing message queue (messages sent by this side).
    queue: VecDeque<Vec<u8>>,
    /// Is this side closed?
    closed: bool,
    /// Waker for this side (woken when peer sends or closes).
    waker: Arc<Waker>,
    /// Mailbox reference for this side.
    mailbox: Option<MailboxRef>,
}

impl ChannelHalf {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            closed: false,
            waker: Waker::new(),
            mailbox: None,
        }
    }
}

/// Shared state between the two endpoints of a channel.
struct ChannelShared {
    /// Side A's state (queue contains messages flowing A→B).
    a: ChannelHalf,
    /// Side B's state (queue contains messages flowing B→A).
    b: ChannelHalf,
    /// Max messages per queue.
    capacity: usize,
}

impl ChannelShared {
    fn new() -> Self {
        Self {
            a: ChannelHalf::new(),
            b: ChannelHalf::new(),
            capacity: DEFAULT_QUEUE_CAPACITY,
        }
    }

    /// Get (our_half, peer_half) based on side.
    fn halves(&mut self, side: Side) -> (&mut ChannelHalf, &mut ChannelHalf) {
        match side {
            Side::A => (&mut self.a, &mut self.b),
            Side::B => (&mut self.b, &mut self.a),
        }
    }
}

/// One endpoint of a channel.
pub struct ChannelEndpoint {
    shared: Arc<Spinlock<ChannelShared>>,
    side: Side,
}

impl ChannelEndpoint {
    /// Create a pair of connected channel endpoints.
    pub fn create_pair() -> (ChannelEndpoint, ChannelEndpoint) {
        let shared = Arc::new(Spinlock::new(ChannelShared::new()));
        (
            ChannelEndpoint {
                shared: shared.clone(),
                side: Side::A,
            },
            ChannelEndpoint {
                shared,
                side: Side::B,
            },
        )
    }

    /// Attach this endpoint to a mailbox.
    pub fn attach_mailbox(&self, mailbox_ref: MailboxRef) {
        let mut shared = self.shared.lock();
        let (ours, _) = shared.halves(self.side);
        ours.mailbox = Some(mailbox_ref);
    }

    /// Send a message to the peer.
    /// Returns Ok(()) on success, or an error.
    pub fn send(&self, msg: &[u8]) -> Result<(), ChannelError> {
        if msg.len() > MAX_MESSAGE_SIZE {
            return Err(ChannelError::MessageTooLarge);
        }

        let mut shared = self.shared.lock();
        let capacity = shared.capacity;
        let (ours, peer) = shared.halves(self.side);

        if peer.closed {
            return Err(ChannelError::PeerClosed);
        }

        if ours.queue.len() >= capacity {
            return Err(ChannelError::QueueFull);
        }

        ours.queue.push_back(msg.to_vec());

        // Notify peer
        peer.waker.wake();
        if let Some(ref mailbox) = peer.mailbox {
            mailbox.post_event(panda_abi::EVENT_CHANNEL_READABLE);
        }

        Ok(())
    }

    /// Check if send would block (queue is full).
    pub fn would_block_send(&self) -> bool {
        let mut shared = self.shared.lock();
        let capacity = shared.capacity;
        let (ours, _) = shared.halves(self.side);
        ours.queue.len() >= capacity
    }

    /// Receive a message from the peer.
    /// Returns Ok(len) on success where len is the message length.
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize, ChannelError> {
        let mut shared = self.shared.lock();
        let capacity = shared.capacity;
        let (_, peer) = shared.halves(self.side);

        // We receive from peer's queue (peer sends to us via their queue)
        if let Some(msg) = peer.queue.pop_front() {
            if buf.len() < msg.len() {
                // Put message back and return error
                peer.queue.push_front(msg);
                return Err(ChannelError::BufferTooSmall);
            }

            let len = msg.len();
            buf[..len].copy_from_slice(&msg);

            // Notify peer that there's space now (if queue was full)
            let was_full = peer.queue.len() + 1 >= capacity;
            if was_full {
                peer.waker.wake();
                if let Some(ref mailbox) = peer.mailbox {
                    mailbox.post_event(panda_abi::EVENT_CHANNEL_WRITABLE);
                }
            }

            Ok(len)
        } else {
            // Queue empty - check if peer closed
            if peer.closed {
                Err(ChannelError::PeerClosed)
            } else {
                Err(ChannelError::QueueEmpty)
            }
        }
    }

    /// Check if recv would block (queue is empty and peer not closed).
    pub fn would_block_recv(&self) -> bool {
        let mut shared = self.shared.lock();
        let (_, peer) = shared.halves(self.side);
        peer.queue.is_empty() && !peer.closed
    }

    /// Check if peer has closed.
    pub fn is_peer_closed(&self) -> bool {
        let mut shared = self.shared.lock();
        let (_, peer) = shared.halves(self.side);
        peer.closed
    }

    /// Get current event flags for this endpoint.
    pub fn poll_events(&self) -> u32 {
        let mut shared = self.shared.lock();
        let capacity = shared.capacity;
        let (ours, peer) = shared.halves(self.side);

        let mut events = 0u32;

        // Readable if peer's queue has messages for us
        if !peer.queue.is_empty() {
            events |= panda_abi::EVENT_CHANNEL_READABLE;
        }

        // Writable if our send queue has space
        if ours.queue.len() < capacity {
            events |= panda_abi::EVENT_CHANNEL_WRITABLE;
        }

        // Peer closed
        if peer.closed {
            events |= panda_abi::EVENT_CHANNEL_CLOSED;
        }

        events
    }

    /// Get the waker for this endpoint.
    pub fn waker(&self) -> Arc<Waker> {
        let mut shared = self.shared.lock();
        let (ours, _) = shared.halves(self.side);
        ours.waker.clone()
    }

    /// Close this endpoint.
    fn close(&self) {
        let mut shared = self.shared.lock();
        let (ours, peer) = shared.halves(self.side);

        ours.closed = true;

        // Notify peer
        peer.waker.wake();
        if let Some(ref mailbox) = peer.mailbox {
            mailbox.post_event(panda_abi::EVENT_CHANNEL_CLOSED);
        }
    }
}

impl Drop for ChannelEndpoint {
    fn drop(&mut self) {
        self.close();
    }
}

impl Resource for ChannelEndpoint {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Channel
    }

    fn waker(&self) -> Option<Arc<crate::process::waker::Waker>> {
        Some(self.waker())
    }

    fn as_channel(&self) -> Option<&ChannelEndpoint> {
        Some(self)
    }

    fn supported_events(&self) -> u32 {
        panda_abi::EVENT_CHANNEL_READABLE
            | panda_abi::EVENT_CHANNEL_WRITABLE
            | panda_abi::EVENT_CHANNEL_CLOSED
    }

    fn poll_events(&self) -> u32 {
        ChannelEndpoint::poll_events(self)
    }
}
