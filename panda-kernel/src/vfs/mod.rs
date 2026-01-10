//! Virtual File System (VFS) abstraction.
//!
//! Provides a unified interface for mounting and accessing different backing stores.

mod tarfs;

pub use tarfs::TarFs;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spinning_top::RwSpinlock;

use crate::waker::Waker;

/// A kernel resource that can be accessed via a handle
pub trait Resource: Send + Sync {
    /// Try to get this resource as a file
    fn as_file(&mut self) -> Option<&mut dyn File> {
        None
    }
    // Future: as_socket(), as_pipe(), etc.
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
    /// Operation would block - caller should block on the waker
    WouldBlock(Arc<Waker>),
}

impl core::fmt::Debug for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FsError::NotFound => write!(f, "NotFound"),
            FsError::InvalidOffset => write!(f, "InvalidOffset"),
            FsError::NotReadable => write!(f, "NotReadable"),
            FsError::NotWritable => write!(f, "NotWritable"),
            FsError::NotSeekable => write!(f, "NotSeekable"),
            FsError::WouldBlock(_) => write!(f, "WouldBlock"),
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
}

/// Directory entry
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Entry name (not full path)
    pub name: String,
    /// Whether this entry is a directory
    pub is_dir: bool,
}

/// A filesystem that can be mounted
pub trait Filesystem: Send + Sync {
    /// Open a file at the given path (relative to mount point)
    fn open(&self, path: &str) -> Option<Box<dyn Resource>>;

    /// Get metadata for a path
    fn stat(&self, path: &str) -> Option<FileStat>;

    /// List directory contents
    fn readdir(&self, path: &str) -> Option<Vec<DirEntry>>;
}

/// An open file
pub trait File: Resource {
    /// Read bytes into the buffer, returning bytes read
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;

    /// Write bytes from the buffer, returning bytes written
    fn write(&mut self, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotWritable)
    }

    /// Seek to a position in the file
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError>;

    /// Get file metadata
    fn stat(&self) -> FileStat;
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

/// Find the mount and relative path for an absolute path, executing a closure with the result.
fn with_resolved_path<T, F>(path: &str, f: F) -> Option<T>
where
    F: FnOnce(&dyn Filesystem, &str) -> Option<T>,
{
    let mounts = MOUNTS.read();

    // Find the longest matching mount point
    let mut best_match: Option<(usize, &Mount)> = None;

    for mount in mounts.iter() {
        if path.starts_with(&mount.path) {
            let mount_len = mount.path.len();
            // Check it's a proper prefix (path continues with / or ends exactly)
            if path.len() == mount_len || path.as_bytes().get(mount_len) == Some(&b'/') {
                match best_match {
                    None => best_match = Some((mount_len, mount)),
                    Some((best_len, _)) if mount_len > best_len => {
                        best_match = Some((mount_len, mount))
                    }
                    _ => {}
                }
            }
        }
    }

    best_match.and_then(|(mount_len, mount)| {
        // Get the relative path (skip mount point and leading slash)
        let relative = if path.len() > mount_len {
            &path[mount_len + 1..] // Skip the '/' after mount point
        } else {
            "" // Root of the mount
        };

        f(mount.fs.as_ref(), relative)
    })
}

/// Open a file at the given absolute path
pub fn open(path: &str) -> Option<Box<dyn Resource>> {
    with_resolved_path(path, |fs, relative| fs.open(relative))
}

/// Get metadata for an absolute path
pub fn stat(path: &str) -> Option<FileStat> {
    with_resolved_path(path, |fs, relative| fs.stat(relative))
}

/// List directory contents at an absolute path
pub fn readdir(path: &str) -> Option<Vec<DirEntry>> {
    with_resolved_path(path, |fs, relative| fs.readdir(relative))
}
