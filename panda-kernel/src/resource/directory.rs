//! Directory interface for listing directory contents.

use alloc::string::String;

// The resource layer only ever needs a name and an is-dir flag for a
// directory entry — exactly what `vfs::DirEntry` already provides — so we
// reuse that type here rather than maintaining a duplicate struct that
// scheme.rs would otherwise have to field-map to and from.
pub use crate::vfs::DirEntry;

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

    /// Get the absolute VFS path for this directory, if any.
    ///
    /// Returns `Some(path)` if this is a VFS-backed directory that supports
    /// mutation operations (create, unlink).
    fn vfs_path(&self) -> Option<String> {
        None
    }
}
