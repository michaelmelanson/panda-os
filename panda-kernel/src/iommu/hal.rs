//! Kernel implementations of the `iommu` crate's HAL traits.

use alloc::collections::BTreeMap;
use core::alloc::Layout;

use ::iommu::{FrameAllocator, MmioAccess};
use spinning_top::Spinlock;
use x86_64::PhysAddr;

use crate::memory::{Frame, PhysicalMapping, allocate_physical};

/// Implements [`FrameAllocator`] by allocating page-aligned physical frames
/// through the kernel's frame allocator, tracked by physical address so they
/// can be released again by `dealloc_frame`.
#[derive(Clone)]
pub struct KernelFrameAllocator {
    frames: alloc::sync::Arc<Spinlock<BTreeMap<u64, Frame>>>,
}

impl KernelFrameAllocator {
    pub fn new() -> Self {
        Self {
            frames: alloc::sync::Arc::new(Spinlock::new(BTreeMap::new())),
        }
    }
}

unsafe impl FrameAllocator for KernelFrameAllocator {
    fn alloc_frame(&self) -> Option<(u64, *mut u8)> {
        let layout = Layout::from_size_align(4096, 4096).ok()?;
        let frame = allocate_physical(layout);
        let phys = frame.start_address().as_u64();
        let ptr = frame.virtual_address().as_mut_ptr::<u8>();
        self.frames.lock().insert(phys, frame);
        Some((phys, ptr))
    }

    unsafe fn dealloc_frame(&self, phys: u64) {
        self.frames.lock().remove(&phys);
    }
}

/// Implements [`MmioAccess`] over a kernel [`PhysicalMapping`] of a VT-d
/// register bank.
pub struct KernelMmioAccess {
    mapping: PhysicalMapping,
}

impl KernelMmioAccess {
    /// Map `size` bytes of the VT-d register bank at `phys_base`.
    pub fn new(phys_base: u64, size: usize) -> Self {
        Self {
            mapping: PhysicalMapping::new(PhysAddr::new(phys_base), size),
        }
    }
}

unsafe impl MmioAccess for KernelMmioAccess {
    unsafe fn read_u32(&self, offset: usize) -> u32 {
        unsafe { self.mapping.read_unchecked::<u32>(offset) }
    }

    unsafe fn write_u32(&self, offset: usize, val: u32) {
        unsafe { self.mapping.write_unchecked::<u32>(offset, val) }
    }

    unsafe fn read_u64(&self, offset: usize) -> u64 {
        unsafe { self.mapping.read_unchecked::<u64>(offset) }
    }

    unsafe fn write_u64(&self, offset: usize, val: u64) {
        unsafe { self.mapping.write_unchecked::<u64>(offset, val) }
    }
}
