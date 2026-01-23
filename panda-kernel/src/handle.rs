//! Handle-based resource management.
//!
//! Handles provide a unified abstraction for kernel resources accessible from userspace.
//! Each process has its own handle table mapping handle IDs to resources.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use crate::process::waker::Waker;
use crate::resource::{
    Block, BlockDevice, Buffer, CharacterOutput, Directory, EventSource, ProcessInterface,
    Resource, Surface,
};

/// Handle identifier (similar to file descriptor but for any resource)
pub type HandleId = u32;

/// A kernel resource handle with per-handle state.
pub struct Handle {
    /// The underlying resource.
    resource: Arc<dyn Resource>,
    /// Current offset for block-based reads (managed per-handle).
    offset: u64,
}

impl Handle {
    /// Create a new handle wrapping a resource.
    pub fn new(resource: Arc<dyn Resource>) -> Self {
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

    /// Replace the underlying resource (used for operations like buffer resize).
    pub fn replace_resource(&mut self, new_resource: Arc<dyn Resource>) {
        self.resource = new_resource;
    }

    /// Get the underlying resource Arc (for sharing ownership).
    pub fn resource_arc(&self) -> Arc<dyn Resource> {
        self.resource.clone()
    }

    /// Get this handle's resource as a Block interface.
    pub fn as_block(&self) -> Option<&dyn Block> {
        self.resource.as_block()
    }

    /// Get this handle's resource as a BlockDevice interface (for async I/O).
    pub fn as_block_device(&self) -> Option<&dyn BlockDevice> {
        self.resource.as_block_device()
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

    /// Get this handle's resource as a Buffer interface.
    pub fn as_buffer(&self) -> Option<&dyn Buffer> {
        self.resource.as_buffer()
    }

    /// Get this handle's resource as a Surface interface.
    pub fn as_surface(&self) -> Option<&dyn Surface> {
        self.resource.as_surface()
    }

    /// Get a waker for blocking on this handle.
    pub fn waker(&self) -> Option<Arc<Waker>> {
        self.resource.waker()
    }

    /// Get this handle's resource as a Window.
    pub fn as_window(&self) -> Option<Arc<spinning_top::Spinlock<crate::compositor::Window>>> {
        self.resource.as_window()
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
    pub fn insert(&mut self, resource: Arc<dyn Resource>) -> HandleId {
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
