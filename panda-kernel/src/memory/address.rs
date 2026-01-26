//! Virtual and physical address utilities.

use core::sync::atomic::{AtomicU64, Ordering};

use log::info;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{PageTableFlags, page_table::PageTableLevel},
};

use super::address_space::KERNEL_HEAP_BASE;
use super::recursive;

/// Physical base address of the kernel heap region.
/// Used to translate heap virtual addresses back to physical and vice versa.
static HEAP_PHYS_BASE: AtomicU64 = AtomicU64::new(0);

/// Convert a virtual address back to physical by walking the page tables.
///
/// This works for any mapped address regardless of which region it's in.
/// Uses recursive page table mapping to access page tables.
pub fn virtual_address_to_physical(virt: VirtAddr) -> PhysAddr {
    // Walk PML4 via recursive mapping
    let pml4 = unsafe { recursive::table_for_addr(virt, PageTableLevel::Four) };
    let pml4_idx = virt.page_table_index(PageTableLevel::Four);
    let pml4_entry = &pml4[pml4_idx];
    if !pml4_entry.flags().contains(PageTableFlags::PRESENT) {
        panic!(
            "virtual_address_to_physical: PML4 entry not present for {:#x}",
            virt.as_u64()
        );
    }

    // Walk PML3 (PDPT) via recursive mapping
    let pml3 = unsafe { recursive::table_for_addr(virt, PageTableLevel::Three) };
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

    // Walk PML2 (PD) via recursive mapping
    let pml2 = unsafe { recursive::table_for_addr(virt, PageTableLevel::Two) };
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

    // Walk PML1 (PT) via recursive mapping
    let pml1 = unsafe { recursive::table_for_addr(virt, PageTableLevel::One) };
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

    info!("Inspecting virtual address {virt_addr:?}");
    loop {
        let page_table = unsafe { recursive::table_for_addr(virt_addr, level) };
        let index = virt_addr.page_table_index(level);
        let entry = &page_table[index];
        info!(" - Level {level:?}, index {index:?}: {entry:?}");

        if entry.addr() == PhysAddr::zero() {
            break;
        }

        let Some(next_level) = level.next_lower_level() else {
            break;
        };
        level = next_level
    }
}
