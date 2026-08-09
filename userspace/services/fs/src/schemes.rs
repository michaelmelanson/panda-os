//! The filesystem service's clients of other schemes.
//!
//! The service is a provider of `file:` and a *client* of two other
//! schemes:
//!
//! - [`SchemeBlockDevice`] — `block:/pci/storage/0`, served by
//!   `userspace/drivers/virtio-blk`, used as the [`BlockDevice`] ext2 reads
//!   and writes through. This is the userspace equivalent of the deleted
//!   `panda-kernel/src/resource/block_client.rs`, and much simpler than it:
//!   there, "kernel-internal code holding a channel to a userspace scheme
//!   provider" needed a whole new primitive; here it is just an open handle
//!   and ordinary `OP_FILE_SEEK`/`READ`/`WRITE` syscalls.
//!
//! - [`InitrdFs`] — `initrd:`, served by the kernel itself, mounted at
//!   `/initrd` so `file:/initrd/...` keeps resolving exactly as it did when
//!   the kernel owned the mount table. Without this the initrd would only
//!   be reachable under its own scheme, which would be a visible change to
//!   every existing client of `file:/initrd/...`.
//!
//! # Exclusivity
//!
//! Holding the `block:` handle open for the mount's lifetime *is* the
//! exclusive claim on the device: the virtio-blk driver fails a second
//! concurrent `Open` with `Busy` (see its `serve_provider_requests`), so a
//! raw `block:` open from another process is rejected while ext2 is
//! mounted, which is what `claim_test` checks. Dropping the handle releases
//! it.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use libpanda::handle::Handle;
use libpanda::{environment, file};

use crate::block::{BlockDevice, BlockError};
use crate::fs::{DirEntry, File, FileStat, Filesystem, FsError, SeekFrom};

/// The device this service mounts ext2 from. `block:` is a userspace scheme
/// (`userspace/drivers/virtio-blk`) and `/pci/storage/0` is the first
/// storage-class device under the unified device path scheme — see
/// docs/DEVICE_PATHS.md.
pub const BLOCK_DEVICE_URI: &str = "block:/pci/storage/0";

/// Sector size assumed for a scheme-backed block device.
///
/// The wire protocol has no sector-size query — only `Request::Size` for the
/// total byte size — so this matches virtio-blk's near-universal 512-byte
/// sector, the same assumption the deleted kernel-side adapter made.
const DEFAULT_SECTOR_SIZE: u32 = 512;

/// Largest transfer this service will attempt in one syscall. The kernel
/// caps a scheme-proxy read or write at one protocol frame's payload
/// anyway (`MAX_TRANSFER_SIZE`), so asking for more just produces a short
/// transfer that the loops below have to handle regardless.
const MAX_IO: usize = panda_abi::scheme_protocol::MAX_TRANSFER_SIZE;

/// Seek `handle` to `offset` from the start.
fn seek_to(handle: Handle, offset: u64) -> Result<(), BlockError> {
    if file::seek(handle, offset as i64, panda_abi::SEEK_SET) < 0 {
        return Err(BlockError::InvalidOffset);
    }
    Ok(())
}

/// A [`BlockDevice`] backed by an open `block:` handle.
pub struct SchemeBlockDevice {
    handle: Handle,
    /// Cached at open time: [`BlockDevice::size`] is synchronous, and the
    /// only way to ask is a `SEEK_END`, which would disturb the file
    /// position every call.
    size: u64,
}

impl SchemeBlockDevice {
    /// Open `uri` as a block device and cache its size.
    pub fn open(uri: &str) -> libpanda::error::Result<Self> {
        let handle = environment::open(uri, 0, 0)?;
        // `SEEK_END` with offset 0 answers `Request::Size` on the provider
        // side (see the kernel's `handle_seek_scheme_proxy`), which is the
        // only size query the protocol offers for a `block:` resource.
        let size = file::seek(handle, 0, panda_abi::SEEK_END);
        if size < 0 {
            return Err(panda_abi::ErrorCode::IoError);
        }
        Ok(Self {
            handle,
            size: size as u64,
        })
    }
}

impl BlockDevice for SchemeBlockDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }
        seek_to(self.handle, offset)?;
        let mut done = 0;
        while done < buf.len() {
            let end = (done + MAX_IO).min(buf.len());
            let n = file::read(self.handle, &mut buf[done..end]);
            if n < 0 {
                return Err(BlockError::IoError);
            }
            if n == 0 {
                break; // EOF / device boundary
            }
            done += n as usize;
        }
        Ok(done)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }
        seek_to(self.handle, offset)?;
        let mut done = 0;
        while done < buf.len() {
            let end = (done + MAX_IO).min(buf.len());
            let n = file::write(self.handle, &buf[done..end]);
            if n < 0 {
                return Err(BlockError::IoError);
            }
            if n == 0 {
                break;
            }
            done += n as usize;
        }
        Ok(done)
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        DEFAULT_SECTOR_SIZE
    }

    fn sync(&self) -> Result<(), BlockError> {
        // virtio-blk is write-through and every `write` round trip already
        // only completes once the driver acknowledges it, so there is
        // nothing to flush — the deleted kernel driver's `sync` was a no-op
        // for the same reason.
        Ok(())
    }
}

/// A read-only [`Filesystem`] over the kernel's `initrd:` scheme, so the
/// in-memory boot archive stays reachable at `file:/initrd/...`.
pub struct InitrdFs;

/// Build the `initrd:` URI for a mount-relative path.
fn initrd_uri(path: &str) -> String {
    alloc::format!("initrd:/{}", path.trim_start_matches('/'))
}

impl Filesystem for InitrdFs {
    fn open(&self, path: &str) -> Result<Box<dyn File>, FsError> {
        let handle = environment::open(&initrd_uri(path), 0, 0).map_err(|_| FsError::NotFound)?;
        Ok(Box::new(HandleFile { handle, pos: 0 }))
    }

    fn stat(&self, path: &str) -> Result<FileStat, FsError> {
        let handle = environment::open(&initrd_uri(path), 0, 0).map_err(|_| FsError::NotFound)?;
        let stat = HandleFile { handle, pos: 0 }.stat();
        stat
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let handle = environment::opendir(&initrd_uri(path)).map_err(|_| FsError::NotFound)?;
        let mut entries = Vec::new();
        let mut entry = panda_abi::DirEntry {
            name_len: 0,
            is_dir: false,
            name: [0u8; panda_abi::DIRENT_NAME_MAX],
        };
        loop {
            let result = file::readdir(handle, &mut entry);
            if result < 0 {
                file::close(handle);
                return Err(FsError::IoError);
            }
            if result == 0 {
                break;
            }
            let name = core::str::from_utf8(&entry.name[..entry.name_len as usize])
                .unwrap_or("")
                .into();
            entries.push(DirEntry {
                name,
                is_dir: entry.is_dir,
            });
        }
        file::close(handle);
        Ok(entries)
    }
}

/// A [`File`] over an open kernel file handle. Used for `initrd:` files;
/// the position is tracked here and pushed to the handle before each read,
/// so it behaves the same whether or not the handle is shared.
struct HandleFile {
    handle: Handle,
    pos: u64,
}

impl File for HandleFile {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if file::seek(self.handle, self.pos as i64, panda_abi::SEEK_SET) < 0 {
            return Err(FsError::InvalidOffset);
        }
        let want = buf.len().min(MAX_IO);
        let n = file::read(self.handle, &mut buf[..want]);
        if n < 0 {
            return Err(FsError::IoError);
        }
        self.pos += n as u64;
        Ok(n as usize)
    }

    fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError> {
        let new = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(n) => self.stat()?.size as i64 + n,
        };
        if new < 0 {
            return Err(FsError::InvalidOffset);
        }
        self.pos = new as u64;
        Ok(self.pos)
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        let mut stat = panda_abi::FileStat {
            size: 0,
            is_dir: false,
        };
        if file::stat(self.handle, &mut stat) < 0 {
            return Err(FsError::IoError);
        }
        Ok(FileStat {
            size: stat.size,
            is_dir: stat.is_dir,
            // The kernel's `FileStat` ABI struct only carries size and
            // is_dir; the richer fields exist for ext2, which fills them in
            // from the inode. Nothing reads them for initrd files.
            mode: if stat.is_dir { 0o555 } else { 0o444 },
            inode: 0,
            nlinks: 1,
            mtime: 0,
            ctime: 0,
            atime: 0,
        })
    }
}

impl Drop for HandleFile {
    fn drop(&mut self) {
        file::close(self.handle);
    }
}
