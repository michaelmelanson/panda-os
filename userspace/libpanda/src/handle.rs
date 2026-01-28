//! Handle abstraction for kernel resources.
//!
//! This module provides both untyped handles (for backwards compatibility)
//! and a typed handle system for compile-time safety.

use core::marker::PhantomData;

// =============================================================================
// Untyped Handle (backwards compatible)
// =============================================================================

/// A handle to a kernel resource.
///
/// Handles are process-local identifiers for resources managed by the kernel.
/// Similar to file descriptors in Unix-like systems.
///
/// For type-safe handles, see `TypedHandle<T>`.
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

    /// Well-known handle for standard input.
    /// Only valid if the process was spawned with stdin redirection.
    pub const STDIN: Handle = Handle(panda_abi::HANDLE_STDIN);

    /// Well-known handle for standard output.
    /// Only valid if the process was spawned with stdout redirection.
    pub const STDOUT: Handle = Handle(panda_abi::HANDLE_STDOUT);

    /// Well-known handle to the current process.
    pub const PROCESS: Handle = Handle(panda_abi::HANDLE_PROCESS);

    /// Alias for PROCESS (for backwards compatibility).
    pub const SELF: Handle = Self::PROCESS;

    /// Well-known handle to the system environment.
    pub const ENVIRONMENT: Handle = Handle(panda_abi::HANDLE_ENVIRONMENT);

    /// Well-known handle to the process's default mailbox.
    pub const MAILBOX: Handle = Handle(panda_abi::HANDLE_MAILBOX);

    /// Well-known handle to the channel connected to the parent process.
    /// Only valid if this process was spawned by another process.
    pub const PARENT: Handle = Handle(panda_abi::HANDLE_PARENT);

    /// Get the parent channel handle, if this process has a parent.
    ///
    /// Returns `Some(Handle::PARENT)` for processes spawned by another process,
    /// or `None` for the init process.
    ///
    /// Note: Currently this always returns `Some` - the caller should handle
    /// communication failures gracefully if there is no actual parent.
    #[inline]
    pub fn parent() -> Option<Self> {
        // For now, we assume HANDLE_PARENT is always valid
        // The init process should handle communication errors gracefully
        Some(Self::PARENT)
    }
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

// =============================================================================
// Typed Handle System
// =============================================================================

mod private {
    /// Sealed trait to prevent external implementations of HandleKind.
    pub trait Sealed {}
}

/// Marker trait for handle types.
///
/// This trait is sealed and cannot be implemented outside this crate.
pub trait HandleKind: private::Sealed {
    /// Human-readable name for this handle kind.
    const NAME: &'static str;
}

/// A type-safe handle wrapper.
///
/// `TypedHandle<T>` wraps a raw handle ID with a phantom type parameter
/// to prevent mixing up different handle types at compile time.
///
/// # Example
/// ```
/// use libpanda::handle::{TypedHandle, File, Surface};
///
/// fn read_file(file: TypedHandle<File>) { /* ... */ }
/// fn blit_surface(surface: TypedHandle<Surface>) { /* ... */ }
///
/// // This would be a compile error:
/// // read_file(surface_handle);
/// ```
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TypedHandle<T: HandleKind> {
    id: u32,
    _marker: PhantomData<T>,
}

impl<T: HandleKind> TypedHandle<T> {
    /// Create a typed handle from a raw handle ID.
    ///
    /// # Safety
    /// The caller must ensure the handle ID refers to a resource of type `T`.
    #[inline]
    pub const unsafe fn from_raw(id: u32) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// Get the raw handle ID.
    #[inline]
    pub const fn as_raw(&self) -> u32 {
        self.id
    }

    /// Convert to an untyped Handle.
    #[inline]
    pub const fn into_untyped(self) -> Handle {
        Handle(self.id)
    }

    /// Create from an untyped Handle.
    ///
    /// # Safety
    /// The caller must ensure the handle refers to a resource of type `T`.
    #[inline]
    pub const unsafe fn from_untyped(handle: Handle) -> Self {
        Self {
            id: handle.0,
            _marker: PhantomData,
        }
    }
}

impl<T: HandleKind> Clone for TypedHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: HandleKind> Copy for TypedHandle<T> {}

impl<T: HandleKind> From<TypedHandle<T>> for Handle {
    fn from(typed: TypedHandle<T>) -> Handle {
        Handle(typed.id)
    }
}

impl<T: HandleKind> From<TypedHandle<T>> for u32 {
    fn from(typed: TypedHandle<T>) -> u32 {
        typed.id
    }
}

// =============================================================================
// Handle Kind Markers
// =============================================================================

/// Marker type for file handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum File {}
impl private::Sealed for File {}
impl HandleKind for File {
    const NAME: &'static str = "File";
}

/// Marker type for directory handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directory {}
impl private::Sealed for Directory {}
impl HandleKind for Directory {
    const NAME: &'static str = "Directory";
}

/// Marker type for surface handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {}
impl private::Sealed for Surface {}
impl HandleKind for Surface {
    const NAME: &'static str = "Surface";
}

/// Marker type for process handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Process {}
impl private::Sealed for Process {}
impl HandleKind for Process {
    const NAME: &'static str = "Process";
}

/// Marker type for channel handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {}
impl private::Sealed for Channel {}
impl HandleKind for Channel {
    const NAME: &'static str = "Channel";
}

/// Marker type for mailbox handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxKind {}
impl private::Sealed for MailboxKind {}
impl HandleKind for MailboxKind {
    const NAME: &'static str = "Mailbox";
}

/// Marker type for buffer handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Buffer {}
impl private::Sealed for Buffer {}
impl HandleKind for Buffer {
    const NAME: &'static str = "Buffer";
}

// =============================================================================
// Type aliases for convenience
// =============================================================================

/// A typed file handle.
pub type FileHandle = TypedHandle<File>;

/// A typed directory handle.
pub type DirectoryHandle = TypedHandle<Directory>;

/// A typed surface handle.
pub type SurfaceHandle = TypedHandle<Surface>;

/// A typed process handle.
pub type ProcessHandle = TypedHandle<Process>;

/// A typed channel handle.
pub type ChannelHandle = TypedHandle<Channel>;

/// A typed mailbox handle.
pub type MailboxHandle = TypedHandle<MailboxKind>;

/// A typed buffer handle.
pub type BufferHandle = TypedHandle<Buffer>;

// =============================================================================
// Well-known typed handles
// =============================================================================

impl TypedHandle<Process> {
    /// Get a handle to the current process.
    #[inline]
    pub const fn current() -> Self {
        Self {
            id: panda_abi::HANDLE_PROCESS,
            _marker: PhantomData,
        }
    }
}

impl TypedHandle<MailboxKind> {
    /// Get the default mailbox handle.
    #[inline]
    pub const fn default_mailbox() -> Self {
        Self {
            id: panda_abi::HANDLE_MAILBOX,
            _marker: PhantomData,
        }
    }
}

impl TypedHandle<Channel> {
    /// Get the parent channel handle.
    ///
    /// Returns `None` if this process has no parent (e.g., init process).
    #[inline]
    pub fn parent() -> Option<Self> {
        // HANDLE_PARENT is always valid if the process was spawned by another
        // For now, we assume it's always valid - the caller should handle
        // communication failures gracefully
        Some(Self {
            id: panda_abi::HANDLE_PARENT,
            _marker: PhantomData,
        })
    }
}
