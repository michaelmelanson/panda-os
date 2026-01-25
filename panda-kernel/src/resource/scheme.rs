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
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use spinning_top::{RwSpinlock, Spinlock};
use x86_64::instructions::port::Port;

use crate::device_path;
use crate::devices::virtio_block;
use crate::devices::virtio_keyboard::{self, VirtioKeyboard};
use crate::process::waker::Waker;
use crate::vfs;

use super::Resource;
use super::char_output::{CharOutError, CharacterOutput};
use super::directory::{DirEntry, Directory};
use super::event_source::{Event, EventSource, KeyEvent};

/// A handler for a resource scheme (e.g., "file", "console", "pci")
#[async_trait]
pub trait SchemeHandler: Send + Sync {
    /// Open a resource at the given path within this scheme
    async fn open(&self, path: &str) -> Option<Box<dyn Resource>>;

    /// List directory contents at the given path within this scheme
    async fn readdir(&self, _path: &str) -> Option<Vec<DirEntry>> {
        None
    }
}

/// Global registry of scheme handlers
static SCHEMES: RwSpinlock<BTreeMap<&'static str, Arc<dyn SchemeHandler>>> =
    RwSpinlock::new(BTreeMap::new());

/// Register a scheme handler
pub fn register_scheme(name: &'static str, handler: Arc<dyn SchemeHandler>) {
    let mut schemes = SCHEMES.write();
    schemes.insert(name, handler);
}

/// Open a resource by URI (e.g., "file:/initrd/init" or "console:/serial/0")
pub async fn open(uri: &str) -> Option<Box<dyn Resource>> {
    let (scheme, path) = uri.split_once(':')?;
    // Clone the handler to avoid holding the lock across await
    let handler: Arc<dyn SchemeHandler> = {
        let schemes = SCHEMES.read();
        schemes.get(scheme).map(|h| Arc::clone(h))?
    };
    handler.open(path).await
}

/// List directory contents by URI (e.g., "file:/initrd")
///
/// Special case: `*:/path` discovers which schemes support the given path,
/// returning each scheme name as a directory entry.
pub async fn readdir(uri: &str) -> Option<Vec<DirEntry>> {
    let (scheme, path) = uri.split_once(':')?;

    // Special case: "*" scheme discovers which schemes support this path
    if scheme == "*" {
        return Some(discover_schemes(path).await);
    }

    // Clone the handler to avoid holding the lock across await
    let handler: Arc<dyn SchemeHandler> = {
        let schemes = SCHEMES.read();
        schemes.get(scheme).map(|h| Arc::clone(h))?
    };
    handler.readdir(path).await
}

/// Discover which schemes can open a given path.
///
/// Returns a list of scheme names that successfully open the path.
/// This enables cross-scheme discovery like `*:/pci/storage/0` or `*:/serial/0`.
pub async fn discover_schemes(path: &str) -> Vec<DirEntry> {
    // Get list of all scheme names and handlers
    let handlers: Vec<(&'static str, Arc<dyn SchemeHandler>)> = {
        let schemes = SCHEMES.read();
        schemes
            .iter()
            .map(|(&name, handler)| (name, Arc::clone(handler)))
            .collect()
    };

    let mut results = Vec::new();

    for (name, handler) in handlers {
        // Try to open the path with this scheme
        if handler.open(path).await.is_some() {
            results.push(DirEntry {
                name: alloc::string::String::from(name),
                is_dir: false,
            });
        }
    }

    results
}

// =============================================================================
// File Scheme - wraps existing VFS
// =============================================================================

/// Scheme handler that wraps the existing VFS mount system
pub struct FileScheme;

#[async_trait]
impl SchemeHandler for FileScheme {
    async fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        // Check if it's a directory first
        if let Ok(stat) = vfs::stat(path).await {
            if stat.is_dir {
                // Return a directory resource
                let entries = vfs::readdir(path).await.ok()?;
                let dir_entries: Vec<DirEntry> = entries
                    .into_iter()
                    .map(|e| DirEntry {
                        name: e.name,
                        is_dir: e.is_dir,
                    })
                    .collect();
                return Some(Box::new(DirectoryResource::new(dir_entries)));
            }
        }

        // Open as a file
        let file = vfs::open(path).await.ok()?;
        Some(Box::new(VfsFileResource::new(file)))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        let entries = vfs::readdir(path).await.ok()?;
        Some(
            entries
                .into_iter()
                .map(|e| DirEntry {
                    name: e.name,
                    is_dir: e.is_dir,
                })
                .collect(),
        )
    }
}

/// A file resource wrapping a VFS file.
struct VfsFileResource {
    file: Spinlock<Box<dyn vfs::File>>,
}

impl VfsFileResource {
    fn new(file: Box<dyn vfs::File>) -> Self {
        Self {
            file: Spinlock::new(file),
        }
    }
}

impl Resource for VfsFileResource {
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
pub struct DirectoryResource {
    entries: Vec<DirEntry>,
}

impl DirectoryResource {
    pub fn new(entries: Vec<DirEntry>) -> Self {
        Self { entries }
    }
}

impl Resource for DirectoryResource {
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
}

// =============================================================================
// Console Scheme - serial console access
// =============================================================================

/// Scheme handler for console devices
pub struct ConsoleScheme;

#[async_trait]
impl SchemeHandler for ConsoleScheme {
    async fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        match path {
            "/serial/0" => Some(Box::new(SerialConsoleResource::new(0x3f8))),
            _ => None,
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
    async fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        // Resolve path like "/pci/input/0" or "/pci/00:03.0"
        let address = device_path::resolve(path)?;
        let keyboard = virtio_keyboard::get_keyboard(&address)?;
        Some(Box::new(KeyboardResource { keyboard }))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        device_path::list(path)
    }
}

/// Handle to an open keyboard device
pub struct KeyboardResource {
    keyboard: Arc<Spinlock<VirtioKeyboard>>,
}

impl KeyboardResource {
    /// Attach a mailbox to receive keyboard events.
    pub fn attach_mailbox(&self, mailbox_ref: super::MailboxRef) {
        self.keyboard.lock().attach_mailbox(mailbox_ref);
    }
}

impl Resource for KeyboardResource {
    fn as_event_source(&self) -> Option<&dyn EventSource> {
        Some(self)
    }

    fn as_keyboard(&self) -> Option<&KeyboardResource> {
        Some(self)
    }

    fn waker(&self) -> Option<Arc<Waker>> {
        Some(self.keyboard.lock().waker())
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

    fn waker(&self) -> Arc<Waker> {
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
    async fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        match path {
            "/window" => {
                let window = crate::compositor::create_window();
                Some(Box::new(super::window::WindowResource { window }))
            }
            "/fb0" => {
                // Return the global framebuffer surface
                super::get_framebuffer_surface().map(|s| Box::new(*s) as Box<dyn Resource>)
            }
            _ => None,
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
    async fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        // Resolve path like "/pci/storage/0" or "/pci/00:04.0"
        let address = device_path::resolve(path)?;

        // Try virtio-blk registry (future: try AHCI, NVMe registries too)
        let device = virtio_block::get_device(&address)?;
        let device: Arc<dyn super::BlockDevice> = Arc::new(device);

        // Wrap in a VFS file for async access
        let file: Box<dyn vfs::File> = Box::new(vfs::BlockDeviceFile::new(device));
        Some(Box::new(BlockDeviceResource {
            file: Spinlock::new(file),
        }))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        device_path::list(path)
    }
}

/// Resource wrapper for a block device.
///
/// Block devices are exposed through the VFS file interface for async I/O.
struct BlockDeviceResource {
    file: Spinlock<Box<dyn vfs::File>>,
}

impl Resource for BlockDeviceResource {
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
// Initialization
// =============================================================================

/// Initialize the resource scheme system with default schemes
pub fn init() {
    register_scheme("file", Arc::new(FileScheme));
    register_scheme("console", Arc::new(ConsoleScheme));
    register_scheme("keyboard", Arc::new(KeyboardScheme));
    register_scheme("surface", Arc::new(SurfaceScheme));
    register_scheme("block", Arc::new(BlockScheme));
}
