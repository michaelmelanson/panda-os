//! Ext2 directory entry manipulation.
//!
//! This module provides functions for inserting and removing directory entries
//! in ext2 directory blocks. Ext2 directories are stored as linked lists of
//! variable-length records within data blocks:
//!
//! ```text
//! +--------+--------+--------+-----+--------+
//! | entry0 | entry1 | entry2 | ... | entryN |
//! +--------+--------+--------+-----+--------+
//! ```
//!
//! Each entry has a fixed 8-byte header (`DirEntryRaw`) followed by the name.
//! The `rec_len` field is the distance to the next entry (4-byte aligned), and
//! the last entry in a block has `rec_len` extending to the end of the block.
//!
//! ## Inserting entries
//!
//! `add_dir_entry` scans existing blocks for space — either a deleted entry
//! with sufficient `rec_len`, or slack space in the last active entry of a
//! block (split). If no space exists, a new block is allocated for the
//! directory.
//!
//! ## Removing entries
//!
//! `remove_dir_entry` finds the named entry and either merges it with the
//! previous entry (by extending the previous entry's `rec_len`) or, if it is
//! the first entry in its block, zeroes the inode field to mark it deleted.

use alloc::vec;

use super::Ext2Fs;
use super::structs::{DirEntryRaw, Inode};
use crate::vfs::FsError;

/// Minimum ext2 directory entry size (8-byte header, no name).
const DIR_ENTRY_HEADER_SIZE: usize = 8;

/// Align a value up to a 4-byte boundary.
fn align4(val: usize) -> usize {
    (val + 3) & !3
}

/// Calculate the actual on-disk size needed for a directory entry with the
/// given name length (header + name, rounded up to 4 bytes).
fn entry_size(name_len: usize) -> usize {
    align4(DIR_ENTRY_HEADER_SIZE + name_len)
}

impl Ext2Fs {
    /// Add a directory entry in the directory identified by `dir_ino`.
    ///
    /// Scans the directory's data blocks for space to insert a new entry
    /// pointing to `target_ino` with the given `name` and `file_type`.
    ///
    /// Returns the updated directory inode (the caller must write it back
    /// to disk with `write_inode`).
    ///
    /// # Errors
    ///
    /// - `AlreadyExists` if an entry with the same name already exists.
    /// - `NoSpace` if the filesystem has no free blocks for directory expansion.
    /// - `IoError` on disk I/O failure.
    pub async fn add_dir_entry(
        &self,
        dir_ino: u32,
        mut dir_inode: Inode,
        name: &str,
        target_ino: u32,
        file_type: u8,
    ) -> Result<Inode, FsError> {
        let name_bytes = name.as_bytes();
        if name_bytes.is_empty() || name_bytes.len() > 255 {
            return Err(FsError::IoError);
        }

        let needed = entry_size(name_bytes.len());
        let block_size = self.block_size() as usize;
        let dir_size = dir_inode.size();
        let num_blocks =
            ((dir_size + self.block_size() as u64 - 1) / self.block_size() as u64) as u32;

        let mut block_buf = vec![0u8; block_size];

        // Scan existing directory blocks
        for file_block in 0..num_blocks {
            let block_num = self.get_block(&dir_inode, file_block).await?;
            if block_num == 0 {
                continue;
            }

            self.read_block(block_num, &mut block_buf).await?;

            let mut pos = 0usize;

            while pos < block_size {
                if pos + DIR_ENTRY_HEADER_SIZE > block_size {
                    break;
                }

                let entry: DirEntryRaw =
                    unsafe { core::ptr::read(block_buf[pos..].as_ptr() as *const _) };

                if entry.rec_len < 8 || pos + entry.rec_len as usize > block_size {
                    break;
                }

                if entry.inode != 0 {
                    // Check for duplicate name
                    let ename_len = entry.name_len as usize;
                    if ename_len == name_bytes.len() && ename_len <= entry.rec_len as usize - 8 {
                        let ename = &block_buf[pos + 8..pos + 8 + ename_len];
                        if ename == name_bytes {
                            return Err(FsError::AlreadyExists);
                        }
                    }

                    // Check for slack space after this (active) entry
                    let actual = entry_size(entry.name_len as usize);
                    let slack = entry.rec_len as usize - actual;
                    if slack >= needed {
                        // Split: shrink current entry, insert new entry in slack
                        let old_rec_len = entry.rec_len;

                        // Rewrite current entry with trimmed rec_len
                        block_buf[pos + 4] = actual as u8;
                        block_buf[pos + 5] = (actual >> 8) as u8;

                        // Write new entry at pos + actual
                        let new_pos = pos + actual;
                        let new_rec_len = old_rec_len as usize - actual;
                        write_dir_entry(
                            &mut block_buf,
                            new_pos,
                            target_ino,
                            new_rec_len as u16,
                            name_bytes,
                            file_type,
                        );

                        self.write_block(block_num, &block_buf).await?;
                        return Ok(dir_inode);
                    }
                } else {
                    // Deleted entry (inode == 0) — reuse if large enough
                    if entry.rec_len as usize >= needed {
                        write_dir_entry(
                            &mut block_buf,
                            pos,
                            target_ino,
                            entry.rec_len,
                            name_bytes,
                            file_type,
                        );
                        self.write_block(block_num, &block_buf).await?;
                        return Ok(dir_inode);
                    }
                }

                pos += entry.rec_len as usize;
            }
        }

        // No space found in existing blocks — allocate a new one
        let new_block = self.alloc_block().await?;
        let mut new_buf = vec![0u8; block_size];

        // Single entry spanning the whole block
        write_dir_entry(
            &mut new_buf,
            0,
            target_ino,
            block_size as u16,
            name_bytes,
            file_type,
        );

        self.write_block(new_block, &new_buf).await?;

        // Set the new block in the directory inode
        let file_block = num_blocks;
        let meta_blocks = self
            .set_block_number(&mut dir_inode, file_block, new_block)
            .await?;
        dir_inode.blocks += (1 + meta_blocks) * (self.block_size() / 512);
        dir_inode.set_size(dir_inode.size() + self.block_size() as u64);

        Ok(dir_inode)
    }

    /// Remove a directory entry by name from the directory identified by `dir_ino`.
    ///
    /// Finds the entry with the given `name`, removes it by either merging
    /// with the previous entry or zeroing the inode field, and returns the
    /// removed inode number along with the updated directory inode.
    ///
    /// # Errors
    ///
    /// - `NotFound` if no entry with the given name exists.
    /// - `IoError` on disk I/O failure.
    pub async fn remove_dir_entry(
        &self,
        dir_ino: u32,
        dir_inode: Inode,
        name: &str,
    ) -> Result<(u32, Inode), FsError> {
        let name_bytes = name.as_bytes();
        let block_size = self.block_size() as usize;
        let dir_size = dir_inode.size();
        let num_blocks =
            ((dir_size + self.block_size() as u64 - 1) / self.block_size() as u64) as u32;

        let mut block_buf = vec![0u8; block_size];

        for file_block in 0..num_blocks {
            let block_num = self.get_block(&dir_inode, file_block).await?;
            if block_num == 0 {
                continue;
            }

            self.read_block(block_num, &mut block_buf).await?;

            let mut pos = 0usize;
            let mut prev_pos: Option<usize> = None;

            while pos < block_size {
                if pos + DIR_ENTRY_HEADER_SIZE > block_size {
                    break;
                }

                let entry: DirEntryRaw =
                    unsafe { core::ptr::read(block_buf[pos..].as_ptr() as *const _) };

                if entry.rec_len < 8 || pos + entry.rec_len as usize > block_size {
                    break;
                }

                if entry.inode != 0 {
                    let ename_len = entry.name_len as usize;
                    if ename_len == name_bytes.len() && ename_len <= entry.rec_len as usize - 8 {
                        let ename = &block_buf[pos + 8..pos + 8 + ename_len];
                        if ename == name_bytes {
                            let removed_ino = entry.inode;

                            if let Some(pp) = prev_pos {
                                // Merge with previous entry: extend prev's rec_len
                                let prev_entry: DirEntryRaw = unsafe {
                                    core::ptr::read(block_buf[pp..].as_ptr() as *const _)
                                };
                                let merged_len =
                                    prev_entry.rec_len as usize + entry.rec_len as usize;
                                block_buf[pp + 4] = merged_len as u8;
                                block_buf[pp + 5] = (merged_len >> 8) as u8;
                            } else {
                                // First entry in block — zero the inode field
                                block_buf[pos] = 0;
                                block_buf[pos + 1] = 0;
                                block_buf[pos + 2] = 0;
                                block_buf[pos + 3] = 0;
                            }

                            self.write_block(block_num, &block_buf).await?;
                            return Ok((removed_ino, dir_inode));
                        }
                    }
                }

                prev_pos = Some(pos);
                pos += entry.rec_len as usize;
            }
        }

        Err(FsError::NotFound)
    }

    /// Free all data blocks and indirect index blocks owned by an inode.
    ///
    /// Walks the direct block pointers (0–11), single indirect (12),
    /// double indirect (13), and triple indirect (14) block pointers,
    /// freeing every allocated block. This is used by `unlink` when the
    /// link count reaches zero.
    pub async fn free_inode_blocks(&self, inode: &Inode) -> Result<(), FsError> {
        let ptrs_per_block = self.block_size() / 4;
        let block_size = self.block_size() as usize;

        // Free direct blocks (0–11)
        for i in 0..12 {
            if inode.block[i] != 0 {
                self.free_block(inode.block[i]).await?;
            }
        }

        // Free single indirect block (12)
        if inode.block[12] != 0 {
            self.free_indirect_block(inode.block[12], block_size, ptrs_per_block, 1)
                .await?;
        }

        // Free double indirect block (13)
        if inode.block[13] != 0 {
            self.free_indirect_block(inode.block[13], block_size, ptrs_per_block, 2)
                .await?;
        }

        // Free triple indirect block (14)
        if inode.block[14] != 0 {
            self.free_indirect_block(inode.block[14], block_size, ptrs_per_block, 3)
                .await?;
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
    ) -> core::pin::Pin<
        alloc::boxed::Box<dyn core::future::Future<Output = Result<(), FsError>> + Send + 'a>,
    > {
        alloc::boxed::Box::pin(async move {
            let mut buf = vec![0u8; block_size];
            self.read_block(block_num, &mut buf).await?;

            for i in 0..ptrs_per_block as usize {
                let offset = i * 4;
                let ptr = u32::from_le_bytes([
                    buf[offset],
                    buf[offset + 1],
                    buf[offset + 2],
                    buf[offset + 3],
                ]);
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
}

/// Write a directory entry into a block buffer at the given position.
fn write_dir_entry(
    buf: &mut [u8],
    pos: usize,
    inode: u32,
    rec_len: u16,
    name: &[u8],
    file_type: u8,
) {
    // inode (u32 LE)
    buf[pos..pos + 4].copy_from_slice(&inode.to_le_bytes());
    // rec_len (u16 LE)
    buf[pos + 4..pos + 6].copy_from_slice(&rec_len.to_le_bytes());
    // name_len (u8)
    buf[pos + 6] = name.len() as u8;
    // file_type (u8)
    buf[pos + 7] = file_type;
    // name bytes
    buf[pos + 8..pos + 8 + name.len()].copy_from_slice(name);
    // Zero-fill the padding between name end and next 4-byte boundary
    let padded_end = pos + align4(DIR_ENTRY_HEADER_SIZE + name.len());
    let name_end = pos + 8 + name.len();
    if padded_end > name_end && padded_end <= pos + rec_len as usize {
        for b in &mut buf[name_end..padded_end] {
            *b = 0;
        }
    }
}
