//! SpawnHandle resource - combines channel endpoint with process info.
//!
//! Returned from spawn() to give parent both IPC channel and process control.

use alloc::sync::Arc;

use crate::process::ProcessId;
use crate::process::info::ProcessInfo;
use crate::process::waker::Waker;
use crate::resource::channel::ChannelError;
use crate::resource::process::{Process, ProcessError};
use crate::resource::{ChannelEndpoint, MailboxRef, Resource};
use crate::scheduler;

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
}

impl Resource for SpawnHandle {
    fn handle_type(&self) -> panda_abi::HandleType {
        // Process handles are also valid as channels
        panda_abi::HandleType::Process
    }

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

    fn attach_mailbox(&self, mailbox_ref: MailboxRef) {
        // Attach to channel for channel events
        self.channel.attach_mailbox(mailbox_ref.clone());
        // Register with process info to get notified on exit
        self.process_info.add_exit_mailbox(mailbox_ref);
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

    fn signal(&self, signal: u32) -> Result<(), ProcessError> {
        let signal = panda_abi::Signal::from_u32(signal).ok_or(ProcessError::NotSupported)?;

        match signal {
            panda_abi::Signal::StopImmediately => {
                // SIGKILL: Immediate forced termination
                let pid = self.process_info.pid();

                // Check if already exited
                if self.process_info.has_exited() {
                    return Ok(());
                }

                // Set exit code before removal so waiters see it
                // Convention: -9 is the SIGKILL exit code
                self.process_info.set_exit_code(-9);

                // Remove from scheduler (reclaims memory, closes handles)
                scheduler::remove_process(pid);

                Ok(())
            }
            panda_abi::Signal::Stop => {
                // SIGTERM: Deliver message to process's parent channel
                // The child process reads from HANDLE_PARENT and handles it

                // Check if already exited
                if self.process_info.has_exited() {
                    return Err(ProcessError::NotFound);
                }

                // Build signal message using safe encoding
                let mut buf = [0u8; panda_abi::SIGNAL_MESSAGE_SIZE];
                let len = panda_abi::encode_signal_message(signal, &mut buf)
                    .ok_or(ProcessError::NotSupported)?;

                // Send via channel with SIGNAL_RECEIVED event
                // (also includes CHANNEL_READABLE so the child can recv the message)
                let event_flags =
                    panda_abi::EVENT_SIGNAL_RECEIVED | panda_abi::EVENT_CHANNEL_READABLE;
                self.channel
                    .send_with_event(&buf[..len], event_flags)
                    .map_err(|e| match e {
                        ChannelError::QueueFull => ProcessError::WouldBlock,
                        ChannelError::PeerClosed => ProcessError::NotFound,
                        _ => ProcessError::NotSupported,
                    })?;

                Ok(())
            }
        }
    }

    fn waker(&self) -> Arc<Waker> {
        self.process_info.waker().clone()
    }
}
