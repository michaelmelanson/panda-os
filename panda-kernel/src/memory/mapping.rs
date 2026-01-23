use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::VirtAddr;

use super::{Frame, free_region, physical_address_to_virtual, unmap_region};

/// What backs the mapped memory region.
pub enum MappingBacking {
    /// Allocated frames - deallocated when refcount hits zero.
    Frames(Vec<Frame>),
    /// Device memory (MMIO) - not deallocated.
    Mmio,
    /// Demand-paged region - frames allocated on page fault, freed by walking page tables.
    /// Used for heap regions that can grow/shrink dynamically.
    DemandPaged,
}

struct MappingInner {
    base_virt: VirtAddr,
    /// Current size in bytes. For DemandPaged mappings, this can change via resize().
    size_bytes: AtomicU64,
    backing: MappingBacking,
}

impl Drop for MappingInner {
    fn drop(&mut self) {
        let size = self.size_bytes.load(Ordering::Acquire) as usize;
        match &self.backing {
            MappingBacking::Frames(_) | MappingBacking::Mmio => {
                // Unmap the virtual address region
                unmap_region(self.base_virt, size);
                // Backing frames are dropped automatically by Vec's Drop
            }
            MappingBacking::DemandPaged => {
                // Walk page tables to find and free any mapped pages
                free_region(self.base_virt, size);
            }
        }
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
    pub fn new(base_virt: VirtAddr, size_bytes: usize, backing: MappingBacking) -> Self {
        Self {
            inner: Arc::new(MappingInner {
                base_virt,
                size_bytes: AtomicU64::new(size_bytes as u64),
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
        self.inner.size_bytes.load(Ordering::Acquire) as usize
    }

    /// Get the current reference count.
    pub fn ref_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    /// Resize a demand-paged mapping.
    /// When shrinking, walks page tables to free pages above the new size.
    /// Only valid for DemandPaged mappings.
    ///
    /// Returns the new size on success, or the current size if resize failed.
    pub fn resize(&self, new_size: usize) -> usize {
        match &self.inner.backing {
            MappingBacking::DemandPaged => {
                let old_size = self.inner.size_bytes.load(Ordering::Acquire) as usize;

                if new_size < old_size {
                    // Shrinking: free pages in the region being released
                    let free_from = self.inner.base_virt + new_size as u64;
                    let free_size = old_size - new_size;
                    free_region(free_from, free_size);
                }

                self.inner
                    .size_bytes
                    .store(new_size as u64, Ordering::Release);
                new_size
            }
            _ => {
                // Cannot resize pre-allocated or MMIO mappings
                self.size()
            }
        }
    }

    /// Write data to the mapping at a given offset via the physical window.
    ///
    /// This allows the kernel to write to userspace mappings without needing
    /// direct access to userspace virtual addresses. Only works for mappings
    /// backed by frames (not MMIO or demand-paged).
    ///
    /// # Safety
    /// The caller must ensure the offset and data length don't exceed the mapping size.
    pub unsafe fn write_at(&self, offset: usize, data: &[u8]) {
        let MappingBacking::Frames(frames) = &self.inner.backing else {
            panic!("write_at only supported for frame-backed mappings");
        };

        let mut remaining = data;
        let mut current_offset = offset;

        while !remaining.is_empty() {
            let frame_idx = current_offset / 4096;
            let offset_in_frame = current_offset % 4096;
            let bytes_in_frame = (4096 - offset_in_frame).min(remaining.len());

            let frame = &frames[frame_idx];
            let phys_virt = physical_address_to_virtual(frame.start_address());
            let dst = unsafe { phys_virt.as_mut_ptr::<u8>().add(offset_in_frame) };

            unsafe {
                core::ptr::copy_nonoverlapping(remaining.as_ptr(), dst, bytes_in_frame);
            }

            remaining = &remaining[bytes_in_frame..];
            current_offset += bytes_in_frame;
        }
    }
}
