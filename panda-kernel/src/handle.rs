//! Handle-based resource management.
//!
//! Handles provide a unified abstraction for kernel resources accessible from userspace.
//! Each process has its own handle table.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;

use crate::vfs::Resource;

/// Handle identifier (similar to file descriptor but for any resource)
pub type HandleId = u32;

/// Per-process handle table
pub struct HandleTable {
    handles: BTreeMap<HandleId, Box<dyn Resource>>,
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

    /// Insert a resource and return its handle ID
    pub fn insert(&mut self, resource: Box<dyn Resource>) -> HandleId {
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(id, resource);
        id
    }

    /// Get a reference to a resource by handle ID
    pub fn get(&self, id: HandleId) -> Option<&dyn Resource> {
        self.handles.get(&id).map(|r| r.as_ref())
    }

    /// Get a mutable reference to a resource by handle ID
    pub fn get_mut(&mut self, id: HandleId) -> Option<&mut Box<dyn Resource>> {
        self.handles.get_mut(&id)
    }

    /// Remove and return a resource by handle ID
    pub fn remove(&mut self, id: HandleId) -> Option<Box<dyn Resource>> {
        self.handles.remove(&id)
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}
