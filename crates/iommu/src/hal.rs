//! Hardware abstraction traits implemented by the kernel.
//!
//! These are the only seam between this crate and kernel-private memory
//! management. The kernel provides concrete implementations backed by its
//! own frame allocator and physical-memory mapping infrastructure.

/// Provides 4 KB physically-contiguous frames for IOMMU page table nodes.
///
/// # Safety
/// Implementors must guarantee that the returned frame is:
/// - 4 KB aligned in both physical and virtual address
/// - exclusively owned by the caller until `dealloc_frame` is called
/// - valid for reads and writes of exactly 4096 bytes through the virtual pointer
pub unsafe trait FrameAllocator: Send + Sync {
    /// Allocate one page frame.
    /// Returns `(physical_address, virtual_pointer)` or `None` on OOM.
    fn alloc_frame(&self) -> Option<(u64, *mut u8)>;

    /// Release a frame previously returned by `alloc_frame`.
    /// # Safety
    /// `phys` must be a value previously returned by this allocator
    /// and not yet freed.
    unsafe fn dealloc_frame(&self, phys: u64);
}

/// Provides volatile read/write access to a contiguous MMIO register region.
///
/// # Safety
/// Implementors must guarantee that reads and writes go directly to device
/// memory without caching, and that the region covers at least the offsets
/// the caller will access.
pub unsafe trait MmioAccess: Send + Sync {
    unsafe fn read_u32(&self, offset: usize) -> u32;
    unsafe fn write_u32(&self, offset: usize, val: u32);
    unsafe fn read_u64(&self, offset: usize) -> u64;
    unsafe fn write_u64(&self, offset: usize, val: u64);
}
