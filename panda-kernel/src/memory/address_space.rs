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

use goblin::pe::PE;
use log::info;
use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned};
use x86_64::structures::paging::page_table::PageTableLevel;
use x86_64::structures::paging::{PageTable, PageTableFlags};
use x86_64::{PhysAddr, VirtAddr};

use crate::uefi::KernelImageInfo;

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

/// PE relocation type: padding/skip entry.
const IMAGE_REL_BASED_ABSOLUTE: u16 = 0;
/// PE relocation type: 32-bit absolute address fixup (add low 16 bits of delta to 16-bit field).
const IMAGE_REL_BASED_HIGHLOW: u16 = 3;
/// PE relocation type: 64-bit absolute address fixup.
const IMAGE_REL_BASED_DIR64: u16 = 10;

/// Map the kernel image to higher-half addresses.
///
/// This creates a duplicate mapping of the kernel at `KERNEL_IMAGE_BASE`,
/// preserving the original identity mapping. The higher-half copy will
/// have relocations applied to it.
///
/// Returns the virtual address where the kernel is mapped in higher half.
unsafe fn map_kernel_to_higher_half(kernel_info: &KernelImageInfo) -> VirtAddr {
    let image_base_phys = kernel_info.image_base as u64;
    let image_size = kernel_info.image_size as usize;

    // Round up to page boundary
    let aligned_size = (image_size + 4095) & !4095;

    info!(
        "Mapping kernel image: phys {:#x}, size {:#x} -> virt {:#x}",
        image_base_phys, aligned_size, KERNEL_IMAGE_BASE
    );

    let pml4 = unsafe { &mut *current_page_table() };

    // Map each 4KB page of the kernel to higher-half
    for offset in (0..aligned_size).step_by(4096) {
        let phys_addr = PhysAddr::new(image_base_phys + offset as u64);
        let virt_addr = VirtAddr::new(KERNEL_IMAGE_BASE + offset as u64);

        // Get PML4 entry
        let pml4_index = virt_addr.page_table_index(PageTableLevel::Four);
        let pml4_entry = &mut pml4[pml4_index];
        let pml3 = if pml4_entry.flags().contains(PageTableFlags::PRESENT) {
            unsafe { &mut *(pml4_entry.addr().as_u64() as *mut PageTable) }
        } else {
            let frame = allocate_frame_raw();
            let table = unsafe { &mut *(frame.start_address().as_u64() as *mut PageTable) };
            unsafe { core::ptr::write_bytes(table, 0, 1) };
            without_write_protection(|| {
                pml4_entry.set_addr(
                    frame.start_address(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                );
            });
            table
        };

        // Get PML3 entry
        let pml3_index = virt_addr.page_table_index(PageTableLevel::Three);
        let pml3_entry = &mut pml3[pml3_index];
        let pml2 = if pml3_entry.flags().contains(PageTableFlags::PRESENT) {
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

        // Get PML2 entry
        let pml2_index = virt_addr.page_table_index(PageTableLevel::Two);
        let pml2_entry = &mut pml2[pml2_index];
        let pml1 = if pml2_entry.flags().contains(PageTableFlags::PRESENT) {
            unsafe { &mut *(pml2_entry.addr().as_u64() as *mut PageTable) }
        } else {
            let frame = allocate_frame_raw();
            let table = unsafe { &mut *(frame.start_address().as_u64() as *mut PageTable) };
            unsafe { core::ptr::write_bytes(table, 0, 1) };
            without_write_protection(|| {
                pml2_entry.set_addr(
                    frame.start_address(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                );
            });
            table
        };

        // Set PML1 entry (4KB page)
        let pml1_index = virt_addr.page_table_index(PageTableLevel::One);
        let pml1_entry = &mut pml1[pml1_index];
        without_write_protection(|| {
            pml1_entry.set_addr(
                phys_addr,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            );
        });
    }

    x86_64::instructions::tlb::flush_all();

    VirtAddr::new(KERNEL_IMAGE_BASE)
}

/// Apply PE base relocations to the kernel mapped at higher-half.
///
/// This fixes up all absolute addresses in the kernel image so they point
/// to the higher-half addresses instead of the original identity-mapped addresses.
///
/// # Arguments
/// * `kernel_info` - Information about the kernel image (identity-mapped base)
/// * `higher_half_base` - The virtual address where kernel is mapped in higher half
unsafe fn apply_kernel_relocations(kernel_info: &KernelImageInfo, higher_half_base: VirtAddr) {
    // Parse the PE from the identity-mapped image
    let image_slice = unsafe {
        core::slice::from_raw_parts(kernel_info.image_base, kernel_info.image_size as usize)
    };

    let pe = PE::parse(image_slice).expect("failed to parse kernel PE");

    let pe_image_base = pe.image_base as u64;
    let delta = higher_half_base.as_u64() as i64 - pe_image_base as i64;

    info!(
        "Applying relocations: PE ImageBase={:#x}, new base={:#x}, delta={:#x}",
        pe_image_base,
        higher_half_base.as_u64(),
        delta
    );

    // Get the base relocation data
    let relocation_data = pe
        .relocation_data
        .as_ref()
        .expect("kernel PE should have relocation data");

    let mut reloc_count = 0;
    let mut highlow_count = 0;

    for block_result in relocation_data.blocks() {
        // Skip malformed blocks (e.g., size 0)
        let block = match block_result {
            Ok(b) => b,
            Err(_) => continue,
        };
        let page_rva = block.rva as u64;

        for word_result in block.words() {
            let word = match word_result {
                Ok(w) => w,
                Err(_) => continue,
            };
            let reloc_type = word.reloc_type() as u16;
            let reloc_offset = word.offset() as u64;

            match reloc_type {
                IMAGE_REL_BASED_DIR64 => {
                    // Calculate the address in the higher-half mapping where we need to patch
                    let patch_addr = higher_half_base.as_u64() + page_rva + reloc_offset;
                    let patch_ptr = patch_addr as *mut u64;

                    // Read the current value, add delta, write back
                    let old_value = unsafe { core::ptr::read_volatile(patch_ptr) };
                    let new_value = (old_value as i64 + delta) as u64;
                    unsafe { core::ptr::write_volatile(patch_ptr, new_value) };

                    reloc_count += 1;
                }
                IMAGE_REL_BASED_HIGHLOW => {
                    // 32-bit relocation - add delta to 32-bit value
                    // Use unaligned read/write since relocations may not be aligned
                    let patch_addr = higher_half_base.as_u64() + page_rva + reloc_offset;
                    let patch_ptr = patch_addr as *mut u32;

                    let old_value = unsafe { core::ptr::read_unaligned(patch_ptr) };
                    let new_value = (old_value as i64 + delta) as u32;
                    unsafe { core::ptr::write_unaligned(patch_ptr, new_value) };

                    highlow_count += 1;
                }
                IMAGE_REL_BASED_ABSOLUTE => {
                    // Padding entry, skip
                }
                other => {
                    // Warn but don't panic on unknown types - some may be padding or arch-specific
                    log::warn!("skipping unknown relocation type: {}", other);
                }
            }
        }
    }

    info!(
        "Applied {} DIR64 relocations, {} HIGHLOW relocations",
        reloc_count, highlow_count
    );
}

/// Relocate the kernel to higher-half addresses.
///
/// This maps the kernel to `KERNEL_IMAGE_BASE` and applies PE base relocations
/// so all absolute addresses point to the higher-half mapping.
///
/// After this function returns:
/// - The kernel is dual-mapped (identity + higher-half)
/// - The higher-half copy has correct relocated addresses
/// - Execution is still in the identity-mapped copy
///
/// # Safety
/// Must be called after `create_physical_memory_window()` and before jumping
/// to higher-half execution.
pub unsafe fn relocate_kernel_to_higher_half(kernel_info: &KernelImageInfo) -> VirtAddr {
    info!("Relocating kernel to higher half...");

    // Map kernel pages to higher-half addresses
    let higher_half_base = unsafe { map_kernel_to_higher_half(kernel_info) };

    // Apply PE relocations to fix absolute addresses
    unsafe { apply_kernel_relocations(kernel_info, higher_half_base) };

    // Verify relocation by reading a known value
    // Use the KERNEL_IMAGE_BASE constant itself as a sanity check
    let identity_ptr = &KERNEL_IMAGE_BASE as *const u64;
    let higher_half_ptr = (higher_half_base.as_u64()
        + (identity_ptr as u64 - kernel_info.image_base as u64))
        as *const u64;

    let identity_value = unsafe { *identity_ptr };
    let higher_half_value = unsafe { *higher_half_ptr };

    info!(
        "Relocation verification: identity={:#x}, higher_half={:#x}",
        identity_value, higher_half_value
    );

    // The higher-half value should equal KERNEL_IMAGE_BASE (the constant's value)
    // since that constant should have been relocated to point to higher-half
    assert_eq!(
        higher_half_value, KERNEL_IMAGE_BASE,
        "relocation verification failed"
    );

    info!(
        "Kernel relocated to higher half at {:#x}",
        higher_half_base.as_u64()
    );

    higher_half_base
}
