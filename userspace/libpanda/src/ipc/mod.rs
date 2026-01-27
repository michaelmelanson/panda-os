//! Inter-process communication abstractions.
//!
//! This module provides high-level abstractions for IPC:
//!
//! - [`Channel`] - A message channel for sending/receiving byte messages
//! - Re-exports from [`crate::mailbox`] for event handling

mod channel;

pub use channel::Channel;

// Re-export mailbox types for convenience
pub use crate::mailbox::{ChannelEvent, Event, Events, InputEvent, Mailbox, ProcessEvent};

/// Maximum size of a single channel message.
pub use panda_abi::MAX_MESSAGE_SIZE;
