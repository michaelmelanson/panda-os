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
    handle: Handle,
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
            Ok(Self {
                handle: Handle::from(result as u32),
            })
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
    pub fn open_with_mailbox(path: &str, mailbox: u32, event_mask: u32) -> Result<Self> {
        let result = sys::env::open(path, mailbox, event_mask);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(Self {
                handle: Handle::from(result as u32),
            })
        }
    }

    /// Create a File from an existing handle.
    ///
    /// The File takes ownership and will close the handle on drop.
    pub fn from_handle(handle: Handle) -> Self {
        Self { handle }
    }

    /// Create a File from a typed file handle.
    ///
    /// The File takes ownership and will close the handle on drop.
    pub fn from_typed(handle: FileHandle) -> Self {
        Self {
            handle: handle.into(),
        }
    }

    /// Get the underlying handle.
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Get the underlying handle as a typed FileHandle.
    ///
    /// # Safety
    /// The caller must ensure this handle actually refers to a file.
    pub unsafe fn typed_handle(&self) -> FileHandle {
        unsafe { FileHandle::from_raw(self.handle.as_raw()) }
    }

    /// Get file metadata (size, type).
    pub fn metadata(&self) -> Result<Metadata> {
        let mut stat = FileStat {
            size: 0,
            is_dir: false,
        };
        let result = sys::file::stat(self.handle, &mut stat);
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

    /// Consume the File and return the underlying handle without closing it.
    ///
    /// The caller is responsible for closing the handle.
    pub fn into_handle(self) -> Handle {
        let handle = self.handle;
        core::mem::forget(self);
        handle
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let result = sys::file::read(self.handle, buf);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(result as usize)
        }
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let result = sys::file::write(self.handle, buf);
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
        let result = sys::file::seek(self.handle, offset, whence);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(result as u64)
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = sys::file::close(self.handle);
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
