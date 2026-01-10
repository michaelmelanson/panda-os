//! Handle-based resource management.
//!
//! Handles provide a unified abstraction for kernel resources accessible from userspace.
//! Each process has its own handle table mapping handle IDs to resources.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use crate::process::{ProcessId, info::ProcessInfo, waker::Waker};
use crate::vfs::File;

/// Handle identifier (similar to file descriptor but for any resource)
pub type HandleId = u32;

/// A kernel resource accessible via a handle.
pub enum Handle {
    /// An open file
    File(Box<dyn File>),
    /// A child process handle
    Process(ProcessHandle),
}

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

/// Per-process handle table mapping handle IDs to resources.
pub struct HandleTable {
    handles: BTreeMap<HandleId, Handle>,
    next_id: HandleId,
}

impl HandleTable {
    /// Create a new empty handle table.
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            next_id: 3, // 0, 1, 2 reserved for stdin/stdout/stderr
        }
    }

    /// Insert a handle and return its ID.
    pub fn insert(&mut self, handle: Handle) -> HandleId {
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(id, handle);
        id
    }

    /// Get a reference to a handle.
    pub fn get(&self, id: HandleId) -> Option<&Handle> {
        self.handles.get(&id)
    }

    /// Get a mutable reference to a handle.
    pub fn get_mut(&mut self, id: HandleId) -> Option<&mut Handle> {
        self.handles.get_mut(&id)
    }

    /// Get a mutable reference to a file handle, returning None if not a file.
    pub fn get_file_mut(&mut self, id: HandleId) -> Option<&mut dyn File> {
        match self.handles.get_mut(&id)? {
            Handle::File(f) => Some(f.as_mut()),
            _ => None,
        }
    }

    /// Get a reference to a process handle, returning None if not a process.
    pub fn get_process(&self, id: HandleId) -> Option<&ProcessHandle> {
        match self.handles.get(&id)? {
            Handle::Process(p) => Some(p),
            _ => None,
        }
    }

    /// Remove a handle by ID.
    pub fn remove(&mut self, id: HandleId) -> Option<Handle> {
        self.handles.remove(&id)
    }

    /// Remove a file handle, returning None if not a file.
    pub fn remove_file(&mut self, id: HandleId) -> Option<Box<dyn File>> {
        match self.handles.get(&id)? {
            Handle::File(_) => match self.handles.remove(&id)? {
                Handle::File(f) => Some(f),
                _ => unreachable!(),
            },
            _ => None,
        }
    }

    /// Remove a process handle, returning None if not a process.
    pub fn remove_process(&mut self, id: HandleId) -> Option<ProcessHandle> {
        match self.handles.get(&id)? {
            Handle::Process(_) => match self.handles.remove(&id)? {
                Handle::Process(p) => Some(p),
                _ => unreachable!(),
            },
            _ => None,
        }
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}
