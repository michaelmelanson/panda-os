use uefi::mem::memory_map::MemoryMapOwned;

pub mod global_alloc;
pub mod heap_allocator;

pub unsafe fn init_from_uefi(memory_map: &MemoryMapOwned) {
    let (heap_phys_base, heap_size) = heap_allocator::init_from_uefi(memory_map);
    unsafe {
        global_alloc::init(heap_phys_base, heap_size);
    }
}
