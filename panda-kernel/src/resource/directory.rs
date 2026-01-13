//! Directory interface for listing directory contents.

use alloc::string::String;

/// A directory entry.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Entry name (not full path).
    pub name: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
}

/// Interface for directory listing.
///
/// Directories support indexed access to their entries.
pub trait Directory: Send + Sync {
    /// Get the entry at the given index.
    ///
    /// Returns `None` if index is past the end.
    fn entry(&self, index: usize) -> Option<DirEntry>;

    /// Get the number of entries in this directory.
    fn count(&self) -> usize;
}
