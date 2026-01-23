use alloc::sync::Arc;
use core::alloc::Layout;
use x86_64::{PhysAddr, structures::paging::PhysFrame};

use super::physical_address_to_virtual;

struct FrameInner {
    frame: PhysFrame,
    layout: Layout,
}

impl Drop for FrameInner {
    fn drop(&mut self) {
        let ptr = physical_address_to_virtual(self.frame.start_address()).as_mut_ptr();
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
    /// The frame must have been allocated with the given layout.
    pub(crate) unsafe fn new(frame: PhysFrame, layout: Layout) -> Self {
        Self {
            inner: Arc::new(FrameInner { frame, layout }),
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
