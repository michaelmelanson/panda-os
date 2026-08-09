//! Resource abstraction and interface traits.
//!
//! Resources are kernel objects that can be accessed via handles from userspace.
//! Each resource implements one or more focused interface traits.

use panda_abi::HandleType;

mod buffer;
mod channel;
mod char_output;
pub mod device;
mod directory;
mod event_source;
pub(crate) mod initrd;
mod mailbox;
mod process;
pub(crate) mod scheme;
mod spawn_handle;

pub use buffer::{Buffer, BufferError, BufferExt, SharedBuffer};
pub use channel::{ChannelEndpoint, ChannelError};
pub use char_output::{CharOutError, CharacterOutput};
pub use device::{DeviceResource, DeviceResourceError, SubscriptionResource};
pub use directory::{DirEntry, Directory};
pub use event_source::{Event, EventSource, KeyEvent};
pub use initrd::InitrdScheme;
pub use mailbox::{Mailbox, MailboxRef};
pub use process::{Process as ProcessInterface, ProcessError};
pub use scheme::{
    ConsoleScheme, DirectoryResource, OpenError, SchemeHandler, SchemeProxyResource,
    UserSchemeProvider, connect, create, init as init_schemes, mkdir, open, readdir,
    register_initrd_scheme, register_scheme, register_user_scheme, rmdir, unlink,
};
pub(crate) use scheme::unregister_scheme_if_present;
pub use spawn_handle::SpawnHandle;

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

    /// Get a waker for blocking on this resource, if applicable.
    fn waker(&self) -> Option<Arc<IoWaker>> {
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

    /// Get this resource as a userspace scheme provider's proxy resource
    /// (M2.2). Only `scheme::SchemeProxyResource` implements this — no
    /// existing interface trait fits a provider-backed, non-VFS,
    /// non-channel file-like resource, so this follows the same
    /// one-accessor-per-concrete-capability idiom as `as_channel`/
    /// `as_directory` rather than routing it through VFS or `Any`-style
    /// downcasting (neither of which this codebase uses).
    fn as_scheme_proxy(&self) -> Option<&scheme::SchemeProxyResource> {
        None
    }

    /// Get this resource as an owned device handle (`OP_DEVICE_CLAIM`'s
    /// result). Follows the same one-accessor-per-concrete-capability idiom
    /// as `as_scheme_proxy`.
    fn as_device(&self) -> Option<&DeviceResource> {
        None
    }

    /// Get this resource as an `OP_DEVICE_SUBSCRIBE` subscription handle
    /// (`OP_DEVICE_POLL_EVENT`'s target).
    fn as_device_subscription(&self) -> Option<&SubscriptionResource> {
        None
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

    // Provider-backed resources (`file:`, served by `userspace/services/fs`)
    // have no kernel-side `vfs::File` to read through — every read is a
    // round trip to the provider. Only the initrd (`initrd:`, still
    // kernel-resident) takes the VFS path below.
    if let Some(proxy) = resource.as_scheme_proxy() {
        return load_binary_from_proxy(proxy).await;
    }

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

/// [`load_binary`]'s path for a resource served by a userspace scheme
/// provider: `Request::Stat` for the size, then `Request::Read` round trips
/// at increasing offsets until the whole file is in hand.
///
/// Reads are capped per round trip at
/// `panda_abi::scheme_protocol::MAX_TRANSFER_SIZE` (one frame's worth), so
/// this loops where the VFS path could hand the whole buffer to a single
/// `read`.
async fn load_binary_from_proxy(
    proxy: &scheme::SchemeProxyResource,
) -> Result<*const [u8], LoadBinaryError> {
    let (size, is_dir) = proxy.stat().await.map_err(|_| LoadBinaryError::IoError)?;
    if is_dir {
        return Err(LoadBinaryError::NotReadable);
    }
    let size = size as usize;

    let mut data = alloc::vec![0u8; size];
    let mut total_read = 0;
    while total_read < size {
        let want = (size - total_read).min(panda_abi::scheme_protocol::MAX_TRANSFER_SIZE);
        let chunk = proxy
            .read_at(total_read as u64, want, false)
            .await
            .map_err(|_| LoadBinaryError::IoError)?;
        if chunk.is_empty() {
            break; // EOF earlier than `stat` promised
        }
        let n = chunk.len().min(size - total_read);
        data[total_read..total_read + n].copy_from_slice(&chunk[..n]);
        total_read += n;
    }

    if total_read != size {
        return Err(LoadBinaryError::IoError);
    }

    Ok(Box::leak(data.into_boxed_slice()))
}
