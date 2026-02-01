//! Ext2 bitmap-based block and inode allocation.
//!
//! This module provides first-fit allocation and deallocation for blocks and
//! inodes using the ext2 bitmap structures. Each block group has a block bitmap
//! and an inode bitmap; free counts are tracked in both the block group
//! descriptor and the superblock.
//!
//! ## Algorithm
//!
//! Allocation uses a sequential scan (first-fit): we iterate over block groups
//! in order, skip groups with zero free entries, then scan the bitmap byte by
//! byte and bit by bit to find the first clear (free) bit. This is simple and
//! correct; more sophisticated strategies (e.g., Orlov allocation) can be
//! added later.
//!
//! ## Locking
//!
//! Each alloc/free operation acquires the `alloc_lock` async mutex for its
//! entire duration, serialising all bitmap read-modify-write cycles. This
//! prevents TOCTOU races where two concurrent tasks could read the same
//! bitmap state and allocate the same block or inode. The `Ext2FsMutable`
//! `RwSpinlock` is still used for in-memory counter access, but the async
//! mutex ensures that the full bitmap I/O + counter update sequence is atomic.

use alloc::vec;

use super::Ext2Fs;
use crate::vfs::FsError;

impl Ext2Fs {
    /// Allocate a free block from the filesystem.
    ///
    /// Scans block groups sequentially and uses first-fit within each bitmap.
    /// Updates the bitmap on disk, and decrements the free block counts in
    /// both the block group descriptor and the superblock, writing both back.
    ///
    /// Returns the allocated block number, or `NoSpace` if the filesystem is full.
    pub async fn alloc_block(&self) -> Result<u32, FsError> {
        let _guard = self.alloc_lock.lock().await;

        let block_size = self.block_size() as usize;
        let (num_groups, first_data_block) = {
            let m = self.mutable().read();
            (m.block_groups.len(), m.superblock.first_data_block)
        };

        for group in 0..num_groups {
            // Check if this group has free blocks
            let (free_count, bitmap_block, blocks_in_group, blocks_per_group) = {
                let m = self.mutable().read();
                let bgd = &m.block_groups[group];
                if bgd.free_blocks_count == 0 {
                    continue;
                }
                let blocks_in_group = if group == num_groups - 1 {
                    // Last group may be smaller
                    let total = self.blocks_count();
                    let bpg = m.superblock.blocks_per_group;
                    let remaining = total - (group as u32 * bpg);
                    remaining
                } else {
                    m.superblock.blocks_per_group
                };
                (bgd.free_blocks_count, bgd.block_bitmap, blocks_in_group, m.superblock.blocks_per_group)
            };

            if free_count == 0 {
                continue;
            }

            // Read the block bitmap
            let mut bitmap = vec![0u8; block_size];
            self.read_block(bitmap_block, &mut bitmap).await?;

            // Scan for first free bit
            let bits_to_scan = blocks_in_group as usize;
            if let Some(bit_index) = find_first_clear_bit(&bitmap, bits_to_scan) {
                // Set the bit
                set_bit(&mut bitmap, bit_index);

                // Write bitmap back
                self.write_block(bitmap_block, &bitmap).await?;

                // Update free counts under the write lock
                {
                    let mut m = self.mutable().write();
                    m.block_groups[group].free_blocks_count -= 1;
                    m.superblock.free_blocks_count -= 1;
                }

                // Write updated metadata to disk
                self.write_block_group_descriptor(group as u32).await?;
                self.write_superblock().await?;

                // Calculate the absolute block number, accounting for first_data_block offset
                let block_num = first_data_block + group as u32 * blocks_per_group + bit_index as u32;
                return Ok(block_num);
            }
        }

        Err(FsError::NoSpace)
    }

    /// Free a previously allocated block.
    ///
    /// Clears the bit in the block bitmap, increments free block counts in
    /// the block group descriptor and superblock, and writes all back to disk.
    pub async fn free_block(&self, block_num: u32) -> Result<(), FsError> {
        let _guard = self.alloc_lock.lock().await;

        if block_num >= self.blocks_count() {
            log::warn!("ext2: free_block {} out of range", block_num);
            return Err(FsError::IoError);
        }

        let (blocks_per_group, first_data_block) = {
            let m = self.mutable().read();
            (m.superblock.blocks_per_group, m.superblock.first_data_block)
        };

        // Subtract first_data_block to get the group-relative position
        let adjusted = block_num - first_data_block;
        let group = (adjusted / blocks_per_group) as usize;
        let bit_index = (adjusted % blocks_per_group) as usize;

        let block_size = self.block_size() as usize;
        let bitmap_block = {
            let m = self.mutable().read();
            if group >= m.block_groups.len() {
                return Err(FsError::IoError);
            }
            m.block_groups[group].block_bitmap
        };

        // Read bitmap, clear bit, write back
        let mut bitmap = vec![0u8; block_size];
        self.read_block(bitmap_block, &mut bitmap).await?;

        if !get_bit(&bitmap, bit_index) {
            log::warn!("ext2: free_block {} was already free", block_num);
            return Err(FsError::IoError);
        }

        clear_bit(&mut bitmap, bit_index);
        self.write_block(bitmap_block, &bitmap).await?;

        // Update free counts
        {
            let mut m = self.mutable().write();
            m.block_groups[group].free_blocks_count += 1;
            m.superblock.free_blocks_count += 1;
        }

        self.write_block_group_descriptor(group as u32).await?;
        self.write_superblock().await?;

        Ok(())
    }

    /// Allocate a free inode from the filesystem.
    ///
    /// Scans block groups sequentially and uses first-fit within each inode
    /// bitmap. Updates the bitmap on disk, and decrements the free inode
    /// counts in both the block group descriptor and the superblock.
    ///
    /// Returns the allocated inode number (1-indexed), or `NoSpace` if no
    /// inodes are available.
    pub async fn alloc_inode(&self) -> Result<u32, FsError> {
        let _guard = self.alloc_lock.lock().await;

        let block_size = self.block_size() as usize;
        let num_groups = {
            let m = self.mutable().read();
            m.block_groups.len()
        };
        let inodes_per_group = self.inodes_per_group();

        for group in 0..num_groups {
            let (free_count, bitmap_block) = {
                let m = self.mutable().read();
                let bgd = &m.block_groups[group];
                if bgd.free_inodes_count == 0 {
                    continue;
                }
                (bgd.free_inodes_count, bgd.inode_bitmap)
            };

            if free_count == 0 {
                continue;
            }

            // Read the inode bitmap
            let mut bitmap = vec![0u8; block_size];
            self.read_block(bitmap_block, &mut bitmap).await?;

            // Scan for first free bit
            let bits_to_scan = inodes_per_group as usize;
            if let Some(bit_index) = find_first_clear_bit(&bitmap, bits_to_scan) {
                // Set the bit
                set_bit(&mut bitmap, bit_index);

                // Write bitmap back
                self.write_block(bitmap_block, &bitmap).await?;

                // Update free counts
                {
                    let mut m = self.mutable().write();
                    m.block_groups[group].free_inodes_count -= 1;
                    m.superblock.free_inodes_count -= 1;
                }

                self.write_block_group_descriptor(group as u32).await?;
                self.write_superblock().await?;

                // Inode numbers are 1-indexed
                let ino = group as u32 * inodes_per_group + bit_index as u32 + 1;
                return Ok(ino);
            }
        }

        Err(FsError::NoSpace)
    }

    /// Free a previously allocated inode.
    ///
    /// Clears the bit in the inode bitmap, increments free inode counts in
    /// the block group descriptor and superblock, and writes all back to disk.
    pub async fn free_inode(&self, ino: u32) -> Result<(), FsError> {
        let _guard = self.alloc_lock.lock().await;

        if ino == 0 {
            return Err(FsError::IoError);
        }

        let inodes_per_group = self.inodes_per_group();
        let group = ((ino - 1) / inodes_per_group) as usize;
        let bit_index = ((ino - 1) % inodes_per_group) as usize;

        let block_size = self.block_size() as usize;
        let bitmap_block = {
            let m = self.mutable().read();
            if group >= m.block_groups.len() {
                return Err(FsError::IoError);
            }
            m.block_groups[group].inode_bitmap
        };

        // Read bitmap, clear bit, write back
        let mut bitmap = vec![0u8; block_size];
        self.read_block(bitmap_block, &mut bitmap).await?;

        if !get_bit(&bitmap, bit_index) {
            log::warn!("ext2: free_inode {} was already free", ino);
            return Err(FsError::IoError);
        }

        clear_bit(&mut bitmap, bit_index);
        self.write_block(bitmap_block, &bitmap).await?;

        // Update free counts
        {
            let mut m = self.mutable().write();
            m.block_groups[group].free_inodes_count += 1;
            m.superblock.free_inodes_count += 1;
        }

        self.write_block_group_descriptor(group as u32).await?;
        self.write_superblock().await?;

        Ok(())
    }
}

// =============================================================================
// Bitmap bit manipulation helpers
// =============================================================================

/// Find the first clear (0) bit in a bitmap, scanning up to `max_bits` bits.
///
/// Returns `Some(index)` of the first clear bit, or `None` if all are set.
fn find_first_clear_bit(bitmap: &[u8], max_bits: usize) -> Option<usize> {
    for (byte_idx, &byte) in bitmap.iter().enumerate() {
        if byte == 0xFF {
            // All bits set, skip this byte
            continue;
        }
        // Check each bit in this byte
        for bit in 0..8u32 {
            let index = byte_idx * 8 + bit as usize;
            if index >= max_bits {
                return None;
            }
            if byte & (1 << bit) == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// Test whether a bit is set in a bitmap.
fn get_bit(bitmap: &[u8], index: usize) -> bool {
    let byte = bitmap[index / 8];
    byte & (1 << (index % 8)) != 0
}

/// Set a bit in a bitmap.
fn set_bit(bitmap: &mut [u8], index: usize) {
    bitmap[index / 8] |= 1 << (index % 8);
}

/// Clear a bit in a bitmap.
fn clear_bit(bitmap: &mut [u8], index: usize) {
    bitmap[index / 8] &= !(1 << (index % 8));
}

