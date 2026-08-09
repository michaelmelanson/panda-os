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
use panda_abi::path;
use spinning_top::RwSpinlock;

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

impl From<crate::resource::BlockError> for FsError {
    /// Preserve the underlying block error's meaning rather than collapsing
    /// every failure into a single variant.
    fn from(err: crate::resource::BlockError) -> Self {
        match err {
            crate::resource::BlockError::InvalidOffset => FsError::InvalidOffset,
            crate::resource::BlockError::NotWritable => FsError::NotWritable,
            crate::resource::BlockError::NotReadable => FsError::NotReadable,
            crate::resource::BlockError::IoError => FsError::IoError,
        }
    }
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
    /// Exclusive claim on the backing device, if this filesystem is backed
    /// by a claimable device (e.g. ext2 on a virtio-blk device). `None` for
    /// filesystems with no underlying device to claim (e.g. `TarFs`, which
    /// is backed by the in-memory initrd).
    ///
    /// Mounts are currently permanent (there is no unmount syscall), so this
    /// claim simply lives for the kernel's lifetime once granted; it exists
    /// to keep a raw `block:` open from being handed out over the same
    /// device while it's mounted, not to support unmounting.
    _claim: Option<crate::devices::claims::ClaimGuard>,
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
    mount_with_claim(path, fs, None);
}

/// Mount a filesystem at the given path, holding a device claim for as long
/// as the mount table entry exists (see `mount_ext2`).
fn mount_with_claim(
    path: &str,
    fs: Arc<dyn Filesystem>,
    claim: Option<crate::devices::claims::ClaimGuard>,
) {
    let mut mounts = MOUNTS.write();
    mounts.push(Mount {
        path: String::from(path),
        fs,
        _claim: claim,
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

/// Canonicalise an absolute path and resolve it to a mounted filesystem.
///
/// Returns the filesystem and the path relative to its mount point.
/// This prevents directory traversal attacks via `..` components.
fn resolve_mount(path: &str) -> Result<(Arc<dyn Filesystem>, String), FsError> {
    let canonical = canonicalize(path);
    let (index, relative) = resolve_path(&canonical).ok_or(FsError::NotFound)?;
    let mounts = MOUNTS.read();
    Ok((mounts[index].fs.clone(), relative))
}

/// Open a file at the given absolute path (async).
pub async fn open(path: &str) -> Result<Box<dyn File>, FsError> {
    let (fs, relative) = resolve_mount(path)?;
    fs.open(&relative).await
}

/// Get metadata for an absolute path (async).
pub async fn stat(path: &str) -> Result<FileStat, FsError> {
    let (fs, relative) = resolve_mount(path)?;
    fs.stat(&relative).await
}

/// List directory contents at an absolute path (async).
pub async fn readdir(path: &str) -> Result<Vec<DirEntry>, FsError> {
    let (fs, relative) = resolve_mount(path)?;
    fs.readdir(&relative).await
}

/// Create a new file at the given absolute path (async).
pub async fn create(path: &str, mode: u16) -> Result<Box<dyn File>, FsError> {
    let (fs, relative) = resolve_mount(path)?;
    fs.create(&relative, mode).await
}

/// Remove (unlink) a file at the given absolute path (async).
pub async fn unlink(path: &str) -> Result<(), FsError> {
    let (fs, relative) = resolve_mount(path)?;
    fs.unlink(&relative).await
}

/// Create a directory at the given absolute path (async).
pub async fn mkdir(path: &str, mode: u16) -> Result<(), FsError> {
    let (fs, relative) = resolve_mount(path)?;
    fs.mkdir(&relative, mode).await
}

/// Remove an empty directory at the given absolute path (async).
pub async fn rmdir(path: &str) -> Result<(), FsError> {
    let (fs, relative) = resolve_mount(path)?;
    fs.rmdir(&relative).await
}

/// Truncate (or extend) a file at the given absolute path (async).
pub async fn truncate(path: &str, size: u64) -> Result<(), FsError> {
    let (fs, relative) = resolve_mount(path)?;
    fs.truncate(&relative, size).await
}

/// Flush all pending metadata and data for the filesystem at the given path (async).
///
/// The path is used to identify which mounted filesystem to sync.
pub async fn sync(path: &str) -> Result<(), FsError> {
    let (fs, _relative) = resolve_mount(path)?;
    fs.sync().await
}

// =============================================================================
// Ext2 mount
// =============================================================================

/// Mount ext2 filesystem from the block device at `block:/pci/storage/0`
/// (the first storage-class device — see `device_path`) at the given
/// mountpoint.
///
/// This is called from the mount syscall handler, itself called by `init`
/// (`userspace/init/src/main.rs`) right after it waits for `virtio-blk`'s
/// readiness handshake (`libpanda::process::wait_ready`) — that wait is
/// what actually closes the startup race against `block:` registration
/// (task #43: "Redesign scheme-open readiness to eliminate the
/// driver-registration race class"), so `open_retrying` below should
/// normally resolve on its first attempt. It stays in place as
/// defense-in-depth for any caller of this function that isn't `init`'s
/// synchronous-wait boot path — see `resource::open_retrying`'s doc
/// comment.
pub async fn mount_ext2(mountpoint: &str) -> Result<(), &'static str> {
    use log::info;

    info!("Attempting to mount ext2 from block:/pci/storage/0");

    let device = crate::resource::open_retrying(|| {
        crate::resource::SchemeBlockDevice::open("block:/pci/storage/0")
    })
    .await
    .map_err(|_| "No block device found (block: never became available)")?;

    let device: Arc<dyn crate::resource::BlockDevice> = Arc::new(device);

    // No `devices::claims` guard needed here (contrast the deleted kernel
    // virtio-blk driver's version of this function): exclusivity is now
    // enforced by the userspace virtio-blk driver's own `Open` handling —
    // opening this device above *is* the exclusive claim, held for as long
    // as `device`'s `SchemeBlockDevice` (and thus its underlying
    // `SchemeProxyResource`) stays alive, i.e. for as long as the mount
    // table entry holds this `Ext2Fs`'s `Arc`. See
    // `resource::block_client::SchemeBlockDevice`'s module doc comment.
    let fs = Ext2Fs::mount(device).await?;
    mount_with_claim(mountpoint, fs, None);
    Ok(())
}
