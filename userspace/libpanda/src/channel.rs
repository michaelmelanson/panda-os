//! Channel abstraction for message-passing.
//!
//! A Channel wraps a resource handle and provides request/response correlation
//! for the future message-passing interface. For now, this module provides the
//! infrastructure that will be used when send/recv syscalls replace the current
//! operation-based interface.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use panda_abi::MessageHeader;

/// A unique identifier for a pending request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// A channel for message-passing with a resource.
///
/// Channels handle request/response correlation, allowing multiple in-flight
/// requests and handling out-of-order responses.
pub struct Channel {
    /// The underlying resource handle.
    handle: u32,
    /// Next message ID to use for requests.
    next_id: u64,
    /// Responses that arrived but haven't been claimed yet.
    pending_responses: BTreeMap<u64, Vec<u8>>,
    /// Unsolicited events (messages with id=0).
    unsolicited: Vec<Vec<u8>>,
}

impl Channel {
    /// Create a new channel for a resource handle.
    pub fn new(handle: u32) -> Self {
        Self {
            handle,
            next_id: 1, // Start at 1, 0 is reserved for unsolicited events
            pending_responses: BTreeMap::new(),
            unsolicited: Vec::new(),
        }
    }

    /// Get the underlying handle ID.
    pub fn handle(&self) -> u32 {
        self.handle
    }

    /// Allocate the next request ID.
    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Create a message header for a new request.
    pub fn new_request_header(&mut self, msg_type: u32) -> MessageHeader {
        MessageHeader {
            id: self.alloc_id(),
            msg_type,
            _reserved: 0,
        }
    }

    /// Send a request and get a RequestId for later correlation.
    ///
    /// Note: This is a stub for the future message-passing interface.
    /// Currently it just stores the message locally.
    pub fn request(&mut self, _msg: &[u8]) -> RequestId {
        let id = self.alloc_id();
        // TODO: When send/recv syscalls are implemented:
        // syscall::msg_send(self.handle, msg);
        RequestId(id)
    }

    /// Wait for and receive a response for a specific request.
    ///
    /// Note: This is a stub for the future message-passing interface.
    pub fn response(&mut self, id: RequestId) -> Option<Vec<u8>> {
        // Check if we already have this response
        if let Some(msg) = self.pending_responses.remove(&id.0) {
            return Some(msg);
        }

        // TODO: When send/recv syscalls are implemented:
        // Loop receiving messages until we get the one we want
        // loop {
        //     let msg = syscall::msg_recv(self.handle);
        //     let header = parse_header(&msg);
        //     if header.id == id.0 {
        //         return Some(msg);
        //     } else if header.id == 0 {
        //         self.unsolicited.push(msg);
        //     } else {
        //         self.pending_responses.insert(header.id, msg);
        //     }
        // }

        None
    }

    /// Synchronous call: send request and wait for response.
    ///
    /// Note: This is a stub for the future message-passing interface.
    pub fn call(&mut self, msg: &[u8]) -> Option<Vec<u8>> {
        let id = self.request(msg);
        self.response(id)
    }

    /// Poll for an unsolicited event (id=0 messages).
    pub fn poll_event(&mut self) -> Option<Vec<u8>> {
        if self.unsolicited.is_empty() {
            // TODO: When send/recv syscalls are implemented:
            // Try a non-blocking recv
            None
        } else {
            Some(self.unsolicited.remove(0))
        }
    }
}

/// A File wrapper that uses Channel internally for stream operations.
///
/// This provides the traditional read/write/seek interface on top of
/// the message-passing Channel abstraction.
pub struct StreamFile {
    channel: Channel,
    offset: u64,
}

impl StreamFile {
    /// Create a new StreamFile from a handle.
    pub fn new(handle: u32) -> Self {
        Self {
            channel: Channel::new(handle),
            offset: 0,
        }
    }

    /// Get the current offset.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Get the underlying handle.
    pub fn handle(&self) -> u32 {
        self.channel.handle()
    }

    // Note: read/write/seek methods would use the Channel's call() method
    // to send BlockMessage requests. For now, the existing file module
    // functions continue to use the operation-based syscalls.
}
