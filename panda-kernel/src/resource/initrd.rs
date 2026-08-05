//! Read-only `initrd:` scheme, backed by the in-memory ustar archive kept
//! after boot.
//!
//! Unlike the `file:` scheme (which walks the general VFS mount table),
//! `InitrdScheme` talks directly to the [`vfs::TarFs`] built from the initrd
//! archive at boot. This lets the service manager read driver binaries from
//! the initrd (`initrd:/drivers/...`) before any filesystem is mounted —
//! resolving the chicken-and-egg problem where the block driver that would
//! mount the root filesystem is itself a driver binary that must be read
//! from somewhere first. No extraction to disk is involved: `TarFs` reads
//! directly out of the archive bytes kept in memory.

use alloc::boxed::Box;
use alloc::vec::Vec;
use alloc::sync::Arc;
use async_trait::async_trait;

use crate::resource::Resource;
use crate::resource::directory::DirEntry;
use crate::resource::scheme::{DirectoryResource, OpenError, SchemeHandler, VfsFileResource};
use crate::vfs::{Filesystem, TarFs};

/// Scheme handler for `initrd:` — reads directly from the in-memory ustar
/// archive, independent of the general VFS mount table.
pub struct InitrdScheme {
    fs: Arc<TarFs>,
}

impl InitrdScheme {
    /// Wrap an already-parsed initrd archive for scheme access.
    pub fn new(fs: Arc<TarFs>) -> Self {
        Self { fs }
    }
}

#[async_trait]
impl SchemeHandler for InitrdScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        let path = path.trim_start_matches('/');

        // Try as a directory first (mirrors `FileScheme::open`'s approach):
        // both `TarFs::readdir` and `TarFs::open` fail cleanly for a path
        // that doesn't resolve the other way, so one lookup determines which
        // resource kind to return.
        if let Ok(entries) = self.fs.readdir(path).await {
            return Ok(Box::new(DirectoryResource::new(entries)));
        }

        let file = self.fs.open(path).await.map_err(|_| OpenError::NotFound)?;
        Ok(Box::new(VfsFileResource::new(file)))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        self.fs.readdir(path.trim_start_matches('/')).await.ok()
    }
}
