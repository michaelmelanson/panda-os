//! Process interface for child process handles.

use alloc::sync::Arc;

use crate::process::ProcessId;
use crate::process::waker::Waker;

/// Interface for process handles.
///
/// Allows querying process status, sending signals, and waiting for exit.
pub trait Process: Send + Sync {
    /// Get the process ID.
    fn pid(&self) -> ProcessId;

    /// Check if the process is still running.
    fn is_running(&self) -> bool;

    /// Get the exit code, if the process has exited.
    fn exit_code(&self) -> Option<i32>;

    /// Send a signal to the process.
    fn signal(&self, _signal: u32) -> Result<(), ProcessError> {
        Err(ProcessError::NotSupported)
    }

    /// Get a waker for blocking until the process exits.
    fn waker(&self) -> Arc<Waker>;
}

/// Errors that can occur during process operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessError {
    /// Operation not supported.
    NotSupported,
    /// Process not found.
    NotFound,
    /// Permission denied.
    PermissionDenied,
    /// Operation would block.
    WouldBlock,
}
