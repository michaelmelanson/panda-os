use core::alloc::Layout;

use linked_list_allocator::LockedHeap;
use x86_64::VirtAddr;

#[global_allocator]
static GLOBAL_ALLOCATOR: LockedHeap = LockedHeap::empty();

pub unsafe fn init(heap_start: usize, heap_size: usize) {
    unsafe {
        GLOBAL_ALLOCATOR.lock().init(heap_start as *mut u8, heap_size);
    }
}

pub fn allocate(layout: Layout) -> VirtAddr {
    use alloc::alloc::alloc_zeroed;
    let ptr = unsafe { alloc_zeroed(layout) };
    VirtAddr::new(ptr as u64)
}
