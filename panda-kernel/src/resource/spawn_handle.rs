//! SpawnHandle resource - combines channel endpoint with process info.
//!
//! Returned from spawn() to give parent both IPC channel and process control.

use alloc::sync::Arc;

use crate::process::ProcessId;
use crate::process::info::ProcessInfo;
use crate::process::waker::IoWaker;
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

    fn waker(&self) -> Option<Arc<IoWaker>> {
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

    fn signal(&self, _signal: u32) -> Result<(), ProcessError> {
        // Not implemented: an earlier attempt at this (forcibly calling
        // `scheduler::remove_process` on the target from within another
        // process's syscall context, to let a test that spawned a
        // persistent driver — see `userspace/drivers/virtio-keyboard` —
        // terminate it before exiting) caused a page fault in the *caller*
        // immediately after the target was removed while it still held an
        // active `DeviceResource` (MMIO mapped, DMA allocated, IRQ
        // subscribed). Tearing down a running process's hardware claims
        // synchronously from a different process's call stack isn't
        // exercised or proven safe anywhere else in this kernel — every
        // other process removal happens via `OP_PROCESS_EXIT`, which runs
        // in the *target's own* context. Cooperative self-exit (the target
        // process observing a request and exiting normally) is the safe
        // alternative used instead — see the shutdown-request handling in
        // `userspace/drivers/virtio-keyboard`'s main loop and
        // `plans/device-driver-model.md`'s Phase 7 landed-deviations note.
        Err(ProcessError::NotSupported)
    }

    fn waker(&self) -> Arc<IoWaker> {
        self.process_info.waker().clone()
    }
}
