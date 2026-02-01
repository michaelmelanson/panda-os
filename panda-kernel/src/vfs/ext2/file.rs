//! Ext2 file implementation.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use async_trait::async_trait;

use super::{Ext2Fs, Inode};
use crate::vfs::{File, FileStat, FsError, SeekFrom};

/// An open file in an ext2 filesystem.
///
/// Holds an `Arc<Ext2Fs>` so that writes can allocate blocks, update the
/// inode on disk, and invalidate caches through the filesystem instance.
pub struct Ext2File {
    /// The filesystem this file belongs to.
    fs: Arc<Ext2Fs>,
    /// The file's inode data (cached in memory; written back on mutation).
    inode: Inode,
    /// The inode number (not stored in the on-disk inode; derived from table position).
    ino: u32,
    /// Total file size (cached from inode for fast access).
    size: u64,
    /// Current file position for sequential read/write.
    pos: u64,
    /// Cache for indirect block data to speed up sequential reads.
    /// Stores (indirect_block_number, cached_pointers).
    indirect_cache: Option<(u32, Vec<u32>)>,
}

impl Ext2File {
    /// Create a new ext2 file.
    pub fn new(fs: Arc<Ext2Fs>, inode: Inode, ino: u32) -> Self {
        Self {
            size: inode.size(),
            fs,
            inode,
            ino,
            pos: 0,
            indirect_cache: None,
        }
    }

    /// Get block number for file block index (handles indirection with caching).
    async fn get_block(&mut self, file_block: u32) -> Result<u32, FsError> {
        let block_size = self.fs.block_size();
        let ptrs_per_block = block_size / 4;

        // Direct blocks (0-11) - no caching needed
        if file_block < 12 {
            return Ok(self.inode.block[file_block as usize]);
        }

        // For indirect blocks, use the cached block lookup
        self.get_block_indirect(file_block, ptrs_per_block).await
    }

    /// Handle indirect block lookup with caching.
    async fn get_block_indirect(
        &mut self,
        file_block: u32,
        ptrs_per_block: u32,
    ) -> Result<u32, FsError> {
        // Single indirect block (12)
        let fb = file_block - 12;
        if fb < ptrs_per_block {
            return self.read_cached_ptr(self.inode.block[12], fb).await;
        }

        // Double indirect block (13)
        let fb = fb - ptrs_per_block;
        if fb < ptrs_per_block * ptrs_per_block {
            let ind = self
                .read_cached_ptr(self.inode.block[13], fb / ptrs_per_block)
                .await?;
            return self.read_cached_ptr(ind, fb % ptrs_per_block).await;
        }

        // Triple indirect block (14)
        let fb = fb - ptrs_per_block * ptrs_per_block;
        let pp = ptrs_per_block * ptrs_per_block;
        let dbl = self.read_cached_ptr(self.inode.block[14], fb / pp).await?;
        let ind = self
            .read_cached_ptr(dbl, (fb % pp) / ptrs_per_block)
            .await?;
        self.read_cached_ptr(ind, fb % ptrs_per_block).await
    }

    /// Read a block pointer, using cache if available.
    ///
    /// This caches the entire indirect block on first access, making sequential
    /// reads through the same indirect block much faster.
    async fn read_cached_ptr(&mut self, block: u32, index: u32) -> Result<u32, FsError> {
        if block == 0 {
            return Ok(0);
        }

        let block_size = self.fs.block_size();

        // Check cache
        if let Some((cached_block, ref pointers)) = self.indirect_cache {
            if cached_block == block {
                return Ok(pointers.get(index as usize).copied().unwrap_or(0));
            }
        }

        // Cache miss - read the entire indirect block
        let ptrs_per_block = (block_size / 4) as usize;
        let mut buf = vec![0u8; block_size as usize];
        let offset = block as u64 * block_size as u64;
        self.fs
            .device()
            .read_at(offset, &mut buf)
            .await
            .map_err(|_| FsError::NotReadable)?;

        // Parse all pointers from the block
        let pointers: Vec<u32> = (0..ptrs_per_block)
            .map(|i| {
                let start = i * 4;
                u32::from_le_bytes([buf[start], buf[start + 1], buf[start + 2], buf[start + 3]])
            })
            .collect();

        let result = pointers.get(index as usize).copied().unwrap_or(0);

        // Update cache
        self.indirect_cache = Some((block, pointers));

        Ok(result)
    }
}

#[async_trait]
impl File for Ext2File {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        if self.pos >= self.size {
            return Ok(0);
        }

        let block_size = self.fs.block_size();
        let to_read = core::cmp::min(buf.len() as u64, self.size - self.pos) as usize;
        let mut done = 0;

        while done < to_read {
            let file_block = (self.pos / block_size as u64) as u32;
            let block_off = (self.pos % block_size as u64) as usize;
            let remaining_in_block = block_size as usize - block_off;
            let chunk = core::cmp::min(remaining_in_block, to_read - done);

            let block_num = self.get_block(file_block).await?;

            if block_num == 0 {
                // Sparse hole - fill with zeros
                buf[done..done + chunk].fill(0);
            } else {
                let disk_off = block_num as u64 * block_size as u64 + block_off as u64;
                self.fs
                    .device()
                    .read_at(disk_off, &mut buf[done..done + chunk])
                    .await
                    .map_err(|_| FsError::NotReadable)?;
            }

            done += chunk;
            self.pos += chunk as u64;
        }

        Ok(done)
    }

    /// Write data to the file at the current position.
    ///
    /// Handles block allocation for sparse holes and file extension. Uses
    /// read-modify-write for partial block writes to preserve existing data.
    /// Updates the inode size and block pointers on disk after each write.
    /// Invalidates the indirect block cache when new blocks are allocated
    /// beyond the direct range.
    async fn write(&mut self, buf: &[u8]) -> Result<usize, FsError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let block_size = self.fs.block_size();
        let mut done = 0usize;
        let mut inode_dirty = false;

        while done < buf.len() {
            let file_block = (self.pos / block_size as u64) as u32;
            let block_off = (self.pos % block_size as u64) as usize;
            let remaining_in_block = block_size as usize - block_off;
            let chunk = core::cmp::min(remaining_in_block, buf.len() - done);

            // Get the current physical block (may be 0 for sparse/unallocated)
            let mut block_num = self.get_block(file_block).await?;

            if block_num == 0 {
                // Allocate a new block
                block_num = self.fs.alloc_block().await?;
                // Zero out the new block so partial writes have clean surroundings
                let zeroes = vec![0u8; block_size as usize];
                self.fs.write_block(block_num, &zeroes).await?;
                // Record the new block in the inode's block map
                self.fs
                    .set_block_number(&mut self.inode, file_block, block_num)
                    .await?;
                // Update the inode's 512-byte block count
                self.inode.blocks += block_size / 512;
                inode_dirty = true;
                // Invalidate indirect cache since block map changed
                if file_block >= 12 {
                    self.indirect_cache = None;
                }
            }

            // Write the data
            if chunk == block_size as usize {
                // Full block write — no read-modify-write needed
                self.fs
                    .write_block(block_num, &buf[done..done + chunk])
                    .await?;
            } else {
                // Partial block write — read-modify-write
                let mut block_buf = vec![0u8; block_size as usize];
                self.fs.read_block(block_num, &mut block_buf).await?;
                block_buf[block_off..block_off + chunk].copy_from_slice(&buf[done..done + chunk]);
                self.fs.write_block(block_num, &block_buf).await?;
            }

            done += chunk;
            self.pos += chunk as u64;

            // Extend file size if we wrote past the end
            if self.pos > self.size {
                self.size = self.pos;
                self.inode.set_size(self.size);
                inode_dirty = true;
            }
        }

        // Persist inode changes (size, block pointers, block count)
        if inode_dirty {
            self.fs.write_inode(self.ino, &self.inode).await?;
        }

        Ok(done)
    }

    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError> {
        let new_pos = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(n) => self.size as i64 + n,
        };
        if new_pos < 0 {
            return Err(FsError::InvalidOffset);
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }

    async fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            size: self.size,
            is_dir: false,
            mode: self.inode.mode,
            inode: self.ino as u64,
            nlinks: self.inode.links_count as u64,
            mtime: self.inode.mtime as u64,
            ctime: self.inode.ctime as u64,
            atime: self.inode.atime as u64,
        })
    }
}
