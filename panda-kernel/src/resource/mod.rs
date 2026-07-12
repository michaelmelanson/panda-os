//! Resource abstraction and interface traits.
//!
//! Resources are kernel objects that can be accessed via handles from userspace.
//! Each resource implements one or more focused interface traits.

use panda_abi::HandleType;

mod block;
mod buffer;
mod channel;
mod char_output;
mod directory;
mod event_source;
mod mailbox;
mod process;
pub(crate) mod scheme;
mod spawn_handle;
mod surface;
mod window;

pub use block::{BlockDevice, BlockError};
pub use buffer::{Buffer, BufferError, BufferExt, SharedBuffer};
pub use channel::{ChannelEndpoint, ChannelError};
pub use char_output::{CharOutError, CharacterOutput};
pub use directory::{DirEntry, Directory};
pub use event_source::{Event, EventSource, KeyEvent};
pub use mailbox::{Mailbox, MailboxRef};
pub use process::Process as ProcessInterface;
pub use scheme::{
    ConsoleScheme, DirectoryResource, FileScheme, KeyboardResource, KeyboardScheme, OpenError,
    SchemeHandler, init as init_schemes, open, readdir, register_scheme,
};
pub use spawn_handle::SpawnHandle;
pub use surface::{
    FramebufferSurface, PixelFormat, Rect, Surface, SurfaceError, SurfaceInfo, alpha_blend,
    get_framebuffer_surface, init_framebuffer,
};
pub use window::WindowResource;

use alloc::boxed::Box;
use alloc::sync::Arc;
use spinning_top::Spinlock;

use crate::process::waker::IoWaker;
use crate::vfs;

/// A VFS file that can be accessed asynchronously.
pub trait VfsFile: Send + Sync {
    /// Get a reference to the underlying async File.
    fn file(&self) -> &Spinlock<Box<dyn vfs::File>>;
}

/// A kernel resource that can be accessed via handles.
///
/// Resources implement one or more focused interface traits (EventSource, etc.).
/// The `as_*` methods allow dynamic dispatch to the appropriate interface.
pub trait Resource: Send + Sync {
    /// Get the handle type tag for this resource.
    /// This is used to tag handles returned to userspace for runtime type checking.
    fn handle_type(&self) -> HandleType;

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

    /// Get this resource as a Surface (for framebuffer, display).
    fn as_surface(&self) -> Option<&dyn Surface> {
        None
    }

    /// Get a waker for blocking on this resource, if applicable.
    fn waker(&self) -> Option<Arc<IoWaker>> {
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

    /// Get this resource as a Channel (for message-based IPC).
    fn as_channel(&self) -> Option<&ChannelEndpoint> {
        None
    }

    /// Get this resource as a Mailbox (for event aggregation).
    fn as_mailbox(&self) -> Option<&Mailbox> {
        None
    }

    /// What events this resource type can generate.
    /// Returns a bitmask of EVENT_* flags from panda_abi.
    fn supported_events(&self) -> u32 {
        0
    }

    /// Current pending events for this resource.
    /// Returns a bitmask of EVENT_* flags that are currently active.
    fn poll_events(&self) -> u32 {
        0
    }

    /// Attach a mailbox to receive events from this resource.
    /// Resources that generate events should override this.
    fn attach_mailbox(&self, _mailbox_ref: MailboxRef) {
        // Default: do nothing
    }
}

/// Whether `resource` may be attached to a channel message and transferred
/// into the receiver's handle table (see `syscall/channel.rs`'s `handle_send`
/// and `handle_recv`, and docs/SYSCALLS.md "Handle transfer").
///
/// Whitelisted by concrete resource identity — `handle_type()` paired with
/// the matching `as_*` interface — rather than merely "implements
/// `as_channel()`". [`SpawnHandle`] also implements `as_channel()` (a
/// process handle doubles as a channel handle to the child so callers can
/// `send`/`recv` on it directly), but it additionally carries process-coupled
/// state — exit-code delivery, the process waker, `wait()` semantics — that
/// isn't yet defined for a resource installed into a *different* process's
/// handle table. Until that's designed, only plain channel endpoints and
/// shared buffers are transferable.
pub fn is_transferable(resource: &Arc<dyn Resource>) -> bool {
    match resource.handle_type() {
        HandleType::Buffer => resource.as_shared_buffer().is_some(),
        HandleType::Channel => resource.as_channel().is_some(),
        _ => false,
    }
}

/// Errors that can occur while loading a binary from a resource URI.
#[derive(Debug)]
pub enum LoadBinaryError {
    /// No resource exists at the given URI.
    NotFound,
    /// The resource exists but does not support reading as a file.
    NotReadable,
    /// An I/O error occurred while stat-ing or reading the file.
    IoError,
}

/// Open a resource URI and read its entire contents into a leaked, `'static`
/// byte slice suitable for zero-copy process creation via
/// `Process::from_elf_data`.
///
/// This is the single path used to load an ELF binary for a new process,
/// whether spawning a child process at runtime or loading the very first
/// process at boot. Callers that need to drive this to completion before the
/// kernel task executor is running (e.g. boot) can use
/// `executor::block_on_immediate`.
pub async fn load_binary(uri: &str) -> Result<*const [u8], LoadBinaryError> {
    let resource = open(uri).await.map_err(|_| LoadBinaryError::NotFound)?;
    let vfs_file = resource
        .as_vfs_file()
        .ok_or(LoadBinaryError::NotReadable)?;

    let file_lock = vfs_file.file();
    let mut file = file_lock.lock();

    let stat = file.stat().await.map_err(|_| LoadBinaryError::IoError)?;
    let size = stat.size as usize;

    let mut data = alloc::vec![0u8; size];
    let mut total_read = 0;
    while total_read < size {
        match file.read(&mut data[total_read..]).await {
            Ok(0) => break, // EOF
            Ok(n) => total_read += n,
            Err(_) => return Err(LoadBinaryError::IoError),
        }
    }

    if total_read != size {
        return Err(LoadBinaryError::IoError);
    }

    let boxed = data.into_boxed_slice();
    Ok(Box::leak(boxed))
}
