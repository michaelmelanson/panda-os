//! Ext2 filesystem driver (read-only).
//!
//! This module implements a read-only ext2 filesystem driver with async I/O.

mod file;
mod structs;

pub use file::Ext2File;
pub use structs::*;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;

use crate::resource::BlockDevice;
use crate::vfs::{DirEntry, File, FileStat, Filesystem, FsError};

/// An ext2 filesystem instance.
pub struct Ext2Fs {
    /// The underlying block device.
    device: Arc<dyn BlockDevice>,
    /// Block size in bytes.
    block_size: u32,
    /// Inode size in bytes.
    inode_size: u32,
    /// Inodes per block group.
    inodes_per_group: u32,
    /// Block group descriptors.
    block_groups: Vec<BlockGroupDescriptor>,
}

impl Ext2Fs {
    /// Mount an ext2 filesystem from a block device.
    pub async fn mount(device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, &'static str> {
        // Read superblock
        let mut sb_buf = [0u8; 1024];
        device
            .read_at(SUPERBLOCK_OFFSET, &mut sb_buf)
            .await
            .map_err(|_| "failed to read superblock")?;

        // Safety: We're reading a well-defined C struct from disk
        let sb: Superblock = unsafe { core::ptr::read(sb_buf.as_ptr() as *const _) };

        if sb.magic != EXT2_SUPER_MAGIC {
            return Err("invalid ext2 magic number");
        }

        let block_size = sb.block_size();
        let inode_size = sb.inode_size();
        let block_group_count = sb.block_group_count();

        // Block group descriptor table location:
        // - For 1KB blocks: starts at block 2 (byte offset 2048)
        // - For larger blocks: starts at block 1 (byte offset = block_size)
        let bgdt_offset = if block_size == 1024 {
            2048u64
        } else {
            block_size as u64
        };
        let bgdt_size = block_group_count as usize * 32; // 32 bytes per descriptor

        let mut bgdt_buf = alloc::vec![0u8; bgdt_size];
        device
            .read_at(bgdt_offset, &mut bgdt_buf)
            .await
            .map_err(|_| "failed to read block group descriptors")?;

        let block_groups: Vec<BlockGroupDescriptor> = (0..block_group_count as usize)
            .map(|i| unsafe { core::ptr::read(bgdt_buf[i * 32..].as_ptr() as *const _) })
            .collect();

        Ok(Arc::new(Self {
            device,
            block_size,
            inode_size,
            inodes_per_group: sb.inodes_per_group,
            block_groups,
        }))
    }

    /// Read an inode by number (1-indexed).
    pub async fn read_inode(&self, ino: u32) -> Result<Inode, FsError> {
        if ino == 0 {
            return Err(FsError::NotFound);
        }

        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        let bgd = &self.block_groups[group as usize];

        let offset =
            bgd.inode_table as u64 * self.block_size as u64 + index as u64 * self.inode_size as u64;

        let mut buf = [0u8; 128]; // Read at least 128 bytes (minimum inode size)
        self.device
            .read_at(offset, &mut buf)
            .await
            .map_err(|_| FsError::NotReadable)?;

        Ok(unsafe { core::ptr::read(buf.as_ptr() as *const Inode) })
    }

    /// Resolve path to inode number.
    pub async fn lookup(&self, path: &str) -> Result<u32, FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok(EXT2_ROOT_INO);
        }

        let mut current = EXT2_ROOT_INO;
        for component in path.split('/').filter(|s| !s.is_empty()) {
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
        let mut offset = 0u64;
        let mut block_buf = alloc::vec![0u8; self.block_size as usize];

        while offset < size {
            let file_block = (offset / self.block_size as u64) as u32;
            let block_num = self.get_block(dir, file_block).await?;

            if block_num != 0 {
                self.read_block(block_num, &mut block_buf).await?;

                let mut pos = 0usize;
                while pos < self.block_size as usize {
                    let entry: DirEntryRaw =
                        unsafe { core::ptr::read(block_buf[pos..].as_ptr() as *const _) };

                    if entry.rec_len == 0 {
                        break;
                    }

                    if entry.inode != 0 && entry.name_len as usize == name.len() {
                        let entry_name = &block_buf[pos + 8..][..entry.name_len as usize];
                        if entry_name == name.as_bytes() {
                            return Ok(entry.inode);
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
        let mut offset = 0u64;
        let mut block_buf = alloc::vec![0u8; self.block_size as usize];

        while offset < size {
            let file_block = (offset / self.block_size as u64) as u32;
            let block_num = self.get_block(dir, file_block).await?;

            if block_num != 0 {
                self.read_block(block_num, &mut block_buf).await?;

                let mut pos = 0usize;
                while pos < self.block_size as usize {
                    let entry: DirEntryRaw =
                        unsafe { core::ptr::read(block_buf[pos..].as_ptr() as *const _) };

                    if entry.rec_len == 0 {
                        break;
                    }

                    if entry.inode != 0 {
                        let name_bytes = &block_buf[pos + 8..][..entry.name_len as usize];
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
        let ptrs_per_block = self.block_size / 4;

        // Direct blocks (0-11)
        if file_block < 12 {
            return Ok(inode.block[file_block as usize]);
        }

        // Indirect block (12)
        let fb = file_block - 12;
        if fb < ptrs_per_block {
            return self.read_block_ptr(inode.block[12], fb).await;
        }

        // Double indirect block (13)
        let fb = fb - ptrs_per_block;
        if fb < ptrs_per_block * ptrs_per_block {
            let ind = self
                .read_block_ptr(inode.block[13], fb / ptrs_per_block)
                .await?;
            return self.read_block_ptr(ind, fb % ptrs_per_block).await;
        }

        // Triple indirect block (14)
        let fb = fb - ptrs_per_block * ptrs_per_block;
        let pp = ptrs_per_block * ptrs_per_block;
        let dbl = self.read_block_ptr(inode.block[14], fb / pp).await?;
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

    /// Read a full block.
    async fn read_block(&self, block: u32, buf: &mut [u8]) -> Result<(), FsError> {
        let offset = block as u64 * self.block_size as u64;
        self.device
            .read_at(offset, buf)
            .await
            .map_err(|_| FsError::NotReadable)?;
        Ok(())
    }

    /// Get block size.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Get a reference to the device.
    pub fn device(&self) -> &Arc<dyn BlockDevice> {
        &self.device
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

        // We need an Arc<Ext2Fs> but we only have &self.
        // This is a design issue - the Filesystem trait should probably work with Arc<Self>.
        // For now, we'll create the file with the device directly.
        Ok(Box::new(Ext2File::new(
            self.device.clone(),
            inode,
            self.block_size,
        )))
    }

    async fn stat(&self, path: &str) -> Result<FileStat, FsError> {
        let ino = self.lookup(path).await?;
        let inode = self.read_inode(ino).await?;
        Ok(FileStat {
            size: inode.size(),
            is_dir: inode.is_dir(),
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
