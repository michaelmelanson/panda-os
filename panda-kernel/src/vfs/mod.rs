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

/// Poll a future once, expecting it to complete immediately.
///
/// This is for use with synchronous filesystems like TarFs that always
/// return immediately-ready futures. Panics if the future returns Pending.
///
/// For truly async operations (like ext2 disk I/O), use the process-level
/// async infrastructure instead.
pub fn poll_immediate<T>(mut future: Pin<&mut (impl Future<Output = T> + ?Sized)>) -> T {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);

    match future.as_mut().poll(&mut cx) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("poll_immediate called on a future that returned Pending"),
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
}

/// File metadata
#[derive(Debug, Clone)]
pub struct FileStat {
    /// Size in bytes
    pub size: u64,
    /// Whether this is a directory
    pub is_dir: bool,
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
    fs: Box<dyn Filesystem>,
}

static MOUNTS: RwSpinlock<Vec<Mount>> = RwSpinlock::new(Vec::new());

/// Mount a filesystem at the given path
pub fn mount(path: &str, fs: Box<dyn Filesystem>) {
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
pub async fn open(path: &str) -> Result<Box<dyn File>, FsError> {
    let (index, relative) = resolve_path(path).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.open(&relative).await
}

/// Get metadata for an absolute path (async).
pub async fn stat(path: &str) -> Result<FileStat, FsError> {
    let (index, relative) = resolve_path(path).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.stat(&relative).await
}

/// List directory contents at an absolute path (async).
pub async fn readdir(path: &str) -> Result<Vec<DirEntry>, FsError> {
    let (index, relative) = resolve_path(path).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    mounts[index].fs.readdir(&relative).await
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

    // Get the async block device
    let Some(device) = crate::devices::virtio_block::get_async_device(address) else {
        return Err("Failed to get block device");
    };
    let device: Arc<dyn crate::resource::BlockDevice> = Arc::new(device);

    // Mount ext2
    let fs = Ext2Fs::mount(device).await?;
    mount(mountpoint, Box::new(Ext2FsWrapper(fs)));
    Ok(())
}

/// Wrapper to convert Arc<Ext2Fs> to Box<dyn Filesystem>.
struct Ext2FsWrapper(Arc<Ext2Fs>);

#[async_trait]
impl Filesystem for Ext2FsWrapper {
    async fn open(&self, path: &str) -> Result<Box<dyn File>, FsError> {
        self.0.open(path).await
    }

    async fn stat(&self, path: &str) -> Result<FileStat, FsError> {
        self.0.stat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        self.0.readdir(path).await
    }
}
