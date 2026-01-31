//! File type with RAII semantics.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::handle::{FileHandle, Handle};
use crate::io::{Read, Seek, SeekFrom, Write};
use crate::sys;
use panda_abi::FileStat;

/// A file handle with RAII semantics.
///
/// The file is automatically closed when dropped.
///
/// # Example
/// ```no_run
/// use libpanda::io::{File, Read};
/// use libpanda::String;
///
/// let mut file = File::open("file:/initrd/hello.txt").unwrap();
/// let mut contents = String::new();
/// file.read_to_string(&mut contents).unwrap();
/// // File is automatically closed here
/// ```
pub struct File {
    handle: FileHandle,
}

impl File {
    /// Open a file by path.
    ///
    /// # Example
    /// ```no_run
    /// use libpanda::io::File;
    ///
    /// let file = File::open("file:/initrd/hello.txt").unwrap();
    /// ```
    pub fn open(path: &str) -> Result<Self> {
        let result = sys::env::open(path, 0, 0);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            let handle = FileHandle::from_raw(result as u64).ok_or(Error::InvalidArgument)?;
            Ok(Self { handle })
        }
    }

    /// Open a file with mailbox attachment for event notifications.
    ///
    /// # Example
    /// ```no_run
    /// use libpanda::io::File;
    /// use libpanda::mailbox::Mailbox;
    ///
    /// let mailbox = Mailbox::default();
    /// let file = File::open_with_mailbox(
    ///     "keyboard:/pci/input/0",
    ///     mailbox.handle().as_raw(),
    ///     panda_abi::EVENT_KEYBOARD_KEY,
    /// ).unwrap();
    /// ```
    pub fn open_with_mailbox(path: &str, mailbox: u64, event_mask: u32) -> Result<Self> {
        let result = sys::env::open(path, mailbox, event_mask);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            let handle = FileHandle::from_raw(result as u64).ok_or(Error::InvalidArgument)?;
            Ok(Self { handle })
        }
    }

    /// Create a File from an existing untyped handle.
    ///
    /// The File takes ownership and will close the handle on drop.
    /// Returns `None` if the handle is not a file handle.
    pub fn from_handle(handle: Handle) -> Option<Self> {
        let handle = FileHandle::from_raw(handle.as_raw())?;
        Some(Self { handle })
    }

    /// Create a File from a typed file handle.
    ///
    /// The File takes ownership and will close the handle on drop.
    pub fn from_typed(handle: FileHandle) -> Self {
        Self { handle }
    }

    /// Get the underlying typed handle.
    pub fn handle(&self) -> FileHandle {
        self.handle
    }

    /// Get the underlying handle as an untyped Handle.
    pub fn untyped_handle(&self) -> Handle {
        self.handle.into()
    }

    /// Get file metadata (size, type).
    pub fn metadata(&self) -> Result<Metadata> {
        let mut stat = FileStat {
            size: 0,
            is_dir: false,
        };
        let result = sys::file::stat(self.handle.into(), &mut stat);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(Metadata {
                size: stat.size,
                is_dir: stat.is_dir,
            })
        }
    }

    /// Read entire file contents into a Vec.
    ///
    /// Convenience method that opens, reads, and closes in one call.
    pub fn read_all(path: &str) -> Result<Vec<u8>> {
        let mut file = Self::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// Read entire file contents into a String.
    ///
    /// Convenience method that opens, reads, and closes in one call.
    /// Returns an error if the file is not valid UTF-8.
    pub fn read_to_string_path(path: &str) -> Result<String> {
        let mut file = Self::open(path)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        Ok(buf)
    }

    /// Consume the File and return the underlying typed handle without closing it.
    ///
    /// The caller is responsible for closing the handle.
    pub fn into_handle(self) -> FileHandle {
        let handle = self.handle;
        core::mem::forget(self);
        handle
    }

    /// Consume the File and return the underlying untyped handle without closing it.
    ///
    /// The caller is responsible for closing the handle.
    pub fn into_untyped_handle(self) -> Handle {
        self.into_handle().into()
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let result = sys::file::read(self.handle.into(), buf);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(result as usize)
        }
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let result = sys::file::write(self.handle.into(), buf);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(result as usize)
        }
    }

    fn flush(&mut self) -> Result<()> {
        // Files don't have explicit flush in our syscall interface
        Ok(())
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let (offset, whence) = match pos {
            SeekFrom::Start(n) => (n as i64, panda_abi::SEEK_SET),
            SeekFrom::End(n) => (n, panda_abi::SEEK_END),
            SeekFrom::Current(n) => (n, panda_abi::SEEK_CUR),
        };
        let result = sys::file::seek(self.handle.into(), offset, whence);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(result as u64)
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = sys::file::close(self.handle.into());
    }
}

/// Metadata about a file.
#[derive(Debug, Clone, Copy)]
pub struct Metadata {
    /// Size of the file in bytes.
    pub size: u64,
    /// Whether this is a directory.
    pub is_dir: bool,
}

impl Metadata {
    /// Returns true if this metadata is for a file.
    pub fn is_file(&self) -> bool {
        !self.is_dir
    }

    /// Returns true if this metadata is for a directory.
    pub fn is_directory(&self) -> bool {
        self.is_dir
    }

    /// Returns the size of the file in bytes.
    pub fn len(&self) -> u64 {
        self.size
    }

    /// Returns true if the file is empty.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
}
