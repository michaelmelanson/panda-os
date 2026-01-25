//! Resource abstraction and interface traits.
//!
//! Resources are kernel objects that can be accessed via handles from userspace.
//! Each resource implements one or more focused interface traits.

mod block;
mod buffer;
mod char_output;
mod directory;
mod event_source;
mod process;
mod process_resource;
mod scheme;
mod surface;
mod window;

pub use block::{Block, BlockDevice, BlockError};
pub use buffer::{Buffer, BufferError, SharedBuffer};
pub use char_output::{CharOutError, CharacterOutput};
pub use directory::{DirEntry, Directory};
pub use event_source::{Event, EventSource, KeyEvent};
pub use process::Process as ProcessInterface;
pub use process_resource::ProcessResource;
pub use scheme::{
    ConsoleScheme, DirectoryResource, FileScheme, KeyboardScheme, SchemeHandler,
    init as init_schemes, open, readdir, register_scheme,
};
pub use surface::{
    FramebufferSurface, PixelFormat, Rect, Surface, SurfaceError, SurfaceInfo, alpha_blend,
    get_framebuffer_surface, init_framebuffer,
};
pub use window::WindowResource;

use alloc::boxed::Box;
use alloc::sync::Arc;
use spinning_top::Spinlock;

use crate::process::waker::Waker;
use crate::vfs;

/// A VFS file that can be accessed asynchronously.
pub trait VfsFile: Send + Sync {
    /// Get a reference to the underlying async File.
    fn file(&self) -> &Spinlock<Box<dyn vfs::File>>;
}

/// A kernel resource that can be accessed via handles.
///
/// Resources implement one or more focused interface traits (Block, EventSource, etc.).
/// The `as_*` methods allow dynamic dispatch to the appropriate interface.
pub trait Resource: Send + Sync {
    /// Get this resource as a Block (for files, disks, memory regions).
    fn as_block(&self) -> Option<&dyn Block> {
        None
    }

    /// Get this resource as an EventSource (for keyboard, mouse, timers).
    fn as_event_source(&self) -> Option<&dyn EventSource> {
        None
    }

    /// Get this resource as a Directory (for directory listings).
    fn as_directory(&self) -> Option<&dyn Directory> {
        None
    }

    /// Get this resource as a Process (for child process handles).
    fn as_process(&self) -> Option<&dyn ProcessInterface> {
        None
    }

    /// Get this resource as a CharacterOutput (for serial console, terminal).
    fn as_char_output(&self) -> Option<&dyn CharacterOutput> {
        None
    }

    /// Get this resource as a Buffer (for shared memory regions).
    fn as_buffer(&self) -> Option<&dyn Buffer> {
        None
    }

    /// Get this resource as a mutable Buffer.
    fn as_buffer_mut(&mut self) -> Option<&mut dyn Buffer> {
        None
    }

    /// Get this resource as a Surface (for framebuffer, display).
    fn as_surface(&self) -> Option<&dyn Surface> {
        None
    }

    /// Get this resource as a mutable Surface.
    fn as_surface_mut(&mut self) -> Option<&mut dyn Surface> {
        None
    }

    /// Get a waker for blocking on this resource, if applicable.
    fn waker(&self) -> Option<Arc<Waker>> {
        None
    }

    /// Get this resource as a Window (for compositor windows).
    fn as_window(&self) -> Option<Arc<Spinlock<crate::compositor::Window>>> {
        None
    }

    /// Get this resource as a SharedBuffer Arc (for sharing buffer ownership).
    fn as_shared_buffer(&self) -> Option<Arc<SharedBuffer>> {
        None
    }

    /// Get this resource as a VFS file (for async file operations).
    fn as_vfs_file(&self) -> Option<&dyn VfsFile> {
        None
    }
}
