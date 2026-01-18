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
use spinning_top::{RwSpinlock, Spinlock};
use x86_64::instructions::port::Port;

use crate::device_address::DeviceAddress;
use crate::devices::virtio_keyboard::{self, VirtioKeyboard};
use crate::process::waker::Waker;
use crate::vfs;

use super::Resource;
use super::block::{Block, BlockError};
use super::char_output::{CharOutError, CharacterOutput};
use super::directory::{DirEntry, Directory};
use super::event_source::{Event, EventSource, KeyEvent};

/// A handler for a resource scheme (e.g., "file", "console", "pci")
pub trait SchemeHandler: Send + Sync {
    /// Open a resource at the given path within this scheme
    fn open(&self, path: &str) -> Option<Box<dyn Resource>>;

    /// List directory contents at the given path within this scheme
    fn readdir(&self, _path: &str) -> Option<Vec<DirEntry>> {
        None
    }
}

/// Global registry of scheme handlers
static SCHEMES: RwSpinlock<BTreeMap<&'static str, Box<dyn SchemeHandler>>> =
    RwSpinlock::new(BTreeMap::new());

/// Register a scheme handler
pub fn register_scheme(name: &'static str, handler: Box<dyn SchemeHandler>) {
    let mut schemes = SCHEMES.write();
    schemes.insert(name, handler);
}

/// Open a resource by URI (e.g., "file:/initrd/init" or "console:/serial/0")
pub fn open(uri: &str) -> Option<Box<dyn Resource>> {
    let (scheme, path) = uri.split_once(':')?;
    let schemes = SCHEMES.read();
    let handler = schemes.get(scheme)?;
    handler.open(path)
}

/// List directory contents by URI (e.g., "file:/initrd")
pub fn readdir(uri: &str) -> Option<Vec<DirEntry>> {
    let (scheme, path) = uri.split_once(':')?;
    let schemes = SCHEMES.read();
    let handler = schemes.get(scheme)?;
    handler.readdir(path)
}

// =============================================================================
// File Scheme - wraps existing VFS
// =============================================================================

/// Scheme handler that wraps the existing VFS mount system
pub struct FileScheme;

impl SchemeHandler for FileScheme {
    fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        // Check if it's a directory first
        if let Some(stat) = vfs::stat(path) {
            if stat.is_dir {
                // Return a directory resource
                let entries = vfs::readdir(path)?;
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
        let file = vfs::open(path)?;
        Some(Box::new(VfsFileResource::new(file)))
    }

    fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        let entries = vfs::readdir(path)?;
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
    fn as_block(&self) -> Option<&dyn Block> {
        Some(self)
    }
}

impl Block for VfsFileResource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError> {
        let mut file = self.file.lock();
        // Seek to offset
        file.seek(vfs::SeekFrom::Start(offset))
            .map_err(|_| BlockError::InvalidOffset)?;
        // Read data
        file.read(buf).map_err(|_| BlockError::IoError)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
        let mut file = self.file.lock();
        // Seek to offset
        file.seek(vfs::SeekFrom::Start(offset))
            .map_err(|_| BlockError::InvalidOffset)?;
        // Write data
        file.write(buf).map_err(|e| match e {
            vfs::FsError::NotWritable => BlockError::NotWritable,
            _ => BlockError::IoError,
        })
    }

    fn size(&self) -> u64 {
        self.file.lock().stat().size
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

impl SchemeHandler for ConsoleScheme {
    fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        match path {
            "/serial/0" => Some(Box::new(SerialConsoleResource::new(0x3f8))),
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

impl SchemeHandler for KeyboardScheme {
    fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        // Parse path like "/pci/00:03.0"
        let address = DeviceAddress::from_path(path)?;
        let keyboard = virtio_keyboard::get_keyboard(&address)?;
        Some(Box::new(KeyboardResource { keyboard }))
    }
}

/// Handle to an open keyboard device
struct KeyboardResource {
    keyboard: Arc<Spinlock<VirtioKeyboard>>,
}

impl Resource for KeyboardResource {
    fn as_event_source(&self) -> Option<&dyn EventSource> {
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

impl SchemeHandler for SurfaceScheme {
    fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
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
}

// =============================================================================
// Initialization
// =============================================================================

/// Initialize the resource scheme system with default schemes
pub fn init() {
    register_scheme("file", Box::new(FileScheme));
    register_scheme("console", Box::new(ConsoleScheme));
    register_scheme("keyboard", Box::new(KeyboardScheme));
    register_scheme("surface", Box::new(SurfaceScheme));
}
