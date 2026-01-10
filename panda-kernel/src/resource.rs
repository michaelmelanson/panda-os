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
use spinning_top::RwSpinlock;
use x86_64::instructions::port::Port;

use crate::vfs::{self, File, FileStat, FsError, Resource, SeekFrom};

/// A handler for a resource scheme (e.g., "file", "console", "pci")
pub trait SchemeHandler: Send + Sync {
    /// Open a resource at the given path within this scheme
    fn open(&self, path: &str) -> Option<Box<dyn Resource>>;
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

// =============================================================================
// File Scheme - wraps existing VFS
// =============================================================================

/// Scheme handler that wraps the existing VFS mount system
pub struct FileScheme;

impl SchemeHandler for FileScheme {
    fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        vfs::open(path)
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

impl Resource for SerialConsole {
    fn as_file(&mut self) -> Option<&mut dyn File> {
        Some(self)
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
// Initialization
// =============================================================================

/// Initialize the resource scheme system with default schemes
pub fn init() {
    register_scheme("file", Box::new(FileScheme));
    register_scheme("console", Box::new(ConsoleScheme));
}
