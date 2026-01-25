//! Block device interface for random-access data.
//!
//! All block devices are async. Synchronous devices (like in-memory buffers)
//! simply return immediately-ready futures.

use alloc::boxed::Box;
use async_trait::async_trait;

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

/// Async block device interface for byte-level access.
///
/// This is the primary interface for block devices. All operations are async.
/// Synchronous devices return immediately-ready futures.
///
/// Implementations handle sector alignment internally.
#[async_trait]
pub trait BlockDevice: Send + Sync {
    /// Read bytes at the given byte offset.
    ///
    /// Returns the number of bytes read. May return fewer bytes than requested
    /// at EOF or device boundary.
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError>;

    /// Write bytes at the given byte offset.
    ///
    /// Returns the number of bytes written.
    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
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
    async fn sync(&self) -> Result<(), BlockError> {
        Ok(())
    }
}

/// Synchronous block interface for random-access data.
///
/// Used by resources that need synchronous access (like VfsFileResource).
/// For truly async operations, use [`BlockDevice`] directly.
pub trait Block: Send + Sync {
    /// Read data at the given offset.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError>;

    /// Write data at the given offset.
    fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize, BlockError> {
        Err(BlockError::NotWritable)
    }

    /// Get the size of this block in bytes.
    fn size(&self) -> u64;

    /// Sync any buffered writes to backing storage.
    fn sync(&self) -> Result<(), BlockError> {
        Ok(())
    }
}
