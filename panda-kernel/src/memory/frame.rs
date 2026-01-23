use alloc::sync::Arc;
use core::alloc::Layout;
use x86_64::{PhysAddr, VirtAddr, structures::paging::PhysFrame};

struct FrameInner {
    frame: PhysFrame,
    /// The virtual address where the frame was allocated.
    /// We must deallocate using this exact address, not one derived from physical address.
    virt_addr: VirtAddr,
    layout: Layout,
}

impl Drop for FrameInner {
    fn drop(&mut self) {
        let ptr = self.virt_addr.as_mut_ptr();
        unsafe {
            alloc::alloc::dealloc(ptr, self.layout);
        }
    }
}

/// RAII guard for physical frame(s).
///
/// Cloning increments the reference count. When the last clone is dropped,
/// the underlying frame is deallocated.
#[derive(Clone)]
pub struct Frame {
    inner: Arc<FrameInner>,
}

impl Frame {
    /// Create a new Frame guard.
    ///
    /// # Safety
    /// The frame must have been allocated with the given layout at the given virtual address.
    pub(crate) unsafe fn new(frame: PhysFrame, virt_addr: VirtAddr, layout: Layout) -> Self {
        Self {
            inner: Arc::new(FrameInner {
                frame,
                virt_addr,
                layout,
            }),
        }
    }

    /// Get the underlying PhysFrame.
    pub fn phys_frame(&self) -> PhysFrame {
        self.inner.frame
    }

    /// Get the physical start address.
    pub fn start_address(&self) -> PhysAddr {
        self.inner.frame.start_address()
    }

    /// Get the size of the frame in bytes.
    pub fn size(&self) -> u64 {
        self.inner.layout.size() as u64
    }

    /// Get the current reference count.
    pub fn ref_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
}
