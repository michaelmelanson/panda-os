//! Virtual and physical address utilities.

use core::sync::atomic::{AtomicU64, Ordering};

use log::info;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{PageTable, PageTableFlags, page_table::PageTableLevel},
};

use super::address_space::KERNEL_HEAP_BASE;
use super::paging::current_page_table;

/// Base address of the physical memory window.
///
/// When this is 0, we use identity mapping (phys == virt).
/// When set to a higher-half address, physical memory is accessed via that window.
static PHYS_MAP_BASE: AtomicU64 = AtomicU64::new(0);

/// Physical base address of the kernel heap region.
/// Used to translate heap virtual addresses back to physical and vice versa.
static HEAP_PHYS_BASE: AtomicU64 = AtomicU64::new(0);

/// Convert a physical address to a virtual address using the physical memory window.
///
/// Initially uses identity mapping (when PHYS_MAP_BASE is 0).
/// After higher-half initialization, uses the physical memory window.
pub fn physical_address_to_virtual(phys: PhysAddr) -> VirtAddr {
    let base = PHYS_MAP_BASE.load(Ordering::Relaxed);
    VirtAddr::new(base + phys.as_u64())
}

/// Convert a virtual address back to physical by walking the page tables.
///
/// This works for any mapped address regardless of which region it's in.
pub fn virtual_address_to_physical(virt: VirtAddr) -> PhysAddr {
    let pml4 = unsafe { &*current_page_table() };

    // Walk PML4
    let pml4_idx = virt.page_table_index(PageTableLevel::Four);
    let pml4_entry = &pml4[pml4_idx];
    if !pml4_entry.flags().contains(PageTableFlags::PRESENT) {
        panic!(
            "virtual_address_to_physical: PML4 entry not present for {:#x}",
            virt.as_u64()
        );
    }

    // Walk PML3 (PDPT) - access via physical window
    let pml3_virt = physical_address_to_virtual(pml4_entry.addr());
    let pml3 = unsafe { &*(pml3_virt.as_ptr::<PageTable>()) };
    let pml3_idx = virt.page_table_index(PageTableLevel::Three);
    let pml3_entry = &pml3[pml3_idx];
    if !pml3_entry.flags().contains(PageTableFlags::PRESENT) {
        panic!(
            "virtual_address_to_physical: PML3 entry not present for {:#x}",
            virt.as_u64()
        );
    }
    // Check for 1GB huge page
    if pml3_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
        let base = pml3_entry.addr().as_u64() & !0x3FFF_FFFF; // Mask to 1GB alignment
        let offset = virt.as_u64() & 0x3FFF_FFFF; // Offset within 1GB page
        return PhysAddr::new(base + offset);
    }

    // Walk PML2 (PD) - access via physical window
    let pml2_virt = physical_address_to_virtual(pml3_entry.addr());
    let pml2 = unsafe { &*(pml2_virt.as_ptr::<PageTable>()) };
    let pml2_idx = virt.page_table_index(PageTableLevel::Two);
    let pml2_entry = &pml2[pml2_idx];
    if !pml2_entry.flags().contains(PageTableFlags::PRESENT) {
        panic!(
            "virtual_address_to_physical: PML2 entry not present for {:#x}",
            virt.as_u64()
        );
    }
    // Check for 2MB huge page
    if pml2_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
        let base = pml2_entry.addr().as_u64() & !0x1F_FFFF; // Mask to 2MB alignment
        let offset = virt.as_u64() & 0x1F_FFFF; // Offset within 2MB page
        return PhysAddr::new(base + offset);
    }

    // Walk PML1 (PT) - access via physical window
    let pml1_virt = physical_address_to_virtual(pml2_entry.addr());
    let pml1 = unsafe { &*(pml1_virt.as_ptr::<PageTable>()) };
    let pml1_idx = virt.page_table_index(PageTableLevel::One);
    let pml1_entry = &pml1[pml1_idx];
    if !pml1_entry.flags().contains(PageTableFlags::PRESENT) {
        panic!(
            "virtual_address_to_physical: PML1 entry not present for {:#x}",
            virt.as_u64()
        );
    }

    let base = pml1_entry.addr().as_u64();
    let offset = virt.as_u64() & 0xFFF; // Offset within 4KB page
    PhysAddr::new(base + offset)
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

/// Set the heap physical base address.
///
/// This should only be called once during memory initialization.
pub fn set_heap_phys_base(base: u64) {
    HEAP_PHYS_BASE.store(base, Ordering::Release);
}

/// Convert a physical address that's within the heap region back to its heap virtual address.
///
/// This is the inverse of virtual_address_to_physical() for heap addresses.
/// The heap is mapped at KERNEL_HEAP_BASE, so:
///   virt = KERNEL_HEAP_BASE + (phys - heap_phys_base)
pub fn heap_phys_to_virt(phys: PhysAddr) -> VirtAddr {
    let heap_phys_base = HEAP_PHYS_BASE.load(Ordering::Acquire);
    debug_assert!(
        phys.as_u64() >= heap_phys_base,
        "physical address {:#x} is below heap base {:#x}",
        phys.as_u64(),
        heap_phys_base
    );
    let offset = phys.as_u64() - heap_phys_base;
    VirtAddr::new(KERNEL_HEAP_BASE + offset)
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
