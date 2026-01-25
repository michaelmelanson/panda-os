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

use crate::device_address::DeviceAddress;
use crate::devices::virtio_block::{self, VirtioBlockDevice};
use crate::devices::virtio_keyboard::{self, VirtioKeyboard};
use crate::process::waker::Waker;
use crate::vfs;

use super::Resource;
use super::block::{Block, BlockError};
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
}

// =============================================================================
// Block Scheme - block device access
// =============================================================================

/// Scheme handler for block devices (virtio-blk, future AHCI, NVMe).
///
/// Paths are device addresses: `/pci/00:04.0`
pub struct BlockScheme;

#[async_trait]
impl SchemeHandler for BlockScheme {
    async fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        // Parse path like "/pci/00:04.0" directly to DeviceAddress
        let address = DeviceAddress::from_path(path)?;

        // Try virtio-blk registry (future: try AHCI, NVMe registries too)
        let device = virtio_block::get_device(&address)?;
        Some(Box::new(BlockDeviceResource { device }))
    }

    // TODO: Implement readdir for block device discovery (see TODO.md)
}

/// Resource wrapper for a block device.
struct BlockDeviceResource {
    device: Arc<Spinlock<VirtioBlockDevice>>,
}

impl Resource for BlockDeviceResource {
    fn as_block(&self) -> Option<&dyn Block> {
        Some(self)
    }
}

impl Block for BlockDeviceResource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut device = self.device.lock();
        let sector_size = device.sector_size() as u64;
        let total_size = device.capacity_sectors() * sector_size;

        if offset >= total_size {
            return Ok(0);
        }

        let available = total_size - offset;
        let to_read = (buf.len() as u64).min(available) as usize;

        let start_sector = offset / sector_size;
        let offset_in_sector = (offset % sector_size) as usize;
        let end_offset = offset + to_read as u64;
        let end_sector = (end_offset + sector_size - 1) / sector_size;
        let num_sectors = end_sector - start_sector;

        // Disable interrupts during sync I/O to avoid deadlock
        device.disable_interrupts();

        // Allocate sector-aligned buffer
        let mut sector_buf = alloc::vec![0u8; (num_sectors * sector_size) as usize];

        // Read sector by sector using sync busy-wait
        for i in 0..num_sectors {
            let sector = start_sector + i;
            let buf_offset = (i * sector_size) as usize;
            if device
                .read_block_sync(
                    sector,
                    &mut sector_buf[buf_offset..buf_offset + sector_size as usize],
                )
                .is_err()
            {
                device.enable_interrupts();
                return Err(BlockError::IoError);
            }
        }

        device.enable_interrupts();

        // Copy the requested portion
        buf[..to_read].copy_from_slice(&sector_buf[offset_in_sector..offset_in_sector + to_read]);
        Ok(to_read)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut device = self.device.lock();
        let sector_size = device.sector_size() as u64;
        let total_size = device.capacity_sectors() * sector_size;

        if offset >= total_size {
            return Err(BlockError::InvalidOffset);
        }

        let available = total_size - offset;
        let to_write = (buf.len() as u64).min(available) as usize;

        let start_sector = offset / sector_size;
        let offset_in_sector = (offset % sector_size) as usize;
        let end_offset = offset + to_write as u64;
        let end_sector = (end_offset + sector_size - 1) / sector_size;
        let num_sectors = end_sector - start_sector;

        // Disable interrupts during sync I/O to avoid deadlock
        device.disable_interrupts();

        // For unaligned writes, we need read-modify-write
        let mut sector_buf = alloc::vec![0u8; (num_sectors * sector_size) as usize];

        // If unaligned, read existing data first
        if offset_in_sector != 0 || to_write % sector_size as usize != 0 {
            for i in 0..num_sectors {
                let sector = start_sector + i;
                let buf_offset = (i * sector_size) as usize;
                if device
                    .read_block_sync(
                        sector,
                        &mut sector_buf[buf_offset..buf_offset + sector_size as usize],
                    )
                    .is_err()
                {
                    device.enable_interrupts();
                    return Err(BlockError::IoError);
                }
            }
        }

        // Copy new data into sector buffer
        sector_buf[offset_in_sector..offset_in_sector + to_write].copy_from_slice(&buf[..to_write]);

        // Write sectors
        for i in 0..num_sectors {
            let sector = start_sector + i;
            let buf_offset = (i * sector_size) as usize;
            if device
                .write_block_sync(
                    sector,
                    &sector_buf[buf_offset..buf_offset + sector_size as usize],
                )
                .is_err()
            {
                device.enable_interrupts();
                return Err(BlockError::IoError);
            }
        }

        device.enable_interrupts();
        Ok(to_write)
    }

    fn size(&self) -> u64 {
        let device = self.device.lock();
        device.capacity_sectors() * device.sector_size() as u64
    }

    fn sync(&self) -> Result<(), BlockError> {
        // virtio-blk is write-through
        Ok(())
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
