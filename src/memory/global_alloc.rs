use core::{alloc::GlobalAlloc, ptr};

use log::{error, trace};
use spinning_top::RwSpinlock;

struct BumpAllocatorInner {
    heap_start: usize,
    heap_end: usize,
    next: usize,
    allocations: usize,
}

impl BumpAllocatorInner {
    pub const fn new() -> Self {
        Self {
            heap_start: 0,
            heap_end: 0,
            next: 0,
            allocations: 0,
        }
    }

    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.heap_start = heap_start;
        self.heap_end = heap_start + heap_size;
        self.next = heap_start;
    }
}

struct BumpAllocator {
    inner: RwSpinlock<BumpAllocatorInner>,
}

impl BumpAllocator {
    pub const fn new() -> Self {
        Self {
            inner: RwSpinlock::new(BumpAllocatorInner::new()),
        }
    }

    pub unsafe fn init(&self, heap_start: usize, heap_size: usize) {
        unsafe {
            self.inner.write().init(heap_start, heap_size);
        }
    }
}

unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let mut allocator = self.inner.write();

        let alloc_start = allocator.next.next_multiple_of(layout.align());
        allocator.next = alloc_start + layout.size();

        if allocator.next > allocator.heap_end {
            error!("Out of memory");
            return ptr::null_mut();
        }

        trace!("Allocated {} bytes at {alloc_start:#010X}", layout.size());

        allocator.allocations += 1;
        alloc_start as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        trace!(
            "Skipping deallocation of {} bytes at {:#010x}",
            layout.size(),
            ptr as usize
        );
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: BumpAllocator = BumpAllocator::new();

pub unsafe fn init(heap_start: usize, heap_size: usize) {
    unsafe {
        GLOBAL_ALLOCATOR.init(heap_start, heap_size);
    }
}
