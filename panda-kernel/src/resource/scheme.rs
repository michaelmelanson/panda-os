//! Resource scheme system for unified resource access.
//!
//! Resources are identified by URIs with a scheme and path:
//! - `file:/initrd/init` -> File via existing VFS/mount system
//! - `console:/serial/0` -> Serial console device
//!
//! The scheme identifies the resource type, and the path is the address
//! within that scheme's namespace.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use spinning_top::{RwSpinlock, Spinlock};
use x86_64::instructions::port::Port;

use crate::device_path;
use crate::devices::claims::{ClaimGuard, ClaimOwner};
use crate::devices::virtio_block;
use crate::devices::virtio_keyboard::{self, VirtioKeyboard};
use crate::process::waker::IoWaker;
use crate::vfs;

use super::Resource;
use super::char_output::{CharOutError, CharacterOutput};
use super::directory::{DirEntry, Directory};
use super::event_source::{Event, EventSource, KeyEvent};

/// Error returned when opening a resource via a scheme fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// No resource exists at the given URI.
    NotFound,
    /// The resource exists but is exclusively claimed by another owner
    /// (see `crate::devices::claims`).
    Busy,
}

/// A handler for a resource scheme (e.g., "file", "console", "pci")
#[async_trait]
pub trait SchemeHandler: Send + Sync {
    /// Open a resource at the given path within this scheme
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError>;

    /// List directory contents at the given path within this scheme
    async fn readdir(&self, _path: &str) -> Option<Vec<DirEntry>> {
        None
    }
}

/// Global registry of scheme handlers.
///
/// Keyed by owned `String` rather than `&'static str`: M2.2 (userspace
/// providers) needs to register scheme names discovered at runtime, which
/// don't have `'static` lifetimes. Callers registering compile-time scheme
/// names (string literals) are unaffected — `register_scheme` accepts
/// anything convertible to `String`.
static SCHEMES: RwSpinlock<BTreeMap<String, Arc<dyn SchemeHandler>>> =
    RwSpinlock::new(BTreeMap::new());

/// Register a scheme handler.
pub fn register_scheme(name: impl Into<String>, handler: Arc<dyn SchemeHandler>) {
    let mut schemes = SCHEMES.write();
    schemes.insert(name.into(), handler);
}

/// List the names of all currently registered schemes, sorted.
///
/// `BTreeMap` already iterates in key order, so this is sorted "for free" —
/// no separate sort step needed.
pub fn scheme_names() -> Vec<String> {
    SCHEMES.read().keys().cloned().collect()
}

/// Open a resource by URI (e.g., "file:/initrd/init" or "console:/serial/0")
pub async fn open(uri: &str) -> Result<Box<dyn Resource>, OpenError> {
    let (scheme, path) = uri.split_once(':').ok_or(OpenError::NotFound)?;
    // Clone the handler to avoid holding the lock across await
    let handler: Arc<dyn SchemeHandler> = {
        let schemes = SCHEMES.read();
        schemes
            .get(scheme)
            .map(|h| Arc::clone(h))
            .ok_or(OpenError::NotFound)?
    };
    handler.open(path).await
}

/// List directory contents by URI (e.g., "file:/initrd")
pub async fn readdir(uri: &str) -> Option<Vec<DirEntry>> {
    let (scheme, path) = uri.split_once(':')?;

    // Clone the handler to avoid holding the lock across await
    let handler: Arc<dyn SchemeHandler> = {
        let schemes = SCHEMES.read();
        schemes.get(scheme).map(|h| Arc::clone(h))?
    };
    handler.readdir(path).await
}

// =============================================================================
// File Scheme - wraps existing VFS
// =============================================================================

/// Scheme handler that wraps the existing VFS mount system
pub struct FileScheme;

#[async_trait]
impl SchemeHandler for FileScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        // Try it as a directory first. This resolves the path with a single
        // lookup: both ext2 and TarFs fail `readdir` with `NotFound` for a
        // path that names a file (or doesn't exist), so a non-directory
        // falls straight through to the file-open path below without a
        // separate `stat` call to decide which lookup to do.
        if let Ok(entries) = vfs::readdir(path).await {
            // Return a directory resource with VFS path for mutation support
            return Ok(Box::new(DirectoryResource::with_vfs_path(
                entries,
                alloc::string::String::from(path),
            )));
        }

        // Open as a file
        let file = vfs::open(path).await.map_err(|_| OpenError::NotFound)?;
        Ok(Box::new(VfsFileResource::new(file)))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        vfs::readdir(path).await.ok()
    }
}

/// A file resource wrapping a VFS file.
pub struct VfsFileResource {
    file: Spinlock<Box<dyn vfs::File>>,
}

impl VfsFileResource {
    pub fn new(file: Box<dyn vfs::File>) -> Self {
        Self {
            file: Spinlock::new(file),
        }
    }
}

impl Resource for VfsFileResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::File
    }

    fn as_vfs_file(&self) -> Option<&dyn super::VfsFile> {
        Some(self)
    }
}

impl super::VfsFile for VfsFileResource {
    fn file(&self) -> &Spinlock<Box<dyn vfs::File>> {
        &self.file
    }
}

/// A directory resource.
///
/// If `vfs_path` is set, this directory supports mutation operations
/// (create, unlink) via the VFS layer.
pub struct DirectoryResource {
    entries: Vec<DirEntry>,
    /// Absolute VFS path of this directory, if backed by a VFS filesystem.
    vfs_path: Option<alloc::string::String>,
}

impl DirectoryResource {
    pub fn new(entries: Vec<DirEntry>) -> Self {
        Self {
            entries,
            vfs_path: None,
        }
    }

    /// Create a directory resource with an associated VFS path for mutation.
    pub fn with_vfs_path(entries: Vec<DirEntry>, path: alloc::string::String) -> Self {
        Self {
            entries,
            vfs_path: Some(path),
        }
    }
}

impl Resource for DirectoryResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Directory
    }

    fn as_directory(&self) -> Option<&dyn Directory> {
        Some(self)
    }
}

impl Directory for DirectoryResource {
    fn entry(&self, index: usize) -> Option<DirEntry> {
        self.entries.get(index).cloned()
    }

    fn count(&self) -> usize {
        self.entries.len()
    }

    fn vfs_path(&self) -> Option<alloc::string::String> {
        self.vfs_path.clone()
    }
}

// =============================================================================
// Console Scheme - serial console access
// =============================================================================

/// Scheme handler for console devices
pub struct ConsoleScheme;

#[async_trait]
impl SchemeHandler for ConsoleScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        match path {
            "/serial/0" => Ok(Box::new(SerialConsoleResource::new(0x3f8))),
            _ => Err(OpenError::NotFound),
        }
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        match path {
            "/" => Some(alloc::vec![DirEntry {
                name: alloc::string::String::from("serial"),
                is_dir: true,
            }]),
            "/serial" => Some(alloc::vec![DirEntry {
                name: alloc::string::String::from("0"),
                is_dir: false,
            }]),
            _ => None,
        }
    }
}

/// A serial console resource
pub struct SerialConsoleResource {
    port: u16,
}

impl SerialConsoleResource {
    pub fn new(port: u16) -> Self {
        Self { port }
    }
}

impl Resource for SerialConsoleResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        // Console is accessed like a file
        panda_abi::HandleType::File
    }

    fn as_char_output(&self) -> Option<&dyn CharacterOutput> {
        Some(self)
    }
}

impl CharacterOutput for SerialConsoleResource {
    fn write(&self, buf: &[u8]) -> Result<usize, CharOutError> {
        for &byte in buf {
            unsafe {
                Port::new(self.port).write(byte);
            }
        }
        Ok(buf.len())
    }
}

// =============================================================================
// Keyboard Scheme - virtio keyboard access
// =============================================================================

/// Scheme handler for keyboard devices
pub struct KeyboardScheme;

#[async_trait]
impl SchemeHandler for KeyboardScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        // Resolve path like "/pci/input/0" or "/pci/00:03.0"
        let address = device_path::resolve(path).ok_or(OpenError::NotFound)?;
        let keyboard = virtio_keyboard::get_keyboard(&address).ok_or(OpenError::NotFound)?;
        Ok(Box::new(KeyboardResource { keyboard }))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        device_path::list(path)
    }
}

/// Handle to an open keyboard device
pub struct KeyboardResource {
    keyboard: Arc<Spinlock<VirtioKeyboard>>,
}

impl Resource for KeyboardResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        // Keyboard is accessed like a file (read events)
        panda_abi::HandleType::File
    }

    fn as_event_source(&self) -> Option<&dyn EventSource> {
        Some(self)
    }

    fn waker(&self) -> Option<Arc<IoWaker>> {
        Some(self.keyboard.lock().waker())
    }

    fn supported_events(&self) -> u32 {
        panda_abi::EVENT_KEYBOARD_KEY
    }

    fn poll_events(&self) -> u32 {
        // The ring buffer only exposes a has-pending check, not per-event
        // peeking, so we report the coarse "at least one key event is
        // buffered" state rather than inventing additional buffering here.
        // Mailbox delivery of individual key events still happens eagerly
        // via the wake/post mechanism in `VirtioKeyboard::poll`, independent
        // of this method.
        if self.keyboard.lock().has_events() {
            panda_abi::EVENT_KEYBOARD_KEY
        } else {
            0
        }
    }

    fn attach_mailbox(&self, mailbox_ref: super::MailboxRef) {
        self.keyboard.lock().attach_mailbox(mailbox_ref);
    }
}

impl EventSource for KeyboardResource {
    fn poll(&self) -> Option<Event> {
        let mut kb = self.keyboard.lock();
        kb.pop_event().map(|event| {
            Event::Key(KeyEvent {
                code: event.code,
                value: event.value,
            })
        })
    }

    fn waker(&self) -> Arc<IoWaker> {
        self.keyboard.lock().waker()
    }
}

// =============================================================================
// Surface Scheme - window compositor access
// =============================================================================

/// Scheme handler for surface devices
pub struct SurfaceScheme;

#[async_trait]
impl SchemeHandler for SurfaceScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        match path {
            "/window" => {
                let window = crate::compositor::create_window();
                Ok(Box::new(super::window::WindowResource { window }))
            }
            "/fb0" => {
                // The legacy "/fb0" path doesn't go through `device_path::resolve`
                // (it isn't a "/pci/..." path), so resolve the display's
                // DeviceAddress the same way a "/pci/display/0" open would:
                // the first PCI device in the display class.
                let address = crate::pci::get_device_by_class(
                    crate::pci::DeviceClass::Display.code(),
                    0,
                )
                .ok_or(OpenError::NotFound)?;
                let claim = crate::devices::claims::claim(address, ClaimOwner::Display)
                    .map_err(|_| OpenError::Busy)?;

                // NOTE (compositor bypass): the in-kernel compositor
                // (crate::compositor) reads and writes the framebuffer
                // directly via `get_framebuffer_surface()`, without going
                // through this scheme or this claim. That bypass is a known,
                // temporary gap that closes once the compositor moves to
                // userspace (see plans/userspace-compositor.md); until then,
                // this claim only protects against concurrent *userspace*
                // opens of "/fb0", not against the in-kernel compositor.
                let surface =
                    super::get_framebuffer_surface().ok_or(OpenError::NotFound)?;
                Ok(Box::new(ClaimedFramebufferSurface {
                    surface: *surface,
                    _claim: claim,
                }))
            }
            _ => Err(OpenError::NotFound),
        }
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        match path {
            "/" => Some(alloc::vec![
                DirEntry {
                    name: alloc::string::String::from("window"),
                    is_dir: false,
                },
                DirEntry {
                    name: alloc::string::String::from("fb0"),
                    is_dir: false,
                },
            ]),
            _ => None,
        }
    }
}

/// A framebuffer surface opened via `surface:/fb0`, holding the display's
/// exclusive claim for the lifetime of the returned handle.
///
/// Dropping this (on `close()`, or when a process's handle table is dropped
/// at exit) releases the claim, so a second `surface:/fb0` open can succeed.
struct ClaimedFramebufferSurface {
    surface: super::FramebufferSurface,
    _claim: ClaimGuard,
}

impl Resource for ClaimedFramebufferSurface {
    fn handle_type(&self) -> panda_abi::HandleType {
        self.surface.handle_type()
    }

    fn as_surface(&self) -> Option<&dyn super::Surface> {
        Some(&self.surface)
    }
}

// =============================================================================
// Block Scheme - block device access
// =============================================================================

/// Scheme handler for block devices (virtio-blk, future AHCI, NVMe).
///
/// Paths support both raw addresses and class-based resolution:
/// - `/pci/00:04.0` - raw PCI address
/// - `/pci/storage/0` - first storage device
pub struct BlockScheme;

#[async_trait]
impl SchemeHandler for BlockScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        // Resolve path like "/pci/storage/0" or "/pci/00:04.0"
        let address = device_path::resolve(path).ok_or(OpenError::NotFound)?;

        // Claim the device before handing out raw access to it. If a
        // filesystem is mounted on this device (see `vfs::mount_ext2`,
        // which holds a `Mount`-tagged claim for as long as the mount is
        // active), this fails with `Busy` instead of minting a second,
        // unsynchronized writer underneath the mounted filesystem.
        let claim = crate::devices::claims::claim(address.clone(), ClaimOwner::RawOpen)
            .map_err(|_| OpenError::Busy)?;

        // Try virtio-blk registry (future: try AHCI, NVMe registries too)
        let device = virtio_block::get_device(&address).ok_or(OpenError::NotFound)?;
        let device: Arc<dyn super::BlockDevice> = Arc::new(device);

        // Wrap in a VFS file for async access
        let file: Box<dyn vfs::File> = Box::new(vfs::BlockDeviceFile::new(device));
        Ok(Box::new(BlockDeviceResource {
            file: Spinlock::new(file),
            _claim: claim,
        }))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        device_path::list(path)
    }
}

/// Resource wrapper for a block device.
///
/// Block devices are exposed through the VFS file interface for async I/O.
/// Holds the device's exclusive claim for the lifetime of this resource;
/// dropping it (on `close()` or process exit) releases the claim.
struct BlockDeviceResource {
    file: Spinlock<Box<dyn vfs::File>>,
    _claim: ClaimGuard,
}

impl Resource for BlockDeviceResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::File
    }

    fn as_vfs_file(&self) -> Option<&dyn super::VfsFile> {
        Some(self)
    }
}

impl super::VfsFile for BlockDeviceResource {
    fn file(&self) -> &Spinlock<Box<dyn vfs::File>> {
        &self.file
    }
}

// =============================================================================
// Scheme Scheme - meta-scheme registry enumeration
// =============================================================================

/// Scheme handler for the `scheme:` meta-scheme.
///
/// This is the honest replacement for the removed `*:` discovery hack:
/// `readdir("scheme:/")` lists the names of every registered scheme handler
/// (including "scheme" itself, since it's registered the same way as
/// everything else via `register_scheme`).
///
/// Nothing is open-able here yet: `scheme:/<name>` is reserved namespace for
/// M2.2's userspace-provider metadata (e.g. which process backs a scheme,
/// its capabilities). Until that lands, every `open` — including the root —
/// fails `NotFound`.
pub struct SchemeScheme;

#[async_trait]
impl SchemeHandler for SchemeScheme {
    async fn open(&self, _path: &str) -> Result<Box<dyn Resource>, OpenError> {
        Err(OpenError::NotFound)
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        match path {
            "/" => Some(
                scheme_names()
                    .into_iter()
                    .map(|name| DirEntry { name, is_dir: false })
                    .collect(),
            ),
            _ => None,
        }
    }
}

// =============================================================================
// Initialization
// =============================================================================

/// Initialize the resource scheme system with default schemes
pub fn init() {
    register_scheme("file", Arc::new(FileScheme));
    register_scheme("console", Arc::new(ConsoleScheme));
    register_scheme("keyboard", Arc::new(KeyboardScheme));
    register_scheme("surface", Arc::new(SurfaceScheme));
    register_scheme("block", Arc::new(BlockScheme));
    register_scheme("scheme", Arc::new(SchemeScheme));
}
