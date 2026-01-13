//! CharacterOutput interface for character-based output devices.

/// Errors that can occur during character output operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharOutError {
    /// I/O error during operation.
    IoError,
    /// Device not ready.
    NotReady,
}

/// Interface for character-based output devices.
///
/// Implemented by serial console, terminal, etc.
pub trait CharacterOutput: Send + Sync {
    /// Write data to the output device.
    ///
    /// Returns the number of bytes written.
    fn write(&self, buf: &[u8]) -> Result<usize, CharOutError>;

    /// Flush any buffered output.
    fn flush(&self) -> Result<(), CharOutError> {
        Ok(())
    }
}
