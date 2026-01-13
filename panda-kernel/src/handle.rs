//! Handle-based resource management.
//!
//! Handles provide a unified abstraction for kernel resources accessible from userspace.
//! Each process has its own handle table mapping handle IDs to resources.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use crate::process::waker::Waker;
use crate::resource::{Block, CharacterOutput, Directory, EventSource, ProcessInterface, Resource};

/// Handle identifier (similar to file descriptor but for any resource)
pub type HandleId = u32;

/// A kernel resource handle with per-handle state.
pub struct Handle {
    /// The underlying resource.
    resource: Box<dyn Resource>,
    /// Current offset for block-based reads (managed per-handle).
    offset: u64,
}

impl Handle {
    /// Create a new handle wrapping a resource.
    pub fn new(resource: Box<dyn Resource>) -> Self {
        Self {
            resource,
            offset: 0,
        }
    }

    /// Get the current offset.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Set the current offset.
    pub fn set_offset(&mut self, offset: u64) {
        self.offset = offset;
    }

    /// Get this handle's resource as a Block interface.
    pub fn as_block(&self) -> Option<&dyn Block> {
        self.resource.as_block()
    }

    /// Get this handle's resource as an EventSource interface.
    pub fn as_event_source(&self) -> Option<&dyn EventSource> {
        self.resource.as_event_source()
    }

    /// Get this handle's resource as a Directory interface.
    pub fn as_directory(&self) -> Option<&dyn Directory> {
        self.resource.as_directory()
    }

    /// Get this handle's resource as a Process interface.
    pub fn as_process(&self) -> Option<&dyn ProcessInterface> {
        self.resource.as_process()
    }

    /// Get this handle's resource as a CharacterOutput interface.
    pub fn as_char_output(&self) -> Option<&dyn CharacterOutput> {
        self.resource.as_char_output()
    }

    /// Get a waker for blocking on this handle.
    pub fn waker(&self) -> Option<Arc<Waker>> {
        self.resource.waker()
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

    /// Insert a resource and return its handle ID.
    pub fn insert(&mut self, resource: Box<dyn Resource>) -> HandleId {
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(id, Handle::new(resource));
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

    /// Remove a handle by ID.
    pub fn remove(&mut self, id: HandleId) -> Option<Handle> {
        self.handles.remove(&id)
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}
