//! Virtual and physical address utilities.

use log::info;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{PageTable, page_table::PageTableLevel},
};

use super::paging::current_page_table;

/// Convert a physical address to a virtual address.
/// This works because we identity map physical addresses.
pub fn physical_address_to_virtual(addr: PhysAddr) -> VirtAddr {
    VirtAddr::new(addr.as_u64())
}

/// Debug utility to inspect page table entries for a virtual address.
pub fn inspect_virtual_address(virt_addr: VirtAddr) {
    let mut level = PageTableLevel::Four;
    let page_table = current_page_table();
    let mut page_table = unsafe { &*page_table };

    info!("Inspecting virtual address {virt_addr:?}");
    loop {
        let index = virt_addr.page_table_index(level);
        let entry = &page_table[index];
        info!(" - Level {level:?}, index {index:?}: {entry:?}");

        if entry.addr() == PhysAddr::zero() {
            break;
        }

        page_table = unsafe { &*(physical_address_to_virtual(entry.addr()).as_ptr::<PageTable>()) };

        let Some(next_level) = level.next_lower_level() else {
            break;
        };
        level = next_level
    }
}
