//! Ext2 filesystem driver.
//!
//! This module implements an ext2 filesystem driver with async I/O, supporting
//! both read and write operations. Mutable filesystem state (superblock and
//! block group descriptors) is protected by a single `RwSpinlock` to allow
//! concurrent reads while serialising allocation and metadata updates.

pub mod bitmap;
mod dir;
mod file;
mod guards;
mod structs;

pub use guards::{BlockGuard, InodeGuard};

pub use file::Ext2File;
pub use structs::*;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use async_trait::async_trait;
use spinning_top::RwSpinlock;

use crate::executor::async_mutex::AsyncMutex;
use crate::resource::BlockDevice;
use crate::vfs::{DirEntry, File, FileStat, Filesystem, FsError};

// =============================================================================
// Block indirection helpers (shared between Ext2Fs and Ext2File)
// =============================================================================

/// Get block number for a file block index, handling indirect blocks.
///
/// Block pointers in an inode:
/// - 0-11: direct blocks
/// - 12: single indirect (points to block of pointers)
/// - 13: double indirect (points to block of single indirect blocks)
/// - 14: triple indirect (points to block of double indirect blocks)
pub async fn get_block_number(
    device: &dyn BlockDevice,
    block_pointers: &[u32; 15],
    block_size: u32,
    file_block: u32,
) -> Result<u32, FsError> {
    let ptrs_per_block = block_size / 4;

    // Direct blocks (0-11)
    if file_block < 12 {
        return Ok(block_pointers[file_block as usize]);
    }

    // Indirect block (12)
    let fb = file_block - 12;
    if fb < ptrs_per_block {
        return read_block_ptr(device, block_pointers[12], fb, block_size).await;
    }

    // Double indirect block (13)
    let fb = fb - ptrs_per_block;
    if fb < ptrs_per_block * ptrs_per_block {
        let ind =
            read_block_ptr(device, block_pointers[13], fb / ptrs_per_block, block_size).await?;
        return read_block_ptr(device, ind, fb % ptrs_per_block, block_size).await;
    }

    // Triple indirect block (14)
    let fb = fb - ptrs_per_block * ptrs_per_block;
    let pp = ptrs_per_block * ptrs_per_block;
    let dbl = read_block_ptr(device, block_pointers[14], fb / pp, block_size).await?;
    let ind = read_block_ptr(device, dbl, (fb % pp) / ptrs_per_block, block_size).await?;
    read_block_ptr(device, ind, fb % ptrs_per_block, block_size).await
}

/// Read a single block pointer from an indirect block.
async fn read_block_ptr(
    device: &dyn BlockDevice,
    block: u32,
    index: u32,
    block_size: u32,
) -> Result<u32, FsError> {
    if block == 0 {
        return Ok(0);
    }
    let offset = block as u64 * block_size as u64 + index as u64 * 4;
    let mut buf = [0u8; 4];
    device
        .read_at(offset, &mut buf)
        .await
        .map_err(|_| FsError::NotReadable)?;
    Ok(u32::from_le_bytes(buf))
}

/// Write a single block pointer into an indirect block.
///
/// If `block` is 0, this is a bug — the caller must allocate the indirect block
/// before calling this function.
async fn write_block_ptr(
    device: &dyn BlockDevice,
    block: u32,
    index: u32,
    block_size: u32,
    value: u32,
) -> Result<(), FsError> {
    if block == 0 {
        return Err(FsError::IoError);
    }
    let offset = block as u64 * block_size as u64 + index as u64 * 4;
    let buf = value.to_le_bytes();
    device
        .write_at(offset, &buf)
        .await
        .map_err(|_| FsError::IoError)?;
    Ok(())
}

// =============================================================================
// Mutable filesystem state
// =============================================================================

/// Mutable ext2 state that requires locking for concurrent access.
///
/// This holds the superblock (for free block/inode counts) and block group
/// descriptors (for per-group free counts and bitmap locations). A single
/// `RwSpinlock` protects all mutable state — this is the simplest correct
/// approach and can be made more granular later if SMP contention becomes
/// an issue.
pub struct Ext2FsMutable {
    /// The superblock, kept in memory for free-count tracking.
    pub superblock: Superblock,
    /// Block group descriptors, updated during allocation/deallocation.
    pub block_groups: Vec<BlockGroupDescriptor>,
}

/// An ext2 filesystem instance.
pub struct Ext2Fs {
    /// The underlying block device.
    device: Arc<dyn BlockDevice>,
    /// Block size in bytes (immutable after mount).
    block_size: u32,
    /// Inode size in bytes (immutable after mount).
    inode_size: u32,
    /// Total number of blocks in the filesystem (immutable after mount).
    blocks_count: u32,
    /// Inodes per block group (immutable after mount).
    inodes_per_group: u32,
    /// Mutable filesystem state protected by a read-write spinlock.
    mutable: RwSpinlock<Ext2FsMutable>,
    /// Async mutex serialising bitmap allocation/deallocation operations.
    ///
    /// This prevents TOCTOU races in the bitmap read-modify-write cycle
    /// that spans multiple `.await` points. The `()` payload is unused;
    /// the mutex exists solely for its exclusion property.
    alloc_lock: AsyncMutex<()>,
    /// Weak self-reference used to hand `Arc<Ext2Fs>` to open files so they
    /// can perform writes (block allocation, inode writeback, etc.).
    self_ref: RwSpinlock<Weak<Ext2Fs>>,
    /// Whether the filesystem is mounted read-write.
    ///
    /// Set to `false` if unsupported RO_COMPAT features are present,
    /// preventing write operations that could corrupt the filesystem.
    read_write: bool,
}

impl Ext2Fs {
    /// Mount an ext2 filesystem from a block device.
    pub async fn mount(device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, &'static str> {
        // Read superblock (buffer is exactly 1024 bytes, matching Superblock struct size)
        let mut sb_buf = [0u8; 1024];
        device
            .read_at(SUPERBLOCK_OFFSET, &mut sb_buf)
            .await
            .map_err(|_| "failed to read superblock")?;

        // Safety: sb_buf is 1024 bytes and Superblock is repr(C) with size 1024.
        // We validate all fields before use below.
        let sb: Superblock = unsafe { core::ptr::read(sb_buf.as_ptr() as *const _) };

        if sb.magic != EXT2_SUPER_MAGIC {
            return Err("invalid ext2 magic number");
        }

        // Validate superblock fields to prevent overflow, division by zero,
        // and excessive allocations from malicious disk images.
        sb.validate()?;

        // Check for unsupported incompatible features
        let unsupported = sb.unsupported_incompat_features();
        if unsupported != 0 {
            log::error!(
                "ext2: unsupported incompatible features: {:#x}",
                unsupported
            );
            return Err("ext2 filesystem has unsupported features");
        }

        // Check for unsupported read-only compatible features
        let unsupported_ro = sb.unsupported_ro_compat_features();
        let read_write = if unsupported_ro != 0 {
            log::warn!(
                "ext2: unsupported RO_COMPAT features: {:#x}, mounting read-only",
                unsupported_ro
            );
            false
        } else {
            true
        };

        // Safe to unwrap: validate() already checked these won't fail
        let block_size = sb.block_size().unwrap();
        let inode_size = sb.inode_size();
        let block_group_count = sb.block_group_count().unwrap();

        // Block group descriptor table location:
        // - For 1KB blocks: starts at block 2 (byte offset 2048)
        // - For larger blocks: starts at block 1 (byte offset = block_size)
        let bgdt_offset = if block_size == 1024 {
            2048u64
        } else {
            block_size as u64
        };
        let bgdt_size = block_group_count as usize * core::mem::size_of::<BlockGroupDescriptor>();

        let mut bgdt_buf = alloc::vec![0u8; bgdt_size];
        device
            .read_at(bgdt_offset, &mut bgdt_buf)
            .await
            .map_err(|_| "failed to read block group descriptors")?;

        let desc_size = core::mem::size_of::<BlockGroupDescriptor>();
        let block_groups: Vec<BlockGroupDescriptor> = (0..block_group_count as usize)
            .map(|i| {
                // Safety: We allocated bgdt_buf with exactly block_group_count * desc_size bytes,
                // so i * desc_size + desc_size <= bgdt_buf.len() always holds.
                unsafe { core::ptr::read(bgdt_buf[i * desc_size..].as_ptr() as *const _) }
            })
            .collect();

        let fs = Arc::new(Self {
            device,
            block_size,
            inode_size,
            blocks_count: sb.blocks_count,
            inodes_per_group: sb.inodes_per_group,
            mutable: RwSpinlock::new(Ext2FsMutable {
                superblock: sb,
                block_groups,
            }),
            alloc_lock: AsyncMutex::new(()),
            self_ref: RwSpinlock::new(Weak::new()),
            read_write,
        });
        *fs.self_ref.write() = Arc::downgrade(&fs);
        Ok(fs)
    }

    // =========================================================================
    // Read operations
    // =========================================================================

    /// Read an inode by number (1-indexed).
    pub async fn read_inode(&self, ino: u32) -> Result<Inode, FsError> {
        if ino == 0 {
            return Err(FsError::NotFound);
        }

        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;

        // Read inode table block number from the block group descriptor
        let inode_table = {
            let m = self.mutable.read();
            if group as usize >= m.block_groups.len() {
                log::warn!("ext2: inode {} references out-of-bounds group {}", ino, group);
                return Err(FsError::NotFound);
            }
            m.block_groups[group as usize].inode_table
        };

        let offset =
            inode_table as u64 * self.block_size as u64 + index as u64 * self.inode_size as u64;

        // Safety: buf is 128 bytes and Inode is repr(C) with size <= 128 bytes.
        // The compile-time assert below ensures the buffer is large enough.
        const _: () = assert!(core::mem::size_of::<Inode>() <= 128);
        let mut buf = [0u8; 128];
        self.device
            .read_at(offset, &mut buf)
            .await
            .map_err(|_| FsError::NotReadable)?;

        Ok(unsafe { core::ptr::read(buf.as_ptr() as *const Inode) })
    }

    /// Resolve path to inode number.
    ///
    /// Defence-in-depth: `.` and `..` components are rejected here even though
    /// the VFS layer should have already canonicalised the path. This prevents
    /// mount-point escape if canonicalization is ever bypassed.
    pub async fn lookup(&self, path: &str) -> Result<u32, FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok(EXT2_ROOT_INO);
        }

        let mut current = EXT2_ROOT_INO;
        for component in path.split('/').filter(|s| !s.is_empty()) {
            // Reject . and .. to prevent traversal beyond mount boundary
            if component == "." || component == ".." {
                return Err(FsError::NotFound);
            }
            let inode = self.read_inode(current).await?;
            if !inode.is_dir() {
                return Err(FsError::NotFound);
            }
            current = self.find_entry(&inode, component).await?;
        }
        Ok(current)
    }

    /// Find directory entry by name.
    async fn find_entry(&self, dir: &Inode, name: &str) -> Result<u32, FsError> {
        let size = dir.size();
        let block_len = self.block_size as usize;
        let dir_entry_size = core::mem::size_of::<DirEntryRaw>();
        let mut offset = 0u64;
        let mut block_buf = alloc::vec![0u8; block_len];

        while offset < size {
            let file_block = (offset / self.block_size as u64) as u32;
            let block_num = self.get_block(dir, file_block).await?;

            if block_num != 0 {
                self.read_block(block_num, &mut block_buf).await?;

                let mut pos = 0usize;
                while pos < block_len {
                    // Bounds check before reading DirEntryRaw
                    if pos + dir_entry_size > block_len {
                        break;
                    }

                    let entry: DirEntryRaw =
                        unsafe { core::ptr::read(block_buf[pos..].as_ptr() as *const _) };

                    // rec_len must be at least 8 (header size) and must not extend past block
                    if entry.rec_len < 8 || pos + entry.rec_len as usize > block_len {
                        log::warn!("ext2: invalid dir entry rec_len {} at offset {}", entry.rec_len, pos);
                        break;
                    }

                    if entry.inode != 0 {
                        let name_len = entry.name_len as usize;
                        // Validate name_len fits within the record
                        if name_len > entry.rec_len as usize - 8 || name_len > 255 {
                            log::warn!("ext2: invalid dir entry name_len {} at offset {}", name_len, pos);
                            pos += entry.rec_len as usize;
                            continue;
                        }
                        if name_len == name.len() {
                            let name_start = pos + 8;
                            let entry_name = &block_buf[name_start..name_start + name_len];
                            if entry_name == name.as_bytes() {
                                return Ok(entry.inode);
                            }
                        }
                    }
                    pos += entry.rec_len as usize;
                }
            }
            offset += self.block_size as u64;
        }
        Err(FsError::NotFound)
    }

    /// List directory entries.
    async fn list_dir(&self, dir: &Inode) -> Result<Vec<DirEntry>, FsError> {
        let mut entries = Vec::new();
        let size = dir.size();
        let block_len = self.block_size as usize;
        let dir_entry_size = core::mem::size_of::<DirEntryRaw>();
        let mut offset = 0u64;
        let mut block_buf = alloc::vec![0u8; block_len];

        while offset < size {
            let file_block = (offset / self.block_size as u64) as u32;
            let block_num = self.get_block(dir, file_block).await?;

            if block_num != 0 {
                self.read_block(block_num, &mut block_buf).await?;

                let mut pos = 0usize;
                while pos < block_len {
                    // Bounds check before reading DirEntryRaw
                    if pos + dir_entry_size > block_len {
                        break;
                    }

                    let entry: DirEntryRaw =
                        unsafe { core::ptr::read(block_buf[pos..].as_ptr() as *const _) };

                    // rec_len must be at least 8 (header size) and must not extend past block
                    if entry.rec_len < 8 || pos + entry.rec_len as usize > block_len {
                        log::warn!("ext2: invalid dir entry rec_len {} at offset {}", entry.rec_len, pos);
                        break;
                    }

                    if entry.inode != 0 {
                        let name_len = entry.name_len as usize;
                        // Validate name_len fits within the record
                        if name_len > entry.rec_len as usize - 8 || name_len > 255 {
                            log::warn!("ext2: invalid dir entry name_len {} at offset {}", name_len, pos);
                            pos += entry.rec_len as usize;
                            continue;
                        }
                        let name_start = pos + 8;
                        let name_bytes = &block_buf[name_start..name_start + name_len];
                        if let Ok(name) = core::str::from_utf8(name_bytes) {
                            if name != "." && name != ".." {
                                entries.push(DirEntry {
                                    name: String::from(name),
                                    is_dir: entry.file_type == FT_DIR,
                                });
                            }
                        }
                    }
                    pos += entry.rec_len as usize;
                }
            }
            offset += self.block_size as u64;
        }
        Ok(entries)
    }

    /// Get block number for file block index (handles indirection).
    pub async fn get_block(&self, inode: &Inode, file_block: u32) -> Result<u32, FsError> {
        get_block_number(&*self.device, &inode.block, self.block_size, file_block).await
    }

    /// Set the physical block number for a file block index, handling indirect blocks.
    ///
    /// This is the write-side mirror of `get_block`. For file block indices
    /// beyond the 12 direct slots it will allocate indirect, double-indirect,
    /// or triple-indirect index blocks as needed.
    ///
    /// The caller must update the inode on disk after this returns (the inode's
    /// `block` array may have been mutated for direct or first-level indirect
    /// pointers).
    /// Set the physical block number for a logical file block index.
    ///
    /// Returns the number of *metadata* blocks (indirect index blocks) that
    /// were newly allocated. The caller must include these in `inode.blocks`
    /// alongside the data block itself.
    pub async fn set_block_number(
        &self,
        inode: &mut Inode,
        file_block: u32,
        value: u32,
    ) -> Result<u32, FsError> {
        let ptrs_per_block = self.block_size / 4;
        let mut meta_blocks: u32 = 0;

        // Direct blocks (0-11)
        if file_block < 12 {
            inode.block[file_block as usize] = value;
            return Ok(0);
        }

        // Single indirect block (12)
        let fb = file_block - 12;
        if fb < ptrs_per_block {
            if inode.block[12] == 0 {
                let ind = self.alloc_block().await?;
                // Zero out the new indirect block
                let zeroes = alloc::vec![0u8; self.block_size as usize];
                self.write_block(ind, &zeroes).await?;
                inode.block[12] = ind;
                meta_blocks += 1;
            }
            write_block_ptr(&*self.device, inode.block[12], fb, self.block_size, value)
                .await?;
            return Ok(meta_blocks);
        }

        // Double indirect block (13)
        let fb = fb - ptrs_per_block;
        if fb < ptrs_per_block * ptrs_per_block {
            if inode.block[13] == 0 {
                let dbl = self.alloc_block().await?;
                let zeroes = alloc::vec![0u8; self.block_size as usize];
                self.write_block(dbl, &zeroes).await?;
                inode.block[13] = dbl;
                meta_blocks += 1;
            }
            let idx1 = fb / ptrs_per_block;
            let idx2 = fb % ptrs_per_block;
            let ind = read_block_ptr(&*self.device, inode.block[13], idx1, self.block_size).await?;
            let ind = if ind == 0 {
                let new_ind = self.alloc_block().await?;
                let zeroes = alloc::vec![0u8; self.block_size as usize];
                self.write_block(new_ind, &zeroes).await?;
                write_block_ptr(
                    &*self.device,
                    inode.block[13],
                    idx1,
                    self.block_size,
                    new_ind,
                )
                .await?;
                meta_blocks += 1;
                new_ind
            } else {
                ind
            };
            write_block_ptr(&*self.device, ind, idx2, self.block_size, value).await?;
            return Ok(meta_blocks);
        }

        // Triple indirect block (14)
        let fb = fb - ptrs_per_block * ptrs_per_block;
        let pp = ptrs_per_block * ptrs_per_block;
        if inode.block[14] == 0 {
            let tri = self.alloc_block().await?;
            let zeroes = alloc::vec![0u8; self.block_size as usize];
            self.write_block(tri, &zeroes).await?;
            inode.block[14] = tri;
            meta_blocks += 1;
        }

        let idx1 = fb / pp;
        let idx2 = (fb % pp) / ptrs_per_block;
        let idx3 = fb % ptrs_per_block;

        let dbl = read_block_ptr(&*self.device, inode.block[14], idx1, self.block_size).await?;
        let dbl = if dbl == 0 {
            let new_dbl = self.alloc_block().await?;
            let zeroes = alloc::vec![0u8; self.block_size as usize];
            self.write_block(new_dbl, &zeroes).await?;
            write_block_ptr(
                &*self.device,
                inode.block[14],
                idx1,
                self.block_size,
                new_dbl,
            )
            .await?;
            meta_blocks += 1;
            new_dbl
        } else {
            dbl
        };

        let ind = read_block_ptr(&*self.device, dbl, idx2, self.block_size).await?;
        let ind = if ind == 0 {
            let new_ind = self.alloc_block().await?;
            let zeroes = alloc::vec![0u8; self.block_size as usize];
            self.write_block(new_ind, &zeroes).await?;
            write_block_ptr(&*self.device, dbl, idx2, self.block_size, new_ind).await?;
            meta_blocks += 1;
            new_ind
        } else {
            ind
        };

        write_block_ptr(&*self.device, ind, idx3, self.block_size, value).await?;
        Ok(meta_blocks)
    }

    /// Read a full block from disk.
    pub async fn read_block(&self, block: u32, buf: &mut [u8]) -> Result<(), FsError> {
        if block >= self.blocks_count {
            log::warn!("ext2: block {} out of range (total {})", block, self.blocks_count);
            return Err(FsError::NotReadable);
        }
        let offset = block as u64 * self.block_size as u64;
        self.device
            .read_at(offset, buf)
            .await
            .map_err(|_| FsError::NotReadable)?;
        Ok(())
    }

    // =========================================================================
    // Write operations
    // =========================================================================

    /// Write a full block to disk.
    ///
    /// The buffer must be exactly `block_size` bytes. The block number must be
    /// within the valid range for the filesystem.
    pub async fn write_block(&self, block: u32, data: &[u8]) -> Result<(), FsError> {
        if block >= self.blocks_count {
            log::warn!("ext2: write_block {} out of range (total {})", block, self.blocks_count);
            return Err(FsError::IoError);
        }
        if data.len() != self.block_size as usize {
            log::warn!(
                "ext2: write_block buffer size {} != block_size {}",
                data.len(),
                self.block_size
            );
            return Err(FsError::IoError);
        }
        let offset = block as u64 * self.block_size as u64;
        self.device
            .write_at(offset, data)
            .await
            .map_err(|_| FsError::IoError)?;
        Ok(())
    }

    /// Write an inode to disk.
    ///
    /// Serialises the `Inode` struct and writes it to the correct position
    /// in the inode table for the block group containing `ino`.
    pub async fn write_inode(&self, ino: u32, inode: &Inode) -> Result<(), FsError> {
        if ino == 0 {
            return Err(FsError::NotFound);
        }

        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;

        let inode_table = {
            let m = self.mutable.read();
            if group as usize >= m.block_groups.len() {
                log::warn!("ext2: write_inode {} references out-of-bounds group {}", ino, group);
                return Err(FsError::NotFound);
            }
            m.block_groups[group as usize].inode_table
        };

        let offset =
            inode_table as u64 * self.block_size as u64 + index as u64 * self.inode_size as u64;

        let bytes = inode.to_bytes();
        self.device
            .write_at(offset, &bytes)
            .await
            .map_err(|_| FsError::IoError)?;
        Ok(())
    }

    /// Write the in-memory superblock back to disk.
    ///
    /// Call this after modifying free block/inode counts in the superblock
    /// to persist the changes.
    pub async fn write_superblock(&self) -> Result<(), FsError> {
        let bytes = {
            let m = self.mutable.read();
            m.superblock.to_bytes()
        };
        self.device
            .write_at(SUPERBLOCK_OFFSET, &bytes)
            .await
            .map_err(|_| FsError::IoError)?;
        Ok(())
    }

    /// Write a block group descriptor back to disk.
    ///
    /// The descriptor is written to the block group descriptor table at
    /// the position for `group_num`.
    pub async fn write_block_group_descriptor(&self, group_num: u32) -> Result<(), FsError> {
        let bytes = {
            let m = self.mutable.read();
            if group_num as usize >= m.block_groups.len() {
                log::warn!(
                    "ext2: write_block_group_descriptor {} out of range",
                    group_num
                );
                return Err(FsError::IoError);
            }
            m.block_groups[group_num as usize].to_bytes()
        };

        let desc_size = core::mem::size_of::<BlockGroupDescriptor>() as u64;
        // Block group descriptor table location: block 2 for 1KB, block 1 for larger
        let bgdt_offset = if self.block_size == 1024 {
            2048u64
        } else {
            self.block_size as u64
        };
        let offset = bgdt_offset + group_num as u64 * desc_size;

        self.device
            .write_at(offset, &bytes)
            .await
            .map_err(|_| FsError::IoError)?;
        Ok(())
    }

    // =========================================================================
    // Accessors
    // =========================================================================

    /// Get block size.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Get the total number of blocks in the filesystem.
    pub fn blocks_count(&self) -> u32 {
        self.blocks_count
    }

    /// Get the number of inodes per block group.
    pub fn inodes_per_group(&self) -> u32 {
        self.inodes_per_group
    }

    /// Get a reference to the device.
    pub fn device(&self) -> &Arc<dyn BlockDevice> {
        &self.device
    }

    /// Get a reference to the mutable state lock.
    pub fn mutable(&self) -> &RwSpinlock<Ext2FsMutable> {
        &self.mutable
    }

    /// Check if the filesystem is mounted read-write.
    ///
    /// Returns `Ok(())` if writes are allowed, or `Err(ReadOnlyFs)` if the
    /// filesystem is mounted read-only due to unsupported RO_COMPAT features.
    pub fn check_writable(&self) -> Result<(), FsError> {
        if self.read_write {
            Ok(())
        } else {
            Err(FsError::ReadOnlyFs)
        }
    }

    /// Get the current Unix timestamp for metadata updates.
    ///
    /// Returns 0 if no RTC/wall clock is available. This is safe for ext2 -
    /// files will have epoch timestamps, which is valid behaviour and matches
    /// what happens on embedded systems without RTCs.
    pub fn current_timestamp(&self) -> u32 {
        // TODO: Implement wall clock time when RTC support is added.
        // For now, return 0 (Unix epoch) as a safe placeholder.
        0
    }

    /// Free all data blocks and indirect index blocks owned by an inode.
    ///
    /// Walks the direct block pointers (0–11), single indirect (12),
    /// double indirect (13), and triple indirect (14) block pointers,
    /// freeing every allocated block. This is used by `unlink` when the
    /// link count reaches zero, and will also be needed by `truncate`.
    pub async fn free_inode_blocks(&self, inode: &Inode) -> Result<(), FsError> {
        let ptrs_per_block = self.block_size() / 4;
        let block_size = self.block_size() as usize;

        // Free direct blocks (0–11)
        for i in 0..12 {
            if inode.block[i] != 0 {
                self.free_block(inode.block[i]).await?;
            }
        }

        // Free indirect blocks at each level of indirection
        for (slot, depth) in [(12, 1u32), (13, 2), (14, 3)] {
            if inode.block[slot] != 0 {
                self.free_indirect_block(inode.block[slot], block_size, ptrs_per_block, depth)
                    .await?;
            }
        }

        Ok(())
    }

    /// Recursively free an indirect block and all blocks it points to.
    ///
    /// `depth` is 1 for single indirect, 2 for double, 3 for triple.
    /// At depth 1, the block contains data block pointers which are freed.
    /// At depth > 1, each pointer leads to another level of indirection.
    /// The indirect block itself is freed after all its children.
    fn free_indirect_block<'a>(
        &'a self,
        block_num: u32,
        block_size: usize,
        ptrs_per_block: u32,
        depth: u32,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), FsError>> + Send + 'a>> {
        alloc::boxed::Box::pin(async move {
            let mut buf = alloc::vec![0u8; block_size];
            self.read_block(block_num, &mut buf).await?;

            for i in 0..ptrs_per_block as usize {
                let ptr = read_block_ptr_from_buf(&buf, i);
                if ptr == 0 {
                    continue;
                }
                if depth == 1 {
                    self.free_block(ptr).await?;
                } else {
                    self.free_indirect_block(ptr, block_size, ptrs_per_block, depth - 1)
                        .await?;
                }
            }

            // Free the indirect block itself
            self.free_block(block_num).await?;
            Ok(())
        })
    }

    /// Free all blocks from `start_file_block` onwards.
    ///
    /// This is used by truncate to shrink a file. It walks the inode's block
    /// pointers and frees all data blocks (and associated indirect blocks)
    /// that are beyond the specified starting block.
    ///
    /// Returns the number of blocks freed.
    pub async fn free_blocks_from(
        &self,
        inode: &mut Inode,
        start_file_block: u32,
    ) -> Result<u32, FsError> {
        let ptrs_per_block = self.block_size() / 4;
        let block_size = self.block_size() as usize;
        let mut freed = 0u32;

        // Free direct blocks (0-11)
        for i in start_file_block.min(12) as usize..12 {
            if inode.block[i] != 0 {
                self.free_block(inode.block[i]).await?;
                inode.block[i] = 0;
                freed += 1;
            }
        }

        // Single indirect (slot 12)
        // Covers file blocks 12 to 12 + ptrs_per_block - 1
        let single_start = 12u32;
        let single_end = single_start + ptrs_per_block;
        if start_file_block < single_end && inode.block[12] != 0 {
            if start_file_block <= single_start {
                // Free the entire single indirect block
                freed += self
                    .free_indirect_block_counting(inode.block[12], block_size, ptrs_per_block, 1)
                    .await?;
                inode.block[12] = 0;
            } else {
                // Partial free within single indirect
                let start_idx = (start_file_block - single_start) as usize;
                freed += self
                    .free_partial_indirect(inode.block[12], start_idx, ptrs_per_block, 1)
                    .await?;
            }
        }

        // Double indirect (slot 13)
        // Covers file blocks single_end to single_end + ptrs_per_block^2 - 1
        let double_start = single_end;
        let double_end = double_start + ptrs_per_block * ptrs_per_block;
        if start_file_block < double_end && inode.block[13] != 0 {
            if start_file_block <= double_start {
                // Free the entire double indirect block
                freed += self
                    .free_indirect_block_counting(inode.block[13], block_size, ptrs_per_block, 2)
                    .await?;
                inode.block[13] = 0;
            } else {
                // Partial free within double indirect
                let rel_block = start_file_block - double_start;
                freed += self
                    .free_partial_double_indirect(inode.block[13], rel_block, ptrs_per_block)
                    .await?;
            }
        }

        // Triple indirect (slot 14)
        // Covers file blocks double_end to double_end + ptrs_per_block^3 - 1
        let triple_start = double_end;
        if start_file_block <= triple_start && inode.block[14] != 0 {
            // Free the entire triple indirect block
            freed += self
                .free_indirect_block_counting(inode.block[14], block_size, ptrs_per_block, 3)
                .await?;
            inode.block[14] = 0;
        } else if inode.block[14] != 0 {
            // Partial free within triple indirect (very rare in practice)
            let rel_block = start_file_block - triple_start;
            freed += self
                .free_partial_triple_indirect(inode.block[14], rel_block, ptrs_per_block)
                .await?;
        }

        Ok(freed)
    }

    /// Free an indirect block and all blocks it points to, returning the count freed.
    fn free_indirect_block_counting<'a>(
        &'a self,
        block_num: u32,
        block_size: usize,
        ptrs_per_block: u32,
        depth: u32,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<u32, FsError>> + Send + 'a>>
    {
        alloc::boxed::Box::pin(async move {
            let mut buf = alloc::vec![0u8; block_size];
            self.read_block(block_num, &mut buf).await?;
            let mut freed = 0u32;

            for i in 0..ptrs_per_block as usize {
                let ptr = read_block_ptr_from_buf(&buf, i);
                if ptr == 0 {
                    continue;
                }
                if depth == 1 {
                    self.free_block(ptr).await?;
                    freed += 1;
                } else {
                    freed += self
                        .free_indirect_block_counting(ptr, block_size, ptrs_per_block, depth - 1)
                        .await?;
                }
            }

            // Free the indirect block itself
            self.free_block(block_num).await?;
            freed += 1;
            Ok(freed)
        })
    }

    /// Free blocks from a partial single indirect block starting at `start_idx`.
    async fn free_partial_indirect(
        &self,
        block_num: u32,
        start_idx: usize,
        ptrs_per_block: u32,
        depth: u32,
    ) -> Result<u32, FsError> {
        let block_size = self.block_size() as usize;
        let mut buf = alloc::vec![0u8; block_size];
        self.read_block(block_num, &mut buf).await?;
        let mut freed = 0u32;

        for i in start_idx..ptrs_per_block as usize {
            let ptr = read_block_ptr_from_buf(&buf, i);
            if ptr == 0 {
                continue;
            }
            if depth == 1 {
                self.free_block(ptr).await?;
                freed += 1;
            } else {
                freed += self
                    .free_indirect_block_counting(ptr, block_size, ptrs_per_block, depth - 1)
                    .await?;
            }
            // Zero the pointer in the buffer
            write_block_ptr_to_buf(&mut buf, i, 0);
        }

        // Write back the modified indirect block (keeping pointers before start_idx)
        self.write_block(block_num, &buf).await?;
        Ok(freed)
    }

    /// Free blocks from a partial double indirect block.
    async fn free_partial_double_indirect(
        &self,
        block_num: u32,
        rel_block: u32,
        ptrs_per_block: u32,
    ) -> Result<u32, FsError> {
        let block_size = self.block_size() as usize;
        let mut buf = alloc::vec![0u8; block_size];
        self.read_block(block_num, &mut buf).await?;
        let mut freed = 0u32;

        let first_idx = (rel_block / ptrs_per_block) as usize;
        let first_offset = (rel_block % ptrs_per_block) as usize;

        for i in first_idx..ptrs_per_block as usize {
            let ptr = read_block_ptr_from_buf(&buf, i);
            if ptr == 0 {
                continue;
            }
            if i == first_idx && first_offset > 0 {
                // Partial free within this single indirect
                freed += self
                    .free_partial_indirect(ptr, first_offset, ptrs_per_block, 1)
                    .await?;
            } else {
                // Free the entire single indirect
                freed += self
                    .free_indirect_block_counting(ptr, block_size, ptrs_per_block, 1)
                    .await?;
                write_block_ptr_to_buf(&mut buf, i, 0);
            }
        }

        // Write back the modified double indirect block
        self.write_block(block_num, &buf).await?;
        Ok(freed)
    }

    /// Free blocks from a partial triple indirect block.
    async fn free_partial_triple_indirect(
        &self,
        block_num: u32,
        rel_block: u32,
        ptrs_per_block: u32,
    ) -> Result<u32, FsError> {
        let block_size = self.block_size() as usize;
        let pp = ptrs_per_block * ptrs_per_block;
        let mut buf = alloc::vec![0u8; block_size];
        self.read_block(block_num, &mut buf).await?;
        let mut freed = 0u32;

        let first_idx = (rel_block / pp) as usize;
        let first_offset = rel_block % pp;

        for i in first_idx..ptrs_per_block as usize {
            let ptr = read_block_ptr_from_buf(&buf, i);
            if ptr == 0 {
                continue;
            }
            if i == first_idx && first_offset > 0 {
                // Partial free within this double indirect
                freed += self
                    .free_partial_double_indirect(ptr, first_offset, ptrs_per_block)
                    .await?;
            } else {
                // Free the entire double indirect
                freed += self
                    .free_indirect_block_counting(ptr, block_size, ptrs_per_block, 2)
                    .await?;
                write_block_ptr_to_buf(&mut buf, i, 0);
            }
        }

        // Write back the modified triple indirect block
        self.write_block(block_num, &buf).await?;
        Ok(freed)
    }

    /// Count the number of allocated blocks up to `max_blocks` file blocks.
    ///
    /// This is used by truncate to recalculate the inode's `blocks` field
    /// after freeing blocks, accounting for sparse holes.
    async fn count_allocated_blocks(&self, inode: &Inode, max_blocks: u32) -> u32 {
        let mut count = 0u32;

        // Count direct blocks
        for i in 0..12.min(max_blocks) as usize {
            if inode.block[i] != 0 {
                count += 1;
            }
        }

        // For simplicity, we don't count indirect metadata blocks here.
        // The blocks field in ext2 counts sectors, and includes metadata blocks,
        // but this simplified version only counts data blocks.
        // A more accurate implementation would walk the indirect structures.

        // TODO: For files with indirect blocks, count those as well.
        // For now, this is sufficient for most use cases.

        count
    }
}

/// Read a u32 block pointer from a buffer at the given index.
fn read_block_ptr_from_buf(buf: &[u8], index: usize) -> u32 {
    let offset = index * 4;
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

/// Write a u32 block pointer to a buffer at the given index.
fn write_block_ptr_to_buf(buf: &mut [u8], index: usize, value: u32) {
    let offset = index * 4;
    let bytes = value.to_le_bytes();
    buf[offset] = bytes[0];
    buf[offset + 1] = bytes[1];
    buf[offset + 2] = bytes[2];
    buf[offset + 3] = bytes[3];
}

#[async_trait]
impl Filesystem for Ext2Fs {
    async fn open(&self, path: &str) -> Result<Box<dyn File>, FsError> {
        let ino = self.lookup(path).await?;
        let inode = self.read_inode(ino).await?;

        if !inode.is_file() {
            return Err(FsError::NotFound); // Can't open directories as files
        }

        let fs_arc = self
            .self_ref
            .read()
            .upgrade()
            .ok_or(FsError::IoError)?;

        Ok(Box::new(Ext2File::new(fs_arc, inode, ino)))
    }

    async fn stat(&self, path: &str) -> Result<FileStat, FsError> {
        let ino = self.lookup(path).await?;
        let inode = self.read_inode(ino).await?;
        Ok(FileStat {
            size: inode.size(),
            is_dir: inode.is_dir(),
            mode: inode.mode,
            inode: ino as u64,
            nlinks: inode.links_count as u64,
            mtime: inode.mtime as u64,
            ctime: inode.ctime as u64,
            atime: inode.atime as u64,
        })
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let ino = self.lookup(path).await?;
        let inode = self.read_inode(ino).await?;

        if !inode.is_dir() {
            return Err(FsError::NotFound);
        }

        self.list_dir(&inode).await
    }

    /// Create a new regular file at the given path.
    ///
    /// Allocates a fresh inode, initialises it as a zero-length regular file,
    /// adds a directory entry in the parent directory, and returns the opened
    /// file handle. If directory entry insertion fails after the inode has been
    /// allocated, the inode is freed to prevent leaks.
    async fn create(&self, path: &str, mode: u16) -> Result<Box<dyn File>, FsError> {
        self.check_writable()?;
        let (parent_path, file_name) = split_parent_name(path)?;

        // Resolve parent directory
        let parent_ino = self.lookup(parent_path).await?;
        let parent_inode = self.read_inode(parent_ino).await?;
        if !parent_inode.is_dir() {
            return Err(FsError::NotFound);
        }

        // Check file does not already exist
        if self.find_entry(&parent_inode, file_name).await.is_ok() {
            return Err(FsError::AlreadyExists);
        }

        // Get Arc<Self> for file handle
        let fs_arc = self
            .self_ref
            .read()
            .upgrade()
            .ok_or(FsError::IoError)?;

        // Allocate a new inode (guarded for automatic rollback)
        let inode_guard = fs_arc.alloc_inode().await?;

        // Get current timestamp for metadata
        let now = self.current_timestamp();

        // Initialise the new inode as a regular file
        let new_inode = Inode {
            mode: S_IFREG | (mode & 0o7777),
            uid: 0,
            size: 0,
            atime: now,
            ctime: now,
            mtime: now,
            dtime: 0,
            gid: 0,
            links_count: 1,
            blocks: 0,
            flags: 0,
            osd1: 0,
            block: [0u32; 15],
            generation: 0,
            file_acl: 0,
            size_high: 0,
            faddr: 0,
            osd2: [0u8; 12],
        };

        // Add directory entry, consuming the guard on success
        // The guard is consumed here, which is the commit point
        let (new_ino, updated_parent) = self
            .add_dir_entry(parent_ino, parent_inode, file_name, inode_guard, FT_REG_FILE)
            .await?;

        // Write the new inode to disk (guard already consumed, inode is committed)
        self.write_inode(new_ino, &new_inode).await?;

        // Persist updated parent inode
        self.write_inode(parent_ino, &updated_parent).await?;

        Ok(Box::new(Ext2File::new(fs_arc, new_inode, new_ino)))
    }

    /// Remove (unlink) a file at the given path.
    ///
    /// Removes the directory entry from the parent, decrements the target
    /// inode's link count, and if links reach zero, frees all data blocks
    /// (direct, indirect, double-indirect, triple-indirect) and the inode.
    async fn unlink(&self, path: &str) -> Result<(), FsError> {
        self.check_writable()?;
        let (parent_path, file_name) = split_parent_name(path)?;

        // Resolve parent directory
        let parent_ino = self.lookup(parent_path).await?;
        let parent_inode = self.read_inode(parent_ino).await?;
        if !parent_inode.is_dir() {
            return Err(FsError::NotFound);
        }

        // Remove directory entry and get the removed inode number
        let (target_ino, updated_parent) = self
            .remove_dir_entry(parent_ino, parent_inode, file_name)
            .await?;

        // Persist updated parent inode
        self.write_inode(parent_ino, &updated_parent).await?;

        // Read and update target inode
        let mut target_inode = self.read_inode(target_ino).await?;
        target_inode.links_count = target_inode.links_count.saturating_sub(1);
        target_inode.ctime = self.current_timestamp();

        if target_inode.links_count == 0 {
            // Free all data blocks
            self.free_inode_blocks(&target_inode).await?;

            // Mark as deleted (dtime != 0 signals deletion to fsck)
            target_inode.dtime = 1;
            target_inode.set_size(0);
            target_inode.blocks = 0;

            // Write the zeroed inode, then free it
            self.write_inode(target_ino, &target_inode).await?;
            self.free_inode(target_ino).await?;
        } else {
            // Just persist the decremented link count
            self.write_inode(target_ino, &target_inode).await?;
        }

        Ok(())
    }

    /// Create a new directory at the given path.
    ///
    /// Allocates a new inode with directory mode, creates an initial block
    /// containing `.` (self-reference) and `..` (parent reference) entries,
    /// adds a directory entry in the parent, and updates link counts.
    ///
    /// # Ext2 semantics
    ///
    /// - The new directory's `links_count` is 2: one from the parent's entry,
    ///   one from its own `.` entry
    /// - The parent's `links_count` is incremented by 1 for the `..` back-reference
    /// - The block group's `used_dirs_count` is incremented
    async fn mkdir(&self, path: &str, mode: u16) -> Result<(), FsError> {
        self.check_writable()?;
        let (parent_path, dir_name) = split_parent_name(path)?;

        // Resolve parent directory
        let parent_ino = self.lookup(parent_path).await?;
        let parent_inode = self.read_inode(parent_ino).await?;
        if !parent_inode.is_dir() {
            return Err(FsError::NotFound);
        }

        // Check for duplicates
        if self.find_entry(&parent_inode, dir_name).await.is_ok() {
            return Err(FsError::AlreadyExists);
        }

        // Get Arc<Self> for RAII guards
        let fs_arc = self
            .self_ref
            .read()
            .upgrade()
            .ok_or(FsError::IoError)?;

        // Allocate a new inode for the directory (guarded for automatic rollback)
        let inode_guard = fs_arc.alloc_inode().await?;

        // Allocate initial block for the directory (guarded for automatic rollback)
        let block_guard = BlockGuard::new(fs_arc, self.alloc_block().await?);

        // Safety: We peek the block number here for use in the inode initialization,
        // but keep the guard alive until after add_dir_entry succeeds. If any
        // operation fails before we consume the guard, the block will be automatically
        // freed on drop.
        let initial_block = unsafe { block_guard.peek() };

        // Add entry in parent directory, consuming the inode guard
        // This is the commit point for the inode allocation
        let (new_ino, updated_parent) = self
            .add_dir_entry(parent_ino, parent_inode, dir_name, inode_guard, FT_DIR)
            .await?;

        // Get current timestamp for metadata
        let now = self.current_timestamp();

        // Initialise the new inode as a directory (now that we have the ino)
        let new_inode = Inode {
            mode: S_IFDIR | (mode & 0o7777),
            uid: 0,
            size: self.block_size(),
            atime: now,
            ctime: now,
            mtime: now,
            dtime: 0,
            gid: 0,
            links_count: 2, // '.' points to self, parent entry points to us
            blocks: self.block_size() / 512, // Sectors used by initial block
            flags: 0,
            osd1: 0,
            block: {
                let mut b = [0u32; 15];
                b[0] = initial_block;
                b
            },
            generation: 0,
            file_acl: 0,
            size_high: 0,
            faddr: 0,
            osd2: [0u8; 12],
        };

        // Write . and .. entries to initial block (using the committed inode number)
        let init_buf = self.init_dir_block(new_ino, parent_ino);
        self.write_block(initial_block, &init_buf).await?;

        // Write inode to disk
        self.write_inode(new_ino, &new_inode).await?;

        // Commit the block allocation now that inode is written
        block_guard.consume();

        // Increment parent's link count (for .. reference)
        let mut updated_parent = updated_parent;
        updated_parent.links_count += 1;
        self.write_inode(parent_ino, &updated_parent).await?;

        // Update used_dirs_count in block group descriptor
        self.inc_used_dirs_count(new_ino).await?;

        Ok(())
    }

    /// Remove an empty directory at the given path.
    ///
    /// Verifies the directory contains only `.` and `..` entries, removes
    /// the directory entry from the parent, frees the directory's blocks
    /// and inode, and updates link counts.
    ///
    /// # Errors
    ///
    /// - `NotFound` if the path doesn't exist
    /// - `NotDirectory` if the target is not a directory
    /// - `NotEmpty` if the directory contains entries other than `.` and `..`
    async fn rmdir(&self, path: &str) -> Result<(), FsError> {
        self.check_writable()?;
        let (parent_path, dir_name) = split_parent_name(path)?;

        // Validate not removing root
        if dir_name.is_empty() || path.trim_matches('/').is_empty() {
            return Err(FsError::IoError); // Can't remove root
        }

        // Resolve parent directory
        let parent_ino = self.lookup(parent_path).await?;
        let parent_inode = self.read_inode(parent_ino).await?;
        if !parent_inode.is_dir() {
            return Err(FsError::NotFound);
        }

        // Find target entry and get inode
        let target_ino = self.find_entry(&parent_inode, dir_name).await?;
        let target_inode = self.read_inode(target_ino).await?;

        // Verify target is a directory
        if !target_inode.is_dir() {
            return Err(FsError::NotDirectory);
        }

        // Verify directory is empty
        if !self.is_dir_empty(&target_inode).await? {
            return Err(FsError::NotEmpty);
        }

        // Remove entry from parent
        let (_, updated_parent) = self
            .remove_dir_entry(parent_ino, parent_inode, dir_name)
            .await?;

        // Free the directory's data blocks
        self.free_inode_blocks(&target_inode).await?;

        // Mark inode as deleted and free it
        let mut deleted_inode = target_inode;
        deleted_inode.dtime = 1;
        deleted_inode.set_size(0);
        deleted_inode.blocks = 0;
        deleted_inode.links_count = 0;
        self.write_inode(target_ino, &deleted_inode).await?;
        self.free_inode(target_ino).await?;

        // Decrement parent's link count (removing .. reference)
        let mut updated_parent = updated_parent;
        updated_parent.links_count = updated_parent.links_count.saturating_sub(1);
        self.write_inode(parent_ino, &updated_parent).await?;

        // Update used_dirs_count in block group descriptor
        self.dec_used_dirs_count(target_ino).await?;

        Ok(())
    }

    /// Truncate (or extend) a file to the given size.
    ///
    /// - **Shrinking**: Frees blocks beyond the new size. Zeros the partial
    ///   last block if the new size is not block-aligned.
    /// - **Growing**: Extends the file size (sparse extension - no blocks
    ///   allocated). Reads beyond the old size will return zeros.
    ///
    /// Updates the inode's size, blocks count, mtime, and ctime.
    ///
    /// # Errors
    ///
    /// - `NotFound` if the path doesn't exist
    /// - `IsDirectory` if the path is a directory
    /// - `ReadOnlyFs` if the filesystem has unsupported RO_COMPAT features
    async fn truncate(&self, path: &str, new_size: u64) -> Result<(), FsError> {
        self.check_writable()?;

        let ino = self.lookup(path).await?;
        let mut inode = self.read_inode(ino).await?;

        if !inode.is_file() {
            return Err(FsError::IsDirectory);
        }

        let current_size = inode.size();
        if new_size == current_size {
            return Ok(());
        }

        let now = self.current_timestamp();

        if new_size > current_size {
            // Grow: sparse extension - just update the size
            inode.set_size(new_size);
        } else {
            // Shrink: free blocks beyond new size
            let block_size = self.block_size() as u64;
            let new_block_count = new_size.div_ceil(block_size) as u32;

            // Zero the partial last block if new_size is not block-aligned
            if new_size % block_size != 0 && new_block_count > 0 {
                let last_block_idx = new_block_count - 1;
                let block_num = self.get_block(&inode, last_block_idx).await?;
                if block_num != 0 {
                    let offset_in_block = (new_size % block_size) as usize;
                    let mut buf = alloc::vec![0u8; self.block_size() as usize];
                    self.read_block(block_num, &mut buf).await?;
                    buf[offset_in_block..].fill(0);
                    self.write_block(block_num, &buf).await?;
                }
            }

            // Free blocks starting from new_block_count
            let freed = self.free_blocks_from(&mut inode, new_block_count).await?;
            let _ = freed; // Blocks freed count (for debugging)

            // Update block count (sectors used)
            // For sparse files, we need to count actual allocated blocks
            let actual_blocks = self.count_allocated_blocks(&inode, new_block_count).await;
            inode.blocks = actual_blocks * (self.block_size() / 512);
            inode.set_size(new_size);
        }

        inode.mtime = now;
        inode.ctime = now;
        self.write_inode(ino, &inode).await?;

        Ok(())
    }

    /// Flush all pending metadata and data to the backing store.
    ///
    /// Writes the in-memory superblock and all block group descriptors to disk,
    /// then syncs the underlying block device.
    async fn sync(&self) -> Result<(), FsError> {
        // Write superblock (ensures free counts are persisted)
        self.write_superblock().await?;

        // Write all block group descriptors
        let num_groups = {
            let m = self.mutable.read();
            m.block_groups.len()
        };
        for group in 0..num_groups {
            self.write_block_group_descriptor(group as u32).await?;
        }

        // Sync the underlying block device
        self.device
            .sync()
            .await
            .map_err(|_| FsError::IoError)?;

        Ok(())
    }
}

/// Split a path into (parent_path, file_name).
///
/// The path is relative to the filesystem mount point (no leading slash after
/// mount-point stripping). Returns `NotFound` if the path has no name component.
fn split_parent_name(path: &str) -> Result<(&str, &str), FsError> {
    let path = path.trim_matches('/');
    if path.is_empty() {
        return Err(FsError::NotFound);
    }
    match path.rfind('/') {
        Some(idx) => Ok((&path[..idx], &path[idx + 1..])),
        None => Ok(("", path)), // File in root directory
    }
}
