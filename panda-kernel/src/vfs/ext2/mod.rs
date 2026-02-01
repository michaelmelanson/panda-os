//! Ext2 filesystem driver.
//!
//! This module implements an ext2 filesystem driver with async I/O, supporting
//! both read and write operations. Mutable filesystem state (superblock and
//! block group descriptors) is protected by a single `RwSpinlock` to allow
//! concurrent reads while serialising allocation and metadata updates.

pub mod bitmap;
mod file;
mod structs;

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
    pub async fn set_block_number(
        &self,
        inode: &mut Inode,
        file_block: u32,
        value: u32,
    ) -> Result<(), FsError> {
        let ptrs_per_block = self.block_size / 4;

        // Direct blocks (0-11)
        if file_block < 12 {
            inode.block[file_block as usize] = value;
            return Ok(());
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
            }
            return write_block_ptr(&*self.device, inode.block[12], fb, self.block_size, value)
                .await;
        }

        // Double indirect block (13)
        let fb = fb - ptrs_per_block;
        if fb < ptrs_per_block * ptrs_per_block {
            if inode.block[13] == 0 {
                let dbl = self.alloc_block().await?;
                let zeroes = alloc::vec![0u8; self.block_size as usize];
                self.write_block(dbl, &zeroes).await?;
                inode.block[13] = dbl;
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
                new_ind
            } else {
                ind
            };
            return write_block_ptr(&*self.device, ind, idx2, self.block_size, value).await;
        }

        // Triple indirect block (14)
        let fb = fb - ptrs_per_block * ptrs_per_block;
        let pp = ptrs_per_block * ptrs_per_block;
        if inode.block[14] == 0 {
            let tri = self.alloc_block().await?;
            let zeroes = alloc::vec![0u8; self.block_size as usize];
            self.write_block(tri, &zeroes).await?;
            inode.block[14] = tri;
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
            new_ind
        } else {
            ind
        };

        write_block_ptr(&*self.device, ind, idx3, self.block_size, value).await
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
}
