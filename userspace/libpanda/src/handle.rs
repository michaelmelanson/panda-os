//! Handle abstraction for kernel resources.

/// A handle to a kernel resource.
///
/// Handles are process-local identifiers for resources managed by the kernel.
/// Similar to file descriptors in Unix-like systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Handle(u32);

impl Handle {
    /// Create a handle from a raw handle ID.
    ///
    /// # Safety
    /// The caller must ensure the handle ID is valid for the current process.
    pub const unsafe fn from_raw(id: u32) -> Self {
        Handle(id)
    }

    /// Get the raw handle ID.
    pub const fn as_raw(self) -> u32 {
        self.0
    }

    /// Well-known handle to the current process.
    pub const SELF: Handle = Handle(panda_abi::HANDLE_SELF);

    /// Well-known handle to the system environment.
    pub const ENVIRONMENT: Handle = Handle(panda_abi::HANDLE_ENVIRONMENT);

    /// Well-known handle to the process's default mailbox.
    pub const MAILBOX: Handle = Handle(panda_abi::HANDLE_MAILBOX);

    /// Well-known handle to the channel connected to the parent process.
    /// Only valid if this process was spawned by another process.
    pub const PARENT: Handle = Handle(panda_abi::HANDLE_PARENT);
}

impl From<Handle> for u32 {
    fn from(handle: Handle) -> u32 {
        handle.0
    }
}

impl From<u32> for Handle {
    fn from(id: u32) -> Handle {
        Handle(id)
    }
}
