use alloc::sync::Arc;
use alloc::vec::Vec;
use x86_64::VirtAddr;

use super::{unmap_region, Frame};

/// What backs the mapped memory region.
pub enum MappingBacking {
    /// Allocated frames - deallocated when refcount hits zero.
    Frames(Vec<Frame>),
    /// Device memory (MMIO) - not deallocated.
    Mmio,
}

struct MappingInner {
    base_virt: VirtAddr,
    size_bytes: usize,
    backing: MappingBacking,
}

impl Drop for MappingInner {
    fn drop(&mut self) {
        // Unmap the virtual address region
        unmap_region(self.base_virt, self.size_bytes);
        // Backing frames are dropped automatically by Vec's Drop
    }
}

/// RAII guard for a memory mapping.
///
/// Cloning increments the reference count. When the last clone is dropped,
/// the mapping is unmapped and backing frames are deallocated.
#[derive(Clone)]
pub struct Mapping {
    inner: Arc<MappingInner>,
}

impl Mapping {
    /// Create a new Mapping guard.
    pub(crate) fn new(base_virt: VirtAddr, size_bytes: usize, backing: MappingBacking) -> Self {
        Self {
            inner: Arc::new(MappingInner {
                base_virt,
                size_bytes,
                backing,
            }),
        }
    }

    /// Get the base virtual address of the mapping.
    pub fn base_virtual_address(&self) -> VirtAddr {
        self.inner.base_virt
    }

    /// Get the size of the mapping in bytes.
    pub fn size(&self) -> usize {
        self.inner.size_bytes
    }

    /// Get the current reference count.
    pub fn ref_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
}
