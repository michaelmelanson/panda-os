//! SpawnHandle resource - combines channel endpoint with process info.
//!
//! Returned from spawn() to give parent both IPC channel and process control.

use alloc::sync::Arc;

use crate::process::ProcessId;
use crate::process::info::ProcessInfo;
use crate::process::waker::Waker;
use crate::resource::process::{Process, ProcessError};
use crate::resource::{ChannelEndpoint, MailboxRef, Resource};

/// A handle returned from spawn() that combines channel and process info.
///
/// This allows the parent to:
/// - Send/recv messages via the channel (as_channel)
/// - Wait for child exit (as_process)
/// - Get notified of events via mailbox
pub struct SpawnHandle {
    /// Channel endpoint for communication with child.
    channel: ChannelEndpoint,
    /// Process info for wait/signal operations.
    process_info: Arc<ProcessInfo>,
}

impl SpawnHandle {
    /// Create a new spawn handle.
    pub fn new(channel: ChannelEndpoint, process_info: Arc<ProcessInfo>) -> Self {
        Self {
            channel,
            process_info,
        }
    }

    /// Attach this handle's channel to a mailbox.
    pub fn attach_mailbox(&self, mailbox_ref: MailboxRef) {
        self.channel.attach_mailbox(mailbox_ref);
    }
}

impl Resource for SpawnHandle {
    fn as_channel(&self) -> Option<&ChannelEndpoint> {
        Some(&self.channel)
    }

    fn as_process(&self) -> Option<&dyn Process> {
        Some(self)
    }

    fn waker(&self) -> Option<Arc<Waker>> {
        // Return the process waker for blocking on exit
        Some(self.process_info.waker().clone())
    }

    fn supported_events(&self) -> u32 {
        // Combine channel events with process exit event
        panda_abi::EVENT_CHANNEL_READABLE
            | panda_abi::EVENT_CHANNEL_WRITABLE
            | panda_abi::EVENT_CHANNEL_CLOSED
            | panda_abi::EVENT_PROCESS_EXITED
    }

    fn poll_events(&self) -> u32 {
        let mut events = self.channel.poll_events();

        // Add process exited if applicable
        if self.process_info.exit_code().is_some() {
            events |= panda_abi::EVENT_PROCESS_EXITED;
        }

        events
    }
}

impl Process for SpawnHandle {
    fn pid(&self) -> ProcessId {
        self.process_info.pid()
    }

    fn is_running(&self) -> bool {
        !self.process_info.has_exited()
    }

    fn exit_code(&self) -> Option<i32> {
        self.process_info.exit_code()
    }

    fn signal(&self, _signal: u32) -> Result<(), ProcessError> {
        // TODO: Implement signal delivery
        Err(ProcessError::NotSupported)
    }

    fn waker(&self) -> Arc<Waker> {
        self.process_info.waker().clone()
    }
}
