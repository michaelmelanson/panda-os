//! Block device interface for random-access data.
//!
//! Synchronous — see the crate doc comment's "Why synchronous?" section.
//! The kernel has no `BlockDevice` trait at all: `block:` is a userspace
//! scheme (`userspace/drivers/virtio-blk`) with a userspace client
//! ([`crate::schemes::SchemeBlockDevice`]).

/// Errors that can occur during block operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// Invalid offset (e.g., seek before start or past end).
    InvalidOffset,
    /// Write operation not supported (read-only resource).
    NotWritable,
    /// Read operation not supported.
    NotReadable,
    /// I/O error during operation.
    IoError,
}

/// Block device interface for byte-level access.
///
/// Implementations handle sector alignment internally.
pub trait BlockDevice: Send + Sync {
    /// Read bytes at the given byte offset.
    ///
    /// Returns the number of bytes read. May return fewer bytes than requested
    /// at EOF or device boundary.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError>;

    /// Write bytes at the given byte offset.
    ///
    /// Returns the number of bytes written.
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
        let _ = (offset, buf);
        Err(BlockError::NotWritable)
    }

    /// Device size in bytes.
    fn size(&self) -> u64;

    /// Sector size in bytes (for alignment optimization).
    fn sector_size(&self) -> u32 {
        512
    }

    /// Flush any cached writes to storage.
    fn sync(&self) -> Result<(), BlockError> {
        Ok(())
    }
}
