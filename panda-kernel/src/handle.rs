//! Handle-based resource management.
//!
//! Handles provide a unified abstraction for kernel resources accessible from userspace.
//! Each process has its own handle table with type-safe resource access.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;

use crate::process_handle::ProcessHandle;
use crate::vfs::{File, Resource};

/// Handle identifier (similar to file descriptor but for any resource)
pub type HandleId = u32;

/// Type of resource a handle points to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    /// A file from the VFS
    File,
    /// A child process
    Process,
}

/// A handle entry with its resource and type
struct HandleEntry {
    resource: Box<dyn Resource>,
    resource_type: ResourceType,
}

/// Per-process handle table with type-safe resource access
pub struct HandleTable {
    handles: BTreeMap<HandleId, HandleEntry>,
    next_id: HandleId,
}

impl HandleTable {
    /// Create a new empty handle table
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            next_id: 3, // 0, 1, 2 reserved for stdin/stdout/stderr
        }
    }

    /// Insert a file resource and return its handle ID
    pub fn insert_file(&mut self, resource: Box<dyn Resource>) -> HandleId {
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(
            id,
            HandleEntry {
                resource,
                resource_type: ResourceType::File,
            },
        );
        id
    }

    /// Insert a process resource and return its handle ID
    pub fn insert_process(&mut self, resource: Box<dyn Resource>) -> HandleId {
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(
            id,
            HandleEntry {
                resource,
                resource_type: ResourceType::Process,
            },
        );
        id
    }

    /// Get the type of a handle, if it exists
    pub fn get_type(&self, id: HandleId) -> Option<ResourceType> {
        self.handles.get(&id).map(|e| e.resource_type)
    }

    /// Get a mutable reference to a file resource, returning None if handle
    /// doesn't exist or is not a file
    pub fn get_file_mut(&mut self, id: HandleId) -> Option<&mut dyn File> {
        let entry = self.handles.get_mut(&id)?;
        if entry.resource_type != ResourceType::File {
            return None;
        }
        entry.resource.as_file()
    }

    /// Get a reference to a process handle, returning None if handle
    /// doesn't exist or is not a process
    pub fn get_process_handle(&self, id: HandleId) -> Option<&ProcessHandle> {
        let entry = self.handles.get(&id)?;
        if entry.resource_type != ResourceType::Process {
            return None;
        }
        entry.resource.as_process_handle()
    }

    /// Remove and return a resource by handle ID, only if it matches the expected type
    pub fn remove_typed(
        &mut self,
        id: HandleId,
        expected_type: ResourceType,
    ) -> Option<Box<dyn Resource>> {
        let entry = self.handles.get(&id)?;
        if entry.resource_type != expected_type {
            return None;
        }
        self.handles.remove(&id).map(|e| e.resource)
    }

    /// Remove a resource by handle ID (any type)
    pub fn remove(&mut self, id: HandleId) -> Option<Box<dyn Resource>> {
        self.handles.remove(&id).map(|e| e.resource)
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}
