//! Block interface for random-access data (files, disks, memory regions).

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

/// Interface for random-access block data.
///
/// Implemented by files, disk partitions, memory regions, etc.
/// Operations are stateless - offsets are provided explicitly.
pub trait Block: Send + Sync {
    /// Read data at the given offset.
    ///
    /// Returns the bytes read. May return fewer bytes than requested if at EOF.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError>;

    /// Write data at the given offset.
    ///
    /// Returns the number of bytes written.
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
