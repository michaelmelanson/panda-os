use linked_list_allocator::LockedHeap;
use log::debug;
use uefi::{
    boot::MemoryType,
    mem::memory_map::{MemoryMap, MemoryMapOwned},
};

static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init_from_uefi(memory_map: &MemoryMapOwned) -> (usize, usize) {
    let mut best_region_base = None;
    let mut best_region_page_count = 0usize;

    for entry in memory_map.entries() {
        if entry.ty != MemoryType::CONVENTIONAL {
            continue;
        }

        let phys_start = entry.phys_start as usize;
        let page_count = entry.page_count as usize;

        if page_count < best_region_page_count {
            continue;
        }

        best_region_base = Some(phys_start);
        best_region_page_count = page_count;
    }

    let best_region_base = best_region_base.unwrap();
    let best_region_size = best_region_page_count * 4096;
    debug!(
        "Selected heap: {} pages ({}MB) starting at {:#012X}",
        best_region_page_count,
        best_region_page_count * 4 / 1024,
        best_region_base
    );

    unsafe {
        ALLOCATOR
            .lock()
            .init(best_region_base as *mut u8, best_region_size);
    }

    (best_region_base, best_region_size)
}
