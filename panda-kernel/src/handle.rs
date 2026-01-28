//! Handle-based resource management.
//!
//! Handles provide a unified abstraction for kernel resources accessible from userspace.
//! Each process has its own handle table mapping handle IDs to resources.
//!
//! Handle format: `[8 bits: type tag][24 bits: handle id]`
//! The type tag allows userspace to verify handle types at runtime.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use panda_abi::HandleType;

use crate::process::waker::Waker;
use crate::resource::{
    Buffer, CharacterOutput, Directory, EventSource, ProcessInterface, Resource, Surface, VfsFile,
};

/// Handle identifier (similar to file descriptor but for any resource).
/// Includes type tag in high 8 bits, handle ID in low 24 bits.
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

    /// Get this handle's resource as a VFS file (for async file operations).
    pub fn as_vfs_file(&self) -> Option<&dyn VfsFile> {
        self.resource.as_vfs_file()
    }

    /// Get this handle's resource as a Window.
    pub fn as_window(&self) -> Option<Arc<spinning_top::Spinlock<crate::compositor::Window>>> {
        self.resource.as_window()
    }

    /// Get this handle's resource as a Channel (for message-based IPC).
    pub fn as_channel(&self) -> Option<&crate::resource::ChannelEndpoint> {
        self.resource.as_channel()
    }

    /// Get this handle's resource as a Mailbox (for event aggregation).
    pub fn as_mailbox(&self) -> Option<&crate::resource::Mailbox> {
        self.resource.as_mailbox()
    }

    /// Get this handle's resource as a Keyboard (for keyboard devices).
    pub fn as_keyboard(&self) -> Option<&crate::resource::KeyboardResource> {
        self.resource.as_keyboard()
    }

    /// Get supported events for this resource.
    pub fn supported_events(&self) -> u32 {
        self.resource.supported_events()
    }

    /// Get current pending events for this resource.
    pub fn poll_events(&self) -> u32 {
        self.resource.poll_events()
    }

    /// Attach a mailbox to receive events from this resource.
    pub fn attach_mailbox(&self, mailbox_ref: crate::resource::MailboxRef) {
        self.resource.attach_mailbox(mailbox_ref)
    }
}

/// Per-process handle table mapping handle IDs to resources.
pub struct HandleTable {
    handles: BTreeMap<HandleId, Handle>,
    next_id: u32,
}

impl HandleTable {
    /// Create a new empty handle table.
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            // 0-6 reserved for well-known handles:
            // 0=stdin, 1=stdout, 2=stderr, 3=process, 4=env, 5=mailbox, 6=parent
            next_id: 7,
        }
    }

    /// Insert a resource at a specific handle ID (already tagged).
    /// Used for well-known handles like HANDLE_MAILBOX and HANDLE_PARENT.
    pub fn insert_at(&mut self, id: HandleId, resource: Arc<dyn Resource>) {
        self.handles.insert(id, Handle::new(resource));
    }

    /// Insert a resource with the specified type tag and return its tagged handle ID.
    pub fn insert_typed(
        &mut self,
        handle_type: HandleType,
        resource: Arc<dyn Resource>,
    ) -> HandleId {
        let id = self.next_id;
        self.next_id += 1;
        let tagged_id = handle_type.make_handle(id);
        self.handles.insert(tagged_id, Handle::new(resource));
        tagged_id
    }

    /// Insert a resource using its self-reported type and return its tagged handle ID.
    pub fn insert(&mut self, resource: Arc<dyn Resource>) -> HandleId {
        let handle_type = resource.handle_type();
        self.insert_typed(handle_type, resource)
    }

    /// Get a reference to a handle by its tagged ID.
    pub fn get(&self, id: HandleId) -> Option<&Handle> {
        self.handles.get(&id)
    }

    /// Get a mutable reference to a handle by its tagged ID.
    pub fn get_mut(&mut self, id: HandleId) -> Option<&mut Handle> {
        self.handles.get_mut(&id)
    }

    /// Remove a handle by its tagged ID.
    pub fn remove(&mut self, id: HandleId) -> Option<Handle> {
        self.handles.remove(&id)
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}
