//! I/O traits and types.
//!
//! This module provides Read, Write, and Seek traits similar to std::io,
//! as well as RAII file wrappers.

mod file;

pub use file::File;

use crate::error::Result;

/// The Read trait for reading bytes.
pub trait Read {
    /// Pull some bytes from this source into the specified buffer.
    ///
    /// Returns the number of bytes read.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Read the exact number of bytes required to fill `buf`.
    ///
    /// Returns an error if EOF is reached before filling the buffer.
    fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.read(buf) {
                Ok(0) => return Err(crate::error::Error::IoError),
                Ok(n) => buf = &mut buf[n..],
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Read all bytes until EOF, appending them to `buf`.
    ///
    /// Returns the number of bytes read.
    fn read_to_end(&mut self, buf: &mut alloc::vec::Vec<u8>) -> Result<usize> {
        let mut total = 0;
        let mut chunk = [0u8; 512];
        loop {
            match self.read(&mut chunk) {
                Ok(0) => return Ok(total),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    total += n;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Read all bytes until EOF into a new String.
    ///
    /// Returns an error if the content is not valid UTF-8.
    fn read_to_string(&mut self, buf: &mut alloc::string::String) -> Result<usize> {
        let mut bytes = alloc::vec::Vec::new();
        let len = self.read_to_end(&mut bytes)?;
        match alloc::string::String::from_utf8(bytes) {
            Ok(s) => {
                buf.push_str(&s);
                Ok(len)
            }
            Err(_) => Err(crate::error::Error::InvalidArgument),
        }
    }
}

/// The Write trait for writing bytes.
pub trait Write {
    /// Write a buffer into this writer.
    ///
    /// Returns the number of bytes written.
    fn write(&mut self, buf: &[u8]) -> Result<usize>;

    /// Flush this output stream, ensuring all buffered data is written.
    fn flush(&mut self) -> Result<()>;

    /// Attempt to write an entire buffer into this writer.
    fn write_all(&mut self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0) => return Err(crate::error::Error::IoError),
                Ok(n) => buf = &buf[n..],
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Enumeration of possible methods to seek within a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekFrom {
    /// Seek from the beginning of the file.
    Start(u64),
    /// Seek from the end of the file.
    End(i64),
    /// Seek from the current position.
    Current(i64),
}

/// The Seek trait for seeking within a stream.
pub trait Seek {
    /// Seek to an offset, in bytes, in a stream.
    ///
    /// Returns the new position from the start of the stream.
    fn seek(&mut self, pos: SeekFrom) -> Result<u64>;

    /// Returns the current seek position from the start of the stream.
    fn stream_position(&mut self) -> Result<u64> {
        self.seek(SeekFrom::Current(0))
    }

    /// Rewind to the beginning of the stream.
    fn rewind(&mut self) -> Result<()> {
        self.seek(SeekFrom::Start(0))?;
        Ok(())
    }
}
