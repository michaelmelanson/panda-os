//! Handle-based resource management.
//!
//! Handles provide a unified abstraction for kernel resources accessible from userspace.
//! Each process has its own handle table mapping handle IDs to resources.
//!
//! Handle format: `[8 bits: type tag][56 bits: handle id]`
//! The type tag allows userspace to verify handle types at runtime.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::fmt;

use panda_abi::HandleType;

use crate::process::waker::Waker;
use crate::resource::{
    Buffer, CharacterOutput, Directory, EventSource, ProcessInterface, Resource, Surface, VfsFile,
};

/// Maximum number of open handles per process.
///
/// This limit prevents a single process from exhausting kernel memory by
/// creating handles in a tight loop. Each handle holds an `Arc<dyn Resource>`,
/// so unbounded handle creation would eventually exhaust the kernel heap.
/// 4096 is generous enough for any reasonable workload while still providing
/// protection against resource exhaustion attacks.
pub const MAX_HANDLES_PER_PROCESS: usize = 4096;

/// Errors that can occur when inserting a handle into the handle table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    /// The 56-bit handle ID space has been exhausted.
    IdSpaceExhausted,
    /// The per-process handle limit has been reached.
    TooManyHandles,
}

impl fmt::Display for HandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandleError::IdSpaceExhausted => write!(f, "handle ID space exhausted (56-bit limit)"),
            HandleError::TooManyHandles => write!(
                f,
                "per-process handle limit reached ({})",
                MAX_HANDLES_PER_PROCESS
            ),
        }
    }
}

/// Handle identifier (similar to file descriptor but for any resource).
/// Includes type tag in high 8 bits, handle ID in low 56 bits.
pub type HandleId = u64;

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
    next_id: u64,
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
    ///
    /// Returns `Err(HandleError::TooManyHandles)` if the process already has
    /// [`MAX_HANDLES_PER_PROCESS`] open handles, or `Err(HandleError::IdSpaceExhausted)`
    /// if the 56-bit handle ID counter has wrapped around.
    pub fn insert_typed(
        &mut self,
        handle_type: HandleType,
        resource: Arc<dyn Resource>,
    ) -> Result<HandleId, HandleError> {
        if self.handles.len() >= MAX_HANDLES_PER_PROCESS {
            return Err(HandleError::TooManyHandles);
        }
        let id = self.next_id;
        if id > HandleType::MAX_ID {
            return Err(HandleError::IdSpaceExhausted);
        }
        self.next_id += 1;
        let tagged_id = handle_type.make_handle(id);
        self.handles.insert(tagged_id, Handle::new(resource));
        Ok(tagged_id)
    }

    /// Insert a resource using its self-reported type and return its tagged handle ID.
    ///
    /// Returns an error if the per-process handle limit is reached or the ID space
    /// is exhausted. See [`insert_typed`](Self::insert_typed) for details.
    pub fn insert(&mut self, resource: Arc<dyn Resource>) -> Result<HandleId, HandleError> {
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
