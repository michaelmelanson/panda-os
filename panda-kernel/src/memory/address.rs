//! Virtual and physical address utilities.

use core::sync::atomic::{AtomicU64, Ordering};

use log::info;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{PageTable, page_table::PageTableLevel},
};

use super::paging::current_page_table;

/// Base address of the physical memory window.
///
/// When this is 0, we use identity mapping (phys == virt).
/// When set to a higher-half address, physical memory is accessed via that window.
static PHYS_MAP_BASE: AtomicU64 = AtomicU64::new(0);

/// Convert a physical address to a virtual address using the physical memory window.
///
/// Initially uses identity mapping (when PHYS_MAP_BASE is 0).
/// After higher-half initialization, uses the physical memory window.
pub fn physical_address_to_virtual(phys: PhysAddr) -> VirtAddr {
    let base = PHYS_MAP_BASE.load(Ordering::Relaxed);
    VirtAddr::new(base + phys.as_u64())
}

/// Convert a virtual address (in the physical memory window) back to physical.
///
/// Initially uses identity mapping (when PHYS_MAP_BASE is 0).
/// After higher-half initialization, subtracts the window base.
pub fn virtual_address_to_physical(virt: VirtAddr) -> PhysAddr {
    let base = PHYS_MAP_BASE.load(Ordering::Relaxed);
    PhysAddr::new(virt.as_u64() - base)
}

/// Set the physical memory window base address.
///
/// This should only be called once during higher-half initialization.
pub fn set_phys_map_base(base: u64) {
    PHYS_MAP_BASE.store(base, Ordering::Release);
}

/// Get the current physical memory window base address.
pub fn get_phys_map_base() -> u64 {
    PHYS_MAP_BASE.load(Ordering::Acquire)
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
