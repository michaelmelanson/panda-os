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

    /// Get the URI this directory listing came from, if it supports
    /// mutation operations (create, unlink, mkdir, rmdir).
    ///
    /// See `resource::scheme::DirectoryResource`'s doc comment for why this
    /// is a full URI rather than a bare filesystem path.
    fn uri(&self) -> Option<String> {
        None
    }
}
