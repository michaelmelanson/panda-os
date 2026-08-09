//! Filesystem interfaces.
//!
//! Synchronous — see the crate doc comment's "Why synchronous?" section.
//! The kernel keeps its own copy of these traits for `vfs::TarFs`/`initrd:`
//! only; nothing crosses the syscall boundary as a trait object, so the two
//! are independent by construction rather than by a shared crate.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::block::BlockError;

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

impl From<BlockError> for FsError {
    /// Preserve the underlying block error's meaning rather than collapsing
    /// every failure into a single variant.
    fn from(err: BlockError) -> Self {
        match err {
            BlockError::InvalidOffset => FsError::InvalidOffset,
            BlockError::NotWritable => FsError::NotWritable,
            BlockError::NotReadable => FsError::NotReadable,
            BlockError::IoError => FsError::IoError,
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

/// A filesystem that can be mounted.
pub trait Filesystem: Send + Sync {
    /// Open a file at the given path (relative to mount point)
    fn open(&self, path: &str) -> Result<Box<dyn File>, FsError>;

    /// Get metadata for a path
    fn stat(&self, path: &str) -> Result<FileStat, FsError>;

    /// List directory contents
    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;

    /// Create a new file at the given path with the given mode.
    ///
    /// Returns the opened file handle. The default implementation returns
    /// `ReadOnlyFs`, allowing read-only filesystems to work without changes.
    fn create(&self, _path: &str, _mode: u16) -> Result<Box<dyn File>, FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Remove (unlink) a file at the given path.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    fn unlink(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Create a directory at the given path with the given mode.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    fn mkdir(&self, _path: &str, _mode: u16) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Remove an empty directory at the given path.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    fn rmdir(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Truncate (or extend) a file to the given size.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    fn truncate(&self, _path: &str, _size: u64) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }

    /// Flush all pending metadata and data to the backing store.
    ///
    /// The default implementation returns `ReadOnlyFs`.
    fn sync(&self) -> Result<(), FsError> {
        Err(FsError::ReadOnlyFs)
    }
}

/// An open file.
pub trait File: Send + Sync {
    /// Read bytes into the buffer, returning bytes read
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;

    /// Write bytes from the buffer, returning bytes written
    fn write(&mut self, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotWritable)
    }

    /// Seek to a position in the file
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError>;

    /// Get file metadata
    fn stat(&self) -> Result<FileStat, FsError>;
}
