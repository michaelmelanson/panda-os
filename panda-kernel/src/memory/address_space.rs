//! Kernel address space layout and initialization.
//!
//! This module defines the virtual address space layout and contains the logic
//! for transitioning from identity-mapped kernel to higher-half kernel execution.
//! It is only used during early boot and is isolated from runtime memory management.
//!
//! The transition involves:
//! 1. Creating a physical memory window at 0xffff_8000_0000_0000
//! 2. Creating an MMIO region at 0xffff_9000_0000_0000
//! 3. Relocating the kernel using PE base relocations
//! 4. Jumping to higher-half execution
//! 5. Removing identity mapping
//!
//! See docs/HIGHER_HALF_KERNEL.md for the full plan.

use log::info;
use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned};
use x86_64::structures::paging::page_table::PageTableLevel;
use x86_64::structures::paging::{PageTable, PageTableFlags};
use x86_64::{PhysAddr, VirtAddr};

use super::paging::{current_page_table, without_write_protection};
use super::{allocate_frame_raw, set_phys_map_base};

/// Base address of the physical memory window.
/// All physical RAM is mapped starting at this address.
pub const PHYS_WINDOW_BASE: u64 = 0xffff_8000_0000_0000;

/// Base address of the MMIO region.
/// Device memory-mapped I/O is allocated starting at this address.
pub const MMIO_REGION_BASE: u64 = 0xffff_9000_0000_0000;

/// Base address of the kernel heap region.
pub const KERNEL_HEAP_BASE: u64 = 0xffff_a000_0000_0000;

/// Base address for the relocated kernel image.
pub const KERNEL_IMAGE_BASE: u64 = 0xffff_c000_0000_0000;

/// Size of a 1GB huge page.
const SIZE_1GB: u64 = 1024 * 1024 * 1024;

/// Size of a 2MB huge page.
const SIZE_2MB: u64 = 2 * 1024 * 1024;

/// Create the physical memory window mapping all physical RAM to higher-half addresses.
///
/// This maps all usable physical memory to virtual addresses starting at `PHYS_WINDOW_BASE`.
/// Uses 1GB huge pages where possible for efficiency, falling back to 2MB pages.
///
/// After this function returns, physical memory can be accessed via:
/// `virtual_address = PHYS_WINDOW_BASE + physical_address`
///
/// # Safety
/// Must be called exactly once during early kernel initialization, before
/// `set_phys_map_base()` is called.
pub unsafe fn create_physical_memory_window(memory_map: &MemoryMapOwned) {
    // Find the highest physical address we need to map
    let max_phys_addr = memory_map
        .entries()
        .map(|entry| entry.phys_start + entry.page_count * 4096)
        .max()
        .unwrap_or(0);

    info!(
        "Creating physical memory window: mapping {:.2} GB of physical memory to {:#x}",
        max_phys_addr as f64 / SIZE_1GB as f64,
        PHYS_WINDOW_BASE
    );

    let pml4 = unsafe { &mut *current_page_table() };

    // Map physical memory in 1GB chunks using huge pages at PML3 level
    let mut phys_addr = 0u64;
    while phys_addr < max_phys_addr {
        let virt_addr = VirtAddr::new(PHYS_WINDOW_BASE + phys_addr);

        // Get PML4 index (bits 39-47 of virtual address)
        let pml4_index = virt_addr.page_table_index(PageTableLevel::Four);

        // Get or create PML3 (PDPT) entry
        let pml4_entry = &mut pml4[pml4_index];
        let pml3 = if pml4_entry.flags().contains(PageTableFlags::PRESENT) {
            unsafe { &mut *(pml4_entry.addr().as_u64() as *mut PageTable) }
        } else {
            let frame = allocate_frame_raw();
            let table = unsafe { &mut *(frame.start_address().as_u64() as *mut PageTable) };
            // Zero the new table
            unsafe { core::ptr::write_bytes(table, 0, 1) };
            without_write_protection(|| {
                pml4_entry.set_addr(
                    frame.start_address(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                );
            });
            table
        };

        // Get PML3 index (bits 30-38 of virtual address)
        let pml3_index = virt_addr.page_table_index(PageTableLevel::Three);
        let pml3_entry = &mut pml3[pml3_index];

        // Check if we can use a 1GB huge page (requires alignment)
        if phys_addr % SIZE_1GB == 0 && phys_addr + SIZE_1GB <= max_phys_addr {
            // Map 1GB huge page at PML3 level
            without_write_protection(|| {
                pml3_entry.set_addr(
                    PhysAddr::new(phys_addr),
                    PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::HUGE_PAGE
                        | PageTableFlags::NO_EXECUTE,
                );
            });
            phys_addr += SIZE_1GB;
        } else {
            // Fall back to 2MB pages via PML2
            let pml2 = if pml3_entry.flags().contains(PageTableFlags::PRESENT)
                && !pml3_entry.flags().contains(PageTableFlags::HUGE_PAGE)
            {
                unsafe { &mut *(pml3_entry.addr().as_u64() as *mut PageTable) }
            } else {
                let frame = allocate_frame_raw();
                let table = unsafe { &mut *(frame.start_address().as_u64() as *mut PageTable) };
                unsafe { core::ptr::write_bytes(table, 0, 1) };
                without_write_protection(|| {
                    pml3_entry.set_addr(
                        frame.start_address(),
                        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                    );
                });
                table
            };

            // Map 2MB pages until we reach the next 1GB boundary or max_phys_addr
            let end_of_region = core::cmp::min(
                (phys_addr + SIZE_1GB) & !(SIZE_1GB - 1), // Next 1GB boundary
                max_phys_addr,
            );

            while phys_addr < end_of_region {
                let pml2_index = VirtAddr::new(PHYS_WINDOW_BASE + phys_addr)
                    .page_table_index(PageTableLevel::Two);
                let pml2_entry = &mut pml2[pml2_index];

                without_write_protection(|| {
                    pml2_entry.set_addr(
                        PhysAddr::new(phys_addr),
                        PageTableFlags::PRESENT
                            | PageTableFlags::WRITABLE
                            | PageTableFlags::HUGE_PAGE
                            | PageTableFlags::NO_EXECUTE,
                    );
                });
                phys_addr += SIZE_2MB;
            }
        }
    }

    // Flush TLB - we've modified page tables
    x86_64::instructions::tlb::flush_all();

    info!("Physical memory window created successfully");
}

/// Initialize the higher-half address space.
///
/// This creates the physical memory window and sets up `PHYS_MAP_BASE` so that
/// `physical_address_to_virtual()` returns higher-half addresses.
///
/// # Safety
/// Must be called exactly once during early kernel initialization.
pub unsafe fn init(memory_map: &MemoryMapOwned) {
    unsafe {
        create_physical_memory_window(memory_map);
    }
    set_phys_map_base(PHYS_WINDOW_BASE);
    info!(
        "Higher-half address space initialized, PHYS_MAP_BASE = {:#x}",
        PHYS_WINDOW_BASE
    );
}

// Phase 3: MMIO region allocator will be implemented here.
// Phase 4: PE relocation logic will be implemented here.
// Phase 5: Jump to higher-half will be implemented here.
// Phase 6: Identity mapping removal will be implemented here.
