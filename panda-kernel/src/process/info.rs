//! Process information visible to handle holders.
//!
//! ProcessInfo contains the external state of a process that persists after
//! the process exits, allowing parents to retrieve exit codes via handles.

use alloc::sync::Arc;
use alloc::vec::Vec;
use spinning_top::{RwSpinlock, Spinlock};

use crate::process::ProcessId;
use crate::resource::MailboxRef;

use super::waker::Waker;

/// External process information accessible via handles.
///
/// This struct is shared between the Process (which owns it) and any
/// ProcessHandles (which hold strong references). When the process exits,
/// it sets the exit_code and wakes any waiters. The ProcessInfo lives
/// until all handles are dropped.
pub struct ProcessInfo {
    /// Process ID
    pid: ProcessId,
    /// Exit code, set when process terminates. None while running.
    exit_code: RwSpinlock<Option<i32>>,
    /// Waker to notify when process exits (for wait() syscall)
    waker: Arc<Waker>,
    /// Mailboxes to notify when process exits
    exit_mailboxes: Spinlock<Vec<MailboxRef>>,
}

impl ProcessInfo {
    /// Create new process info for a running process.
    pub fn new(pid: ProcessId) -> Self {
        Self {
            pid,
            exit_code: RwSpinlock::new(None),
            waker: Waker::new(),
            exit_mailboxes: Spinlock::new(Vec::new()),
        }
    }

    /// Register a mailbox to be notified when this process exits.
    pub fn add_exit_mailbox(&self, mailbox: MailboxRef) {
        self.exit_mailboxes.lock().push(mailbox);
    }

    /// Get the process ID.
    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    /// Check if the process has exited.
    pub fn has_exited(&self) -> bool {
        self.exit_code.read().is_some()
    }

    /// Get the exit code if the process has exited.
    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.read()
    }

    /// Set the exit code when process terminates. Wakes any waiters and notifies mailboxes.
    pub fn set_exit_code(&self, code: i32) {
        *self.exit_code.write() = Some(code);
        self.waker.wake();

        // Notify all registered mailboxes
        for mailbox in self.exit_mailboxes.lock().iter() {
            mailbox.post_event(panda_abi::EVENT_PROCESS_EXITED);
        }
    }

    /// Get the waker for blocking on process exit.
    pub fn waker(&self) -> &Arc<Waker> {
        &self.waker
    }
}
