//! Process handle resource for tracking child processes.
//!
//! A ProcessHandle holds a strong reference to a child's ProcessInfo.
//! The ProcessInfo survives after the process exits, allowing the parent
//! to retrieve the exit code. It's cleaned up when all handles are dropped.

use alloc::sync::Arc;

use crate::process::ProcessId;
use crate::process_info::ProcessInfo;
use crate::vfs::Resource;
use crate::waker::Waker;

/// A handle to a child process.
///
/// Holds a strong reference to the process's external info, which survives
/// after the process exits. This allows the parent to retrieve the exit code.
pub struct ProcessHandle {
    info: Arc<ProcessInfo>,
}

impl ProcessHandle {
    /// Create a new process handle from process info.
    pub fn new(info: Arc<ProcessInfo>) -> Self {
        Self { info }
    }

    /// Get the process ID.
    pub fn pid(&self) -> ProcessId {
        self.info.pid()
    }

    /// Check if the process has exited.
    pub fn has_exited(&self) -> bool {
        self.info.has_exited()
    }

    /// Get the exit code if the process has exited.
    pub fn exit_code(&self) -> Option<i32> {
        self.info.exit_code()
    }

    /// Get the waker for blocking until process exits.
    pub fn waker(&self) -> &Arc<Waker> {
        self.info.waker()
    }
}

impl Resource for ProcessHandle {
    // ProcessHandle doesn't implement File - it's accessed via process-specific syscalls
}
