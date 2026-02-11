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
//! let inode_guard = InodeGuard::new(fs.clone(), fs.alloc_inode().await?);
//! let block_guard = BlockGuard::new(fs.clone(), fs.alloc_block().await?);
//!
//! // Write operations use consume_with() to get the value and run a closure
//! let ino = inode_guard.consume_with(|ino| {
//!     // Use ino for writes, then return it to commit
//!     fs.write_inode(ino, &inode).await?;
//!     Ok(ino)
//! })?;
//!
//! // Or consume directly at the commit point (e.g., passing to add_dir_entry)
//! let (ino, updated) = fs.add_dir_entry(..., inode_guard, ...).await?;
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
    /// This is the only way to access the inode number. After calling
    /// this, the caller takes responsibility for the inode and must handle
    /// cleanup on any subsequent errors.
    pub fn consume(mut self) -> u32 {
        self.ino.take().expect("InodeGuard already consumed")
    }
}

impl Drop for InodeGuard {
    fn drop(&mut self) {
        if let Some(ino) = self.ino.take() {
            // Schedule async cleanup. We can't await in drop, so we spawn a task.
            // The free_inode operation is idempotent and safe to run asynchronously.
            let fs = self.fs.clone();
            crate::executor::spawn(async move {
                let _ = fs.free_inode(ino).await;
            });
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
    /// This is the only way to access the block number. After calling
    /// this, the caller takes responsibility for the block and must handle
    /// cleanup on any subsequent errors.
    pub fn consume(mut self) -> u32 {
        self.block.take().expect("BlockGuard already consumed")
    }
}

impl Drop for BlockGuard {
    fn drop(&mut self) {
        if let Some(block) = self.block.take() {
            // Schedule async cleanup. We can't await in drop, so we spawn a task.
            // The free_block operation is idempotent and safe to run asynchronously.
            let fs = self.fs.clone();
            crate::executor::spawn(async move {
                let _ = fs.free_block(block).await;
            });
        }
    }
}
