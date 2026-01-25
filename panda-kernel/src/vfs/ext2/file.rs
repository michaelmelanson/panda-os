//! Ext2 file implementation.

use alloc::boxed::Box;
use alloc::sync::Arc;
use async_trait::async_trait;

use super::Inode;
use crate::resource::BlockDevice;
use crate::vfs::{File, FileStat, FsError, SeekFrom};

/// An open file in an ext2 filesystem.
pub struct Ext2File {
    /// The block device.
    device: Arc<dyn BlockDevice>,
    /// The file's inode data.
    inode: Inode,
    /// Block size.
    block_size: u32,
    /// Total file size.
    size: u64,
    /// Current read position.
    pos: u64,
}

impl Ext2File {
    /// Create a new ext2 file.
    pub fn new(device: Arc<dyn BlockDevice>, inode: Inode, block_size: u32) -> Self {
        Self {
            size: inode.size(),
            device,
            inode,
            block_size,
            pos: 0,
        }
    }

    /// Get block number for file block index (handles indirection).
    async fn get_block(&self, file_block: u32) -> Result<u32, FsError> {
        let ptrs_per_block = self.block_size / 4;

        // Direct blocks (0-11)
        if file_block < 12 {
            return Ok(self.inode.block[file_block as usize]);
        }

        // Indirect block (12)
        let fb = file_block - 12;
        if fb < ptrs_per_block {
            return self.read_block_ptr(self.inode.block[12], fb).await;
        }

        // Double indirect block (13)
        let fb = fb - ptrs_per_block;
        if fb < ptrs_per_block * ptrs_per_block {
            let ind = self
                .read_block_ptr(self.inode.block[13], fb / ptrs_per_block)
                .await?;
            return self.read_block_ptr(ind, fb % ptrs_per_block).await;
        }

        // Triple indirect block (14)
        let fb = fb - ptrs_per_block * ptrs_per_block;
        let pp = ptrs_per_block * ptrs_per_block;
        let dbl = self.read_block_ptr(self.inode.block[14], fb / pp).await?;
        let ind = self.read_block_ptr(dbl, (fb % pp) / ptrs_per_block).await?;
        self.read_block_ptr(ind, fb % ptrs_per_block).await
    }

    /// Read a block pointer from an indirect block.
    async fn read_block_ptr(&self, block: u32, index: u32) -> Result<u32, FsError> {
        if block == 0 {
            return Ok(0);
        }
        let offset = block as u64 * self.block_size as u64 + index as u64 * 4;
        let mut buf = [0u8; 4];
        self.device
            .read_at(offset, &mut buf)
            .await
            .map_err(|_| FsError::NotReadable)?;
        Ok(u32::from_le_bytes(buf))
    }
}

#[async_trait]
impl File for Ext2File {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        if self.pos >= self.size {
            return Ok(0);
        }

        let to_read = core::cmp::min(buf.len() as u64, self.size - self.pos) as usize;
        let mut done = 0;

        while done < to_read {
            let file_block = (self.pos / self.block_size as u64) as u32;
            let block_off = (self.pos % self.block_size as u64) as usize;
            let remaining_in_block = self.block_size as usize - block_off;
            let chunk = core::cmp::min(remaining_in_block, to_read - done);

            let block_num = self.get_block(file_block).await?;

            if block_num == 0 {
                // Sparse hole - fill with zeros
                buf[done..done + chunk].fill(0);
            } else {
                let disk_off = block_num as u64 * self.block_size as u64 + block_off as u64;
                self.device
                    .read_at(disk_off, &mut buf[done..done + chunk])
                    .await
                    .map_err(|_| FsError::NotReadable)?;
            }

            done += chunk;
            self.pos += chunk as u64;
        }

        Ok(done)
    }

    async fn write(&mut self, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotWritable)
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
        })
    }
}
