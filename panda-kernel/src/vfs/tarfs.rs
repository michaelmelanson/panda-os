//! TAR archive filesystem implementation.
//!
//! Provides read-only access to files stored in a TAR archive (used for initrd).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use tar_no_std::TarArchiveRef;

use super::{DirEntry, File, FileStat, Filesystem, FsError, Resource, SeekFrom};

/// A filesystem backed by a TAR archive
pub struct TarFs {
    /// Maps path to (data pointer, length)
    files: BTreeMap<String, (*const u8, usize)>,
}

// Safety: The data pointers come from UEFI allocation that persists for kernel lifetime
unsafe impl Send for TarFs {}
unsafe impl Sync for TarFs {}

impl TarFs {
    /// Create a TarFs from raw TAR archive data
    pub fn from_tar_data(data: *const [u8]) -> Self {
        let bytes = unsafe { data.as_ref().unwrap() };
        let archive = TarArchiveRef::new(bytes).expect("Failed to parse TAR archive");

        let mut files = BTreeMap::new();

        for entry in archive.entries() {
            let filename = entry.filename();
            let name = filename.as_str().expect("Invalid UTF-8 in filename");
            if name.is_empty() {
                continue;
            }

            // Store with normalized path (no leading ./)
            let normalized = name.trim_start_matches("./");
            let data_ptr = entry.data().as_ptr();
            let data_len = entry.data().len();
            files.insert(String::from(normalized), (data_ptr, data_len));
        }

        TarFs { files }
    }
}

impl Filesystem for TarFs {
    fn open(&self, path: &str) -> Option<Box<dyn Resource>> {
        let (ptr, len) = self.files.get(path)?;
        Some(Box::new(TarFile {
            data: *ptr,
            len: *len,
            pos: 0,
        }))
    }

    fn stat(&self, path: &str) -> Option<FileStat> {
        // Check if it's a file
        if let Some((_, len)) = self.files.get(path) {
            return Some(FileStat {
                size: *len as u64,
                is_dir: false,
            });
        }

        // Check if it's a directory (any file starts with this path)
        let dir_prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{}/", path)
        };

        for key in self.files.keys() {
            if path.is_empty() || key.starts_with(&dir_prefix) {
                return Some(FileStat {
                    size: 0,
                    is_dir: true,
                });
            }
        }

        None
    }

    fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{}/", path)
        };

        let mut entries = Vec::new();
        let mut seen_dirs = BTreeMap::new();

        for key in self.files.keys() {
            let relative = if prefix.is_empty() {
                key.as_str()
            } else if let Some(rel) = key.strip_prefix(&prefix) {
                rel
            } else {
                continue;
            };

            // Get the first component of the relative path
            let name = if let Some(slash_pos) = relative.find('/') {
                &relative[..slash_pos]
            } else {
                relative
            };

            if name.is_empty() {
                continue;
            }

            let is_dir = relative.contains('/');

            // Avoid duplicate directory entries
            if is_dir {
                if seen_dirs.contains_key(name) {
                    continue;
                }
                seen_dirs.insert(String::from(name), ());
            }

            entries.push(DirEntry {
                name: String::from(name),
                is_dir,
            });
        }

        if entries.is_empty() && !path.is_empty() {
            None
        } else {
            Some(entries)
        }
    }
}

/// An open file in a TAR archive
struct TarFile {
    data: *const u8,
    len: usize,
    pos: usize,
}


// Safety: Data pointer is from static UEFI allocation
unsafe impl Send for TarFile {}
unsafe impl Sync for TarFile {}

impl Resource for TarFile {
    fn as_file(&mut self) -> Option<&mut dyn File> {
        Some(self)
    }
}

impl File for TarFile {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        let remaining = self.len.saturating_sub(self.pos);
        let to_read = buf.len().min(remaining);

        if to_read > 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(self.data.add(self.pos), buf.as_mut_ptr(), to_read);
            }
            self.pos += to_read;
        }

        Ok(to_read)
    }

    fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(offset) => self.pos as i64 + offset,
            SeekFrom::End(offset) => self.len as i64 + offset,
        };

        if new_pos < 0 || new_pos as usize > self.len {
            return Err(FsError::InvalidOffset);
        }

        self.pos = new_pos as usize;
        Ok(self.pos as u64)
    }

    fn stat(&self) -> FileStat {
        FileStat {
            size: self.len as u64,
            is_dir: false,
        }
    }
}
