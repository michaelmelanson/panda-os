//! Filesystem interfaces for the kernel's one remaining filesystem: the
//! read-only in-memory initrd behind the `initrd:` scheme.
//!
//! Everything else lives in `userspace/services/fs`: the mount table, the
//! `file:` scheme, ext2, and the `BlockDevice` interface they read through.
//! What is left here is the
//! minimum needed to bootstrap — [`TarFs`] parses the ustar archive UEFI
//! handed the kernel at boot, and `resource::initrd::InitrdScheme` serves it
//! directly, with no mount table involved (see that module's doc comment for
//! why `initrd:` has to stay in the kernel: the filesystem service is
//! itself a binary that has to be readable before any filesystem is
//! mounted).
//!
//! These interfaces are still async because the kernel's task executor is
//! what drives them; the userspace service's equivalents in
//! `userspace/services/fs/src/fs.rs` are synchronous, and nothing crosses
//! between the two as a trait object — the two sides talk over
//! `panda_abi::scheme_protocol` frames.

mod tarfs;

pub use tarfs::TarFs;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use async_trait::async_trait;

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

/// A read-only filesystem the kernel can serve directly.
///
/// Only [`TarFs`] implements this now. The write half of the old trait
/// (`create`/`unlink`/`mkdir`/`rmdir`/`truncate`/`sync`) went to
/// `userspace/services/fs` with the filesystems that could actually
/// implement it — `TarFs` returned `ReadOnlyFs` from every one of them.
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
#[async_trait]
pub trait File: Send + Sync {
    /// Read bytes into the buffer, returning bytes read
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;

    /// Seek to a position in the file
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError>;

    /// Get file metadata
    async fn stat(&self) -> Result<FileStat, FsError>;
}
