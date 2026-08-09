//! RAII guards for ext2 resource allocation.
//!
//! These guards wrap allocated inodes and blocks, automatically freeing them
//! on drop if not explicitly consumed. This provides exception-safe cleanup
//! during multi-step operations like `mkdir` and `create`.
//!
//! # Design
//!
//! The guards only expose the guarded value through `consume()`, which takes
//! ownership of the guard and returns the value. This ensures that:
//!
//! 1. External code cannot access the value without committing to keeping it
//! 2. The consume happens at the "commit point" (e.g., when adding a dir entry)
//! 3. RAII cleanup runs if any operation fails before the commit
//!
//! # Usage
//!
//! ```ignore
//! // alloc_inode returns InodeGuard directly
//! let inode_guard = fs.alloc_inode()?;
//! let block_guard = BlockGuard::new(fs.clone(), fs.alloc_block()?);
//!
//! // For operations needing the value before commit, use unsafe peek()
//! let block_num = unsafe { block_guard.peek() };
//! fs.write_block(block_num, &data)?;
//!
//! // Consume at the commit point (e.g., passing to add_dir_entry)
//! let (ino, updated) = fs.add_dir_entry(..., inode_guard, ...)?;
//! block_guard.consume();  // Commit block after successful dir entry
//! ```

use alloc::sync::Arc;

use super::Ext2Fs;

/// RAII guard for an allocated inode.
///
/// When dropped without being consumed, schedules the inode to be freed.
/// Call `consume()` to take ownership of the inode number and prevent
/// automatic cleanup.
pub struct InodeGuard {
    fs: Arc<Ext2Fs>,
    ino: Option<u32>,
}

impl InodeGuard {
    /// Create a new guard for an allocated inode.
    pub fn new(fs: Arc<Ext2Fs>, ino: u32) -> Self {
        Self { fs, ino: Some(ino) }
    }

    /// Consume the guard, returning the inode number without freeing it.
    ///
    /// This is the only way to safely access the inode number. After calling
    /// this, the caller takes responsibility for the inode and must handle
    /// cleanup on any subsequent errors.
    pub fn consume(mut self) -> u32 {
        self.ino.take().expect("InodeGuard already consumed")
    }

    /// Access the inode number without consuming the guard.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that either:
    /// 1. The guard will be consumed (via `consume()`) before being dropped, or
    /// 2. The caller will manually handle deallocation if the guard is dropped
    ///
    /// This is intended for operations that need to write data using the inode
    /// number before the final "commit point" where the guard is consumed.
    /// The guard should be kept alive until the operation is committed.
    pub unsafe fn peek(&self) -> u32 {
        self.ino.expect("InodeGuard already consumed")
    }
}

impl Drop for InodeGuard {
    fn drop(&mut self) {
        if let Some(ino) = self.ino.take() {
            // The kernel version spawned an executor task here, because
            // `free_inode` was async and `drop` cannot await. Now that it is
            // an ordinary synchronous call (see the crate doc comment's "Why
            // synchronous?" section) the cleanup happens inline, which also
            // makes it ordered with respect to whatever the caller does
            // next instead of running at some arbitrary later point.
            let _ = self.fs.free_inode(ino);
        }
    }
}

/// RAII guard for an allocated block.
///
/// When dropped without being consumed, schedules the block to be freed.
/// Call `consume()` to take ownership of the block number and prevent
/// automatic cleanup.
pub struct BlockGuard {
    fs: Arc<Ext2Fs>,
    block: Option<u32>,
}

impl BlockGuard {
    /// Create a new guard for an allocated block.
    pub fn new(fs: Arc<Ext2Fs>, block: u32) -> Self {
        Self {
            fs,
            block: Some(block),
        }
    }

    /// Consume the guard, returning the block number without freeing it.
    ///
    /// This is the only way to safely access the block number. After calling
    /// this, the caller takes responsibility for the block and must handle
    /// cleanup on any subsequent errors.
    pub fn consume(mut self) -> u32 {
        self.block.take().expect("BlockGuard already consumed")
    }

    /// Access the block number without consuming the guard.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that either:
    /// 1. The guard will be consumed (via `consume()`) before being dropped, or
    /// 2. The caller will manually handle deallocation if the guard is dropped
    ///
    /// This is intended for operations that need to write data using the block
    /// number before the final "commit point" where the guard is consumed.
    /// The guard should be kept alive until the operation is committed.
    pub unsafe fn peek(&self) -> u32 {
        self.block.expect("BlockGuard already consumed")
    }
}

impl Drop for BlockGuard {
    fn drop(&mut self) {
        if let Some(block) = self.block.take() {
            // Synchronous inline cleanup — see `InodeGuard`'s `Drop`.
            let _ = self.fs.free_block(block);
        }
    }
}
