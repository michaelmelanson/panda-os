//! Process resource for child process handles.

use alloc::sync::Arc;

use crate::process::ProcessId;
use crate::process::info::ProcessInfo;
use crate::process::waker::Waker;

use super::Resource;
use super::process::{Process, ProcessError};

/// A resource wrapping a child process handle.
pub struct ProcessResource {
    info: Arc<ProcessInfo>,
}

impl ProcessResource {
    /// Create a new process resource from process info.
    pub fn new(info: Arc<ProcessInfo>) -> Self {
        Self { info }
    }
}

impl Resource for ProcessResource {
    fn as_process(&self) -> Option<&dyn Process> {
        Some(self)
    }

    fn waker(&self) -> Option<Arc<Waker>> {
        Some(self.info.waker().clone())
    }
}

impl Process for ProcessResource {
    fn pid(&self) -> ProcessId {
        self.info.pid()
    }

    fn is_running(&self) -> bool {
        !self.info.has_exited()
    }

    fn exit_code(&self) -> Option<i32> {
        self.info.exit_code()
    }

    fn signal(&self, _signal: u32) -> Result<(), ProcessError> {
        // TODO: Implement signal delivery
        Err(ProcessError::NotSupported)
    }

    fn waker(&self) -> Arc<Waker> {
        self.info.waker().clone()
    }
}
