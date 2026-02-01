//! Virtual File System (VFS) abstraction.
//!
//! Provides a unified interface for mounting and accessing different backing stores.
//!
//! All VFS operations are async. Synchronous filesystems (like TarFs) simply
//! return immediately-ready futures.

pub mod ext2;
mod tarfs;

pub use ext2::Ext2Fs;
pub use tarfs::TarFs;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker as TaskWaker};
use panda_abi::path;
use spinning_top::RwSpinlock;

// =============================================================================
// Synchronous wrapper for immediate-completion futures
// =============================================================================

/// A no-op waker that does nothing when woken.
/// Used for polling futures that are expected to complete immediately.
fn noop_waker() -> TaskWaker {
    fn noop_clone(_: *const ()) -> RawWaker {
        RawWaker::new(core::ptr::null(), &NOOP_VTABLE)
    }
    fn noop(_: *const ()) {}

    static NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(noop_clone, noop, noop, noop);

    unsafe { TaskWaker::from_raw(RawWaker::new(core::ptr::null(), &NOOP_VTABLE)) }
}

/// Poll a future once, returning `Some(result)` if it completes immediately.
///
/// This is for use with synchronous filesystems like TarFs that always
/// return immediately-ready futures. Returns `None` if the future is not
/// ready (i.e., it would need to be polled again later).
///
/// For truly async operations (like ext2 disk I/O), use the process-level
/// async infrastructure instead.
pub fn poll_immediate<T>(mut future: Pin<&mut (impl Future<Output = T> + ?Sized)>) -> Option<T> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);

    match future.as_mut().poll(&mut cx) {
        Poll::Ready(result) => Some(result),
        Poll::Pending => None,
    }
}

/// How to reposition within a file
pub enum SeekFrom {
    /// Offset from the start of the file
    Start(u64),
    /// Offset from the current position (can be negative)
    Current(i64),
    /// Offset from the end of the file (usually negative)
    End(i64),
}

/// Filesystem errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// Path not found
    NotFound,
    /// Invalid seek offset
    InvalidOffset,
    /// Resource is not readable
    NotReadable,
    /// Resource is not writable
    NotWritable,
    /// Resource is not seekable
    NotSeekable,
    /// Filesystem is full; no space left for allocation
    NoSpace,
    /// A file or directory already exists at the given path
    AlreadyExists,
    /// Directory is not empty (e.g., for rmdir)
    NotEmpty,
    /// Operation is not valid on a directory (e.g., truncate on a dir)
    IsDirectory,
    /// Expected a directory but found a file
    NotDirectory,
    /// Filesystem is mounted read-only
    ReadOnlyFs,
    /// Block device I/O failure
    IoError,
}

/// File metadata
#[derive(Debug, Clone)]
pub struct FileStat {
    /// Size in bytes
    pub size: u64,
    /// Whether this is a directory
    pub is_dir: bool,
    /// File permissions mode (e.g., 0o755)
    pub mode: u16,
    /// Inode number
    pub inode: u64,
    /// Number of hard links
    pub nlinks: u64,
    /// Last modification time (Unix timestamp)
    pub mtime: u64,
    /// Creation / status-change time (Unix timestamp)
    pub ctime: u64,
    /// Last access time (Unix timestamp)
    pub atime: u64,
}

/// Directory entry
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Entry name (not full path)
    pub name: String,
    /// Whether this entry is a directory
    pub is_dir: bool,
}

/// A filesystem that can be mounted (async interface).
///
/// All operations are async. Synchronous filesystems return immediately-ready futures.
#[async_trait]
pub trait Filesystem: Send + Sync {
    /// Open a file at the given path (relative to mount point)
    async fn open(&self, path: &str) -> Result<Box<dyn File>, FsError>;

    /// Get metadata for a path
    async fn stat(&self, path: &str) -> Result<FileStat, FsError>;

    /// List directory contents
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;

    /// Create a new file at the given path with the given mode.
    ///
    /// Returns the opened file handle. The default implementation returns
    /// `ReadOnlyFs`, allowing read-only filesystems to work without changes.
    async fn create(&self, _path: &str, _mode: u16) -> Result<Box<dyn File>, FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Remove (unlink) a file at the given path.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    async fn unlink(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Create a directory at the given path with the given mode.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    async fn mkdir(&self, _path: &str, _mode: u16) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Remove an empty directory at the given path.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    async fn rmdir(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Truncate (or extend) a file to the given size.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    async fn truncate(&self, _path: &str, _size: u64) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Flush all pending metadata and data to the backing store.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    async fn sync(&self) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }
}

/// An open file (async interface).
///
/// All I/O operations are async. In-memory files complete immediately.
#[async_trait]
pub trait File: Send + Sync {
    /// Read bytes into the buffer, returning bytes read
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;

    /// Write bytes from the buffer, returning bytes written
    async fn write(&mut self, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotWritable)
    }

    /// Seek to a position in the file
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError>;

    /// Get file metadata
    async fn stat(&self) -> Result<FileStat, FsError>;
}

/// A mounted filesystem
struct Mount {
    /// Mount point path (e.g., "/initrd")
    path: String,
    /// The filesystem implementation
    fs: Arc<dyn Filesystem>,
}

static MOUNTS: RwSpinlock<Vec<Mount>> = RwSpinlock::new(Vec::new());

/// Canonicalize a VFS path, allocating a new String only if needed.
///
/// Returns the path with `.`, `..`, and repeated slashes resolved.
/// If the path is already canonical, returns it borrowed in a `String`
/// without reprocessing.
fn canonicalize(input: &str) -> String {
    if path::is_canonical(input) {
        return String::from(input);
    }
    let mut buf = [0u8; 4096];
    match path::canonicalize_path_to_buf(input, &mut buf) {
        Some(s) => String::from(s),
        None => String::from("/"), // Path too long or too deep; fall back to root
    }
}

/// Mount a filesystem at the given path.
pub fn mount(path: &str, fs: Arc<dyn Filesystem>) {
    let mut mounts = MOUNTS.write();
    mounts.push(Mount {
        path: String::from(path),
        fs,
    });
}

/// Find the filesystem and relative path for an absolute path.
/// Returns (filesystem_index, relative_path) or None if no mount matches.
fn resolve_path(path: &str) -> Option<(usize, String)> {
    let mounts = MOUNTS.read();

    // Find the longest matching mount point
    let mut best_match: Option<(usize, usize)> = None;

    for (index, mount) in mounts.iter().enumerate() {
        if path.starts_with(&mount.path) {
            let mount_len = mount.path.len();
            // Check it's a proper prefix (path continues with / or ends exactly)
            if path.len() == mount_len || path.as_bytes().get(mount_len) == Some(&b'/') {
                match best_match {
                    None => best_match = Some((mount_len, index)),
                    Some((best_len, _)) if mount_len > best_len => {
                        best_match = Some((mount_len, index))
                    }
                    _ => {}
                }
            }
        }
    }

    best_match.map(|(mount_len, index)| {
        // Get the relative path (skip mount point and leading slash)
        let relative = if path.len() > mount_len {
            String::from(&path[mount_len + 1..]) // Skip the '/' after mount point
        } else {
            String::new() // Root of the mount
        };
        (index, relative)
    })
}

/// Open a file at the given absolute path (async).
///
/// The path is canonicalized before mount-point resolution to prevent
/// directory traversal attacks via `..` components.
pub async fn open(path: &str) -> Result<Box<dyn File>, FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.open(&relative).await
}

/// Get metadata for an absolute path (async).
///
/// The path is canonicalized before mount-point resolution to prevent
/// directory traversal attacks via `..` components.
pub async fn stat(path: &str) -> Result<FileStat, FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.stat(&relative).await
}

/// List directory contents at an absolute path (async).
///
/// The path is canonicalized before mount-point resolution to prevent
/// directory traversal attacks via `..` components.
pub async fn readdir(path: &str) -> Result<Vec<DirEntry>, FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.readdir(&relative).await
}

/// Create a new file at the given absolute path (async).
///
/// The path is canonicalized before mount-point resolution.
pub async fn create(path: &str, mode: u16) -> Result<Box<dyn File>, FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.create(&relative, mode).await
}

/// Remove (unlink) a file at the given absolute path (async).
///
/// The path is canonicalized before mount-point resolution.
pub async fn unlink(path: &str) -> Result<(), FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.unlink(&relative).await
}

/// Create a directory at the given absolute path (async).
///
/// The path is canonicalized before mount-point resolution.
pub async fn mkdir(path: &str, mode: u16) -> Result<(), FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.mkdir(&relative, mode).await
}

/// Remove an empty directory at the given absolute path (async).
///
/// The path is canonicalized before mount-point resolution.
pub async fn rmdir(path: &str) -> Result<(), FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.rmdir(&relative).await
}

/// Truncate (or extend) a file at the given absolute path (async).
///
/// The path is canonicalized before mount-point resolution.
pub async fn truncate(path: &str, size: u64) -> Result<(), FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.truncate(&relative, size).await
}

/// Flush all pending metadata and data for the filesystem at the given path (async).
///
/// The path is used to identify which mounted filesystem to sync.
/// The path is canonicalized before mount-point resolution.
pub async fn sync(path: &str) -> Result<(), FsError> {
    let canonical = canonicalize(path);
    let (index, _relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.sync().await
}

// =============================================================================
// Block Device File Wrapper
// =============================================================================

use crate::resource::BlockDevice;

/// A file wrapper around a block device.
///
/// This allows block devices to be accessed through the VFS file interface.
pub struct BlockDeviceFile {
    device: Arc<dyn BlockDevice>,
    pos: u64,
}

impl BlockDeviceFile {
    /// Create a new block device file.
    pub fn new(device: Arc<dyn BlockDevice>) -> Self {
        Self { device, pos: 0 }
    }
}

#[async_trait]
impl File for BlockDeviceFile {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        let n = self
            .device
            .read_at(self.pos, buf)
            .await
            .map_err(|_| FsError::NotReadable)?;
        self.pos += n as u64;
        Ok(n)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<usize, FsError> {
        let n = self
            .device
            .write_at(self.pos, buf)
            .await
            .map_err(|_| FsError::NotWritable)?;
        self.pos += n as u64;
        Ok(n)
    }

    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError> {
        let size = self.device.size();
        let new_pos = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(n) => size as i64 + n,
        };
        if new_pos < 0 {
            return Err(FsError::InvalidOffset);
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }

    async fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            size: self.device.size(),
            is_dir: false,
            mode: 0o660,
            inode: 0,
            nlinks: 1,
            mtime: 0,
            ctime: 0,
            atime: 0,
        })
    }
}

// =============================================================================
// Ext2 mount
// =============================================================================

/// Mount ext2 filesystem from the first block device at the given mountpoint.
///
/// This is called from the mount syscall handler.
pub async fn mount_ext2(mountpoint: &str) -> Result<(), &'static str> {
    use log::info;

    // Get the list of block devices
    let devices = crate::devices::virtio_block::list_devices();

    if devices.is_empty() {
        return Err("No block devices found");
    }

    // Use the first block device
    let address = &devices[0];
    info!("Attempting to mount ext2 from block device {:?}", address);

    // Get the block device
    let Some(device) = crate::devices::virtio_block::get_device(address) else {
        return Err("Failed to get block device");
    };
    let device: Arc<dyn crate::resource::BlockDevice> = Arc::new(device);

    // Mount ext2 - Ext2Fs::mount returns Arc<Ext2Fs> which implements Filesystem
    let fs = Ext2Fs::mount(device).await?;
    mount(mountpoint, fs);
    Ok(())
}
