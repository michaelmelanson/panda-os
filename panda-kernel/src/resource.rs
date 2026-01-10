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
use crate::devices::virtio_keyboard::{self, InputEvent, VirtioKeyboard};
use crate::vfs::{self, DirEntry, File, FileStat, FsError, SeekFrom};

/// A handler for a resource scheme (e.g., "file", "console", "pci")
pub trait SchemeHandler: Send + Sync {
    /// Open a resource at the given path within this scheme
    fn open(&self, path: &str) -> Option<Box<dyn File>>;

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
pub fn open(uri: &str) -> Option<Box<dyn File>> {
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
    fn open(&self, path: &str) -> Option<Box<dyn File>> {
        vfs::open(path)
    }

    fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        vfs::readdir(path)
    }
}

// =============================================================================
// Console Scheme - serial console access
// =============================================================================

/// Scheme handler for console devices
pub struct ConsoleScheme;

impl SchemeHandler for ConsoleScheme {
    fn open(&self, path: &str) -> Option<Box<dyn File>> {
        match path {
            "/serial/0" => Some(Box::new(SerialConsole::new(0x3f8))),
            _ => None,
        }
    }
}

/// A serial console resource
pub struct SerialConsole {
    port: u16,
}

impl SerialConsole {
    pub fn new(port: u16) -> Self {
        Self { port }
    }
}

impl File for SerialConsole {
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize, FsError> {
        // TODO: Implement keyboard input with blocking
        Ok(0)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, FsError> {
        for &byte in buf {
            unsafe {
                Port::new(self.port).write(byte);
            }
        }
        Ok(buf.len())
    }

    fn seek(&mut self, _pos: SeekFrom) -> Result<u64, FsError> {
        Err(FsError::NotSeekable)
    }

    fn stat(&self) -> FileStat {
        FileStat {
            size: 0,
            is_dir: false,
        }
    }
}

// =============================================================================
// Keyboard Scheme - virtio keyboard access
// =============================================================================

/// Scheme handler for keyboard devices
pub struct KeyboardScheme;

impl SchemeHandler for KeyboardScheme {
    fn open(&self, path: &str) -> Option<Box<dyn File>> {
        // Parse path like "/pci/00:03.0"
        let address = DeviceAddress::from_path(path)?;
        let keyboard = virtio_keyboard::get_keyboard(&address)?;
        Some(Box::new(KeyboardHandle { keyboard }))
    }
}

/// Handle to an open keyboard device
struct KeyboardHandle {
    keyboard: Arc<Spinlock<VirtioKeyboard>>,
}

impl File for KeyboardHandle {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        let mut kb = self.keyboard.lock();

        // Try to get an event
        if let Some(event) = kb.pop_event() {
            // Copy event to buffer
            let event_size = core::mem::size_of::<InputEvent>();
            if buf.len() >= event_size {
                let event_bytes = unsafe {
                    core::slice::from_raw_parts(
                        &event as *const InputEvent as *const u8,
                        event_size,
                    )
                };
                buf[..event_size].copy_from_slice(event_bytes);
                Ok(event_size)
            } else {
                // Buffer too small
                Ok(0)
            }
        } else {
            // No events - return WouldBlock with the waker
            Err(FsError::WouldBlock(kb.waker()))
        }
    }

    fn write(&mut self, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotWritable)
    }

    fn seek(&mut self, _pos: SeekFrom) -> Result<u64, FsError> {
        Err(FsError::NotSeekable)
    }

    fn stat(&self) -> FileStat {
        FileStat {
            size: 0,
            is_dir: false,
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
}
