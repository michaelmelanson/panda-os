//! RAII guards for ext2 resource allocation.
//!
//! These guards wrap allocated inodes and blocks, automatically freeing them
//! on drop if not explicitly consumed. This provides exception-safe cleanup
//! during multi-step operations like `mkdir` and `create`.
//!
//! # Usage
//!
//! ```ignore
//! let inode_guard = InodeGuard::new(fs.clone(), fs.alloc_inode().await?);
//! let block_guard = BlockGuard::new(fs.clone(), fs.alloc_block().await?);
//!
//! // Do work that might fail...
//!
//! // On success, consume the guards to prevent freeing:
//! let ino = inode_guard.consume();
//! let block = block_guard.consume();
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

    /// Get the inode number.
    pub fn ino(&self) -> u32 {
        self.ino.expect("InodeGuard already consumed")
    }

    /// Consume the guard, returning the inode number without freeing it.
    ///
    /// After calling this, the caller takes responsibility for the inode.
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

    /// Get the block number.
    pub fn block(&self) -> u32 {
        self.block.expect("BlockGuard already consumed")
    }

    /// Consume the guard, returning the block number without freeing it.
    ///
    /// After calling this, the caller takes responsibility for the block.
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
