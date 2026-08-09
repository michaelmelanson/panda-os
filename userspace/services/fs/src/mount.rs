//! The VFS mount table and path canonicalisation policy.
//!
//! Moved from `panda-kernel/src/vfs/mod.rs` (roadmap M5). Two changes from
//! the kernel version, both consequences of this now being a process rather
//! than kernel-global state:
//!
//! - the mount list is an owned [`MountTable`] rather than a global
//!   `static RwSpinlock<Vec<Mount>>` — the service owns exactly one and
//!   passes it around, which is also what makes the mount-resolution rules
//!   unit-testable on the host (see the tests at the bottom of this file,
//!   ported from the deleted `panda-kernel/tests/vfs_path.rs`);
//! - there is no device-claim guard on a mount entry. Exclusivity is
//!   enforced by the `block:` provider's own `Open` handling now (see
//!   `userspace/drivers/virtio-blk`), and this service simply holds its
//!   `block:` handle open for as long as the mount lives.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use panda_abi::path;

use crate::fs::{DirEntry, FileStat, Filesystem, FsError};
use alloc::boxed::Box;

/// A mounted filesystem.
struct Mount {
    /// Mount point path (e.g. `/initrd`).
    path: String,
    /// The filesystem implementation.
    fs: Arc<dyn Filesystem>,
}

/// Canonicalise a VFS path.
///
/// Returns the path with `.`, `..`, and repeated slashes resolved.
fn canonicalize(input: &str) -> String {
    if path::is_canonical(input) {
        return String::from(input);
    }
    let mut buf = [0u8; 4096];
    match path::canonicalize_path_to_buf(input, &mut buf) {
        Some(s) => String::from(s),
        None => String::from("/"), // Path too long or too deep; fall back to root
    }
}

/// The set of mounted filesystems, and the path-resolution policy over them.
#[derive(Default)]
pub struct MountTable {
    mounts: Vec<Mount>,
}

impl MountTable {
    /// An empty mount table.
    pub fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    /// Mount a filesystem at the given path.
    pub fn mount(&mut self, path: &str, fs: Arc<dyn Filesystem>) {
        self.mounts.push(Mount {
            path: String::from(path),
            fs,
        });
    }

    /// Whether anything is mounted at exactly `path`.
    pub fn is_mounted(&self, path: &str) -> bool {
        self.mounts.iter().any(|m| m.path == path)
    }

    /// Find the filesystem and relative path for an absolute canonical path.
    ///
    /// Returns the longest matching mount point's filesystem and the path
    /// relative to it, or `None` if no mount matches.
    fn resolve_path(&self, path: &str) -> Option<(usize, String)> {
        // Find the longest matching mount point
        let mut best_match: Option<(usize, usize)> = None;

        for (index, mount) in self.mounts.iter().enumerate() {
            if path.starts_with(&mount.path) {
                let mount_len = mount.path.len();
                // Check it's a proper prefix (path continues with / or ends exactly)
                if path.len() == mount_len || path.as_bytes().get(mount_len) == Some(&b'/') {
                    match best_match {
                        None => best_match = Some((mount_len, index)),
                        Some((best_len, _)) if mount_len > best_len => {
                            best_match = Some((mount_len, index))
                        }
                        _ => {}
                    }
                }
            }
        }

        best_match.map(|(mount_len, index)| {
            // Get the relative path (skip mount point and leading slash)
            let relative = if path.len() > mount_len {
                String::from(&path[mount_len + 1..]) // Skip the '/' after mount point
            } else {
                String::new() // Root of the mount
            };
            (index, relative)
        })
    }

    /// Canonicalise an absolute path and resolve it to a mounted filesystem.
    ///
    /// Returns the filesystem and the path relative to its mount point.
    /// This prevents directory traversal attacks via `..` components.
    pub fn resolve_mount(&self, path: &str) -> Result<(Arc<dyn Filesystem>, String), FsError> {
        let canonical = canonicalize(path);
        let (index, relative) = self.resolve_path(&canonical).ok_or(FsError::NotFound)?;
        Ok((self.mounts[index].fs.clone(), relative))
    }

    /// Open a file at the given absolute path.
    pub fn open(&self, path: &str) -> Result<Box<dyn crate::fs::File>, FsError> {
        let (fs, relative) = self.resolve_mount(path)?;
        fs.open(&relative)
    }

    /// Get metadata for an absolute path.
    pub fn stat(&self, path: &str) -> Result<FileStat, FsError> {
        let (fs, relative) = self.resolve_mount(path)?;
        fs.stat(&relative)
    }

    /// List directory contents at an absolute path.
    pub fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let (fs, relative) = self.resolve_mount(path)?;
        fs.readdir(&relative)
    }

    /// Create a new file at the given absolute path.
    pub fn create(&self, path: &str, mode: u16) -> Result<Box<dyn crate::fs::File>, FsError> {
        let (fs, relative) = self.resolve_mount(path)?;
        fs.create(&relative, mode)
    }

    /// Remove (unlink) a file at the given absolute path.
    pub fn unlink(&self, path: &str) -> Result<(), FsError> {
        let (fs, relative) = self.resolve_mount(path)?;
        fs.unlink(&relative)
    }

    /// Create a directory at the given absolute path.
    pub fn mkdir(&self, path: &str, mode: u16) -> Result<(), FsError> {
        let (fs, relative) = self.resolve_mount(path)?;
        fs.mkdir(&relative, mode)
    }

    /// Remove an empty directory at the given absolute path.
    pub fn rmdir(&self, path: &str) -> Result<(), FsError> {
        let (fs, relative) = self.resolve_mount(path)?;
        fs.rmdir(&relative)
    }

    /// Truncate (or extend) a file at the given absolute path.
    pub fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        let (fs, relative) = self.resolve_mount(path)?;
        fs.truncate(&relative, size)
    }

    /// Flush all pending metadata and data for the filesystem mounted at the
    /// given path.
    pub fn sync(&self, path: &str) -> Result<(), FsError> {
        let (fs, _relative) = self.resolve_mount(path)?;
        fs.sync()
    }
}

#[cfg(test)]
mod tests {
    //! Mount-resolution and path-canonicalisation tests, ported from the
    //! deleted `panda-kernel/tests/vfs_path.rs` (which ran in QEMU because
    //! the mount table used to be kernel-global state). The read-only
    //! in-memory [`MemFs`] below stands in for that test's `TarFs`: what is
    //! under test is the mount table's canonicalise-then-longest-prefix
    //! policy, not any particular filesystem.

    use super::*;
    use crate::fs::{File, SeekFrom};

    struct MemFile {
        data: &'static [u8],
        pos: usize,
    }

    impl File for MemFile {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
            let n = (self.data.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }

        fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError> {
            let new = match pos {
                SeekFrom::Start(n) => n as i64,
                SeekFrom::Current(n) => self.pos as i64 + n,
                SeekFrom::End(n) => self.data.len() as i64 + n,
            };
            if new < 0 {
                return Err(FsError::InvalidOffset);
            }
            self.pos = new as usize;
            Ok(self.pos as u64)
        }

        fn stat(&self) -> Result<FileStat, FsError> {
            Ok(stat_for(self.data.len() as u64, false))
        }
    }

    fn stat_for(size: u64, is_dir: bool) -> FileStat {
        FileStat {
            size,
            is_dir,
            mode: 0o644,
            inode: 1,
            nlinks: 1,
            mtime: 0,
            ctime: 0,
            atime: 0,
        }
    }

    /// A read-only filesystem over a fixed `(path, contents)` table.
    struct MemFs {
        files: &'static [(&'static str, &'static [u8])],
    }

    impl Filesystem for MemFs {
        fn open(&self, path: &str) -> Result<Box<dyn File>, FsError> {
            let (_, data) = self
                .files
                .iter()
                .find(|(name, _)| *name == path)
                .ok_or(FsError::NotFound)?;
            Ok(Box::new(MemFile { data, pos: 0 }))
        }

        fn stat(&self, path: &str) -> Result<FileStat, FsError> {
            if path.is_empty() {
                return Ok(stat_for(0, true));
            }
            let (_, data) = self
                .files
                .iter()
                .find(|(name, _)| *name == path)
                .ok_or(FsError::NotFound)?;
            Ok(stat_for(data.len() as u64, false))
        }

        fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
            // Only the root of the mount lists; anything else is a file.
            if !path.is_empty() {
                return Err(FsError::NotFound);
            }
            Ok(self
                .files
                .iter()
                .map(|(name, _)| DirEntry {
                    name: String::from(*name),
                    is_dir: false,
                })
                .collect())
        }
    }

    /// Mount layout mirroring the deleted kernel test:
    /// - `/test`      -> `hello.txt`, `sub/nested.txt`
    /// - `/test/deep` -> `secret.txt`
    fn table() -> MountTable {
        let mut table = MountTable::new();
        table.mount(
            "/test",
            Arc::new(MemFs {
                files: &[
                    ("hello.txt", b"Hello, world!"),
                    ("sub/nested.txt", b"Nested content"),
                ],
            }),
        );
        table.mount(
            "/test/deep",
            Arc::new(MemFs {
                files: &[("secret.txt", b"Secret file")],
            }),
        );
        table
    }

    #[test]
    fn canonical_path_unchanged() {
        assert!(table().open("/test/hello.txt").is_ok());
    }

    #[test]
    fn dot_components_stripped() {
        assert!(table().open("/test/./hello.txt").is_ok());
    }

    #[test]
    fn dotdot_resolves_parent() {
        // /test/sub/../hello.txt -> /test/hello.txt
        assert!(table().open("/test/sub/../hello.txt").is_ok());
    }

    #[test]
    fn dotdot_escape_changes_mount() {
        // Canonicalises to /test/hello.txt, so it must match the `/test`
        // mount rather than passing "../hello.txt" to `/test/deep`.
        assert!(table().open("/test/deep/../hello.txt").is_ok());
    }

    #[test]
    fn dotdot_past_root_clamped() {
        assert!(table().open("/../../../test/hello.txt").is_ok());
    }

    #[test]
    fn repeated_slashes_collapsed() {
        assert!(table().open("///test//hello.txt").is_ok());
    }

    #[test]
    fn longest_mount_prefix_wins() {
        assert!(table().open("/test/deep/./secret.txt").is_ok());
    }

    #[test]
    fn escape_to_unmounted_path_fails() {
        // /test/../nonexistent/file -> /nonexistent/file -> no mount.
        assert!(matches!(
            table().open("/test/../nonexistent/file"),
            Err(FsError::NotFound)
        ));
    }

    #[test]
    fn stat_canonicalizes() {
        let stat = table().stat("/test/./hello.txt").unwrap();
        assert!(!stat.is_dir);
        assert!(stat.size > 0);
    }

    #[test]
    fn readdir_canonicalizes() {
        // /test/sub/.. -> /test
        let entries = table().readdir("/test/sub/..").unwrap();
        assert!(!entries.is_empty());
    }

    #[test]
    fn write_methods_return_read_only_fs() {
        let table = table();
        assert!(matches!(
            table.create("/test/newfile.txt", 0o644),
            Err(FsError::ReadOnlyFs)
        ));
        assert_eq!(table.unlink("/test/hello.txt").unwrap_err(), FsError::ReadOnlyFs);
        assert_eq!(table.mkdir("/test/newdir", 0o755).unwrap_err(), FsError::ReadOnlyFs);
        assert_eq!(table.rmdir("/test/sub").unwrap_err(), FsError::ReadOnlyFs);
        assert_eq!(
            table.truncate("/test/hello.txt", 0).unwrap_err(),
            FsError::ReadOnlyFs
        );
        assert_eq!(table.sync("/test").unwrap_err(), FsError::ReadOnlyFs);
    }
}
