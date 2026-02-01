//! Page table management and memory mapping.

use log::debug;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::tlb,
    registers::control::Cr3,
    structures::paging::{
        PageTable, PageTableFlags, PhysFrame,
        page_table::{PageTableEntry, PageTableLevel},
    },
};

use super::mapping::{Mapping, MappingBacking};
use super::recursive;
use super::write_protection::without_write_protection;
use super::{
    MemoryMappingOptions, allocate_frame, allocate_frame_raw, allocate_physical,
    deallocate_frame_raw,
};

/// Get the current page table pointer via recursive mapping.
pub fn current_page_table() -> *mut PageTable {
    recursive::pml4_mut() as *mut PageTable
}

/// Get the current page table's physical address.
pub fn current_page_table_phys() -> PhysAddr {
    Cr3::read().0.start_address()
}

/// Switch to a different page table.
///
/// # Safety
/// The page table must be valid and contain correct kernel mappings.
pub unsafe fn switch_page_table(pml4_phys: PhysAddr) {
    let frame = PhysFrame::from_start_address(pml4_phys).unwrap();
    unsafe {
        Cr3::write(frame, Cr3::read().1);
    }
}

/// Create a new PML4 page table with kernel mappings copied from the current one.
/// Returns the physical address of the new page table.
///
/// Copies only the higher-half kernel mappings (PML4 entries 256-511) from the
/// current page table. The entire lower half (entries 0-255) is left empty for
/// userspace to use.
///
/// Higher-half entries include:
/// - Physical memory window (0xffff_8000...)
/// - MMIO region (0xffff_9000...)
/// - Kernel heap (0xffff_a000...)
/// - Kernel image (0xffff_c000...)
pub fn create_user_page_table() -> PhysAddr {
    // Allocate a frame for the new PML4 (already zeroed by alloc_zeroed)
    let frame = super::allocate_frame();
    let frame_phys = frame.start_address();

    // Access the new page table via its heap virtual address
    let new_pml4 = frame.virtual_address().as_mut_ptr::<PageTable>();

    // Higher-half starts at PML4 index 256 (0xffff_8000_0000_0000 >> 39 = 256)
    const HIGHER_HALF_START: usize = 256;

    // Access current PML4 via recursive mapping
    let current_pml4 = recursive::pml4();

    unsafe {
        let dst_table = &mut *new_pml4;

        // Copy only higher-half kernel entries (256-511)
        // Lower half (0-255) is left empty for userspace
        // Skip the recursive entry (510) - we'll set it up for the new table
        for i in HIGHER_HALF_START..512 {
            if i != recursive::RECURSIVE_INDEX {
                dst_table[i] = current_pml4[i].clone();
            }
        }

        // Set up recursive entry for the new page table
        dst_table[recursive::RECURSIVE_INDEX].set_addr(
            frame_phys,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
        );
    }

    // Leak the frame - it's now owned by the process
    core::mem::forget(frame);

    frame_phys
}

/// Update permissions for already-mapped pages.
/// This is used when ELF segments overlap and need merged permissions.
pub fn update_permissions(
    base_virt_addr: VirtAddr,
    size_bytes: usize,
    options: MemoryMappingOptions,
) {
    assert!(
        base_virt_addr.is_aligned(4096u64),
        "virtual address must be page-aligned"
    );

    let mut flags = PageTableFlags::PRESENT;
    if options.user {
        flags |= PageTableFlags::USER_ACCESSIBLE;
    }
    if options.writable {
        flags |= PageTableFlags::WRITABLE;
    }
    if !options.executable {
        flags |= PageTableFlags::NO_EXECUTE;
    }

    for i in (0..size_bytes).step_by(4096) {
        let virt_addr = base_virt_addr + i as u64;

        let (entry, _level) = l1_page_table_entry(virt_addr, flags);
        let entry = unsafe { &mut *entry };
        without_write_protection(|| entry.set_flags(flags));
    }

    // Single TLB flush after all permission updates
    tlb::flush_all();
}

/// Check if a virtual address is already mapped (including via huge pages).
/// Returns true if the address can be accessed without a page fault.
fn is_mapped(addr: VirtAddr) -> bool {
    let mut level = PageTableLevel::Four;

    loop {
        // Access page table at current level via recursive mapping
        let page_table = unsafe { recursive::table_for_addr(addr, level) };
        let entry = &page_table[addr.page_table_index(level)];

        if !entry.flags().contains(PageTableFlags::PRESENT) {
            return false;
        }

        // Check for huge page at level 2 (2MB) or level 3 (1GB)
        if (level == PageTableLevel::Two || level == PageTableLevel::Three)
            && entry.flags().contains(PageTableFlags::HUGE_PAGE)
        {
            return true; // Huge page covers this address
        }

        let Some(next_level) = level.next_lower_level() else {
            return true; // Reached L1, address is mapped
        };

        level = next_level;
    }
}

/// Map physical memory to virtual address (internal implementation).
///
/// When `flush_tlb` is false, the caller is responsible for issuing a TLB flush
/// after all mappings are complete. This is an optimisation for batch mapping where
/// a single `tlb::flush_all()` replaces N individual `tlb::flush()` calls.
fn map_inner(
    base_phys_addr: PhysAddr,
    base_virt_addr: VirtAddr,
    size_bytes: usize,
    options: &MemoryMappingOptions,
    flush_tlb: bool,
) {
    assert!(
        base_phys_addr.is_aligned(4096u64),
        "physical address must be page-aligned"
    );
    assert!(
        base_virt_addr.is_aligned(4096u64),
        "virtual address must be page-aligned"
    );

    for i in (0..size_bytes).step_by(4096) {
        let phys_addr = base_phys_addr + i as u64;
        let virt_addr = base_virt_addr + i as u64;

        // Skip if already mapped (e.g., by UEFI huge pages)
        if is_mapped(virt_addr) {
            continue;
        }

        let mut flags = PageTableFlags::PRESENT;
        if options.user {
            flags |= PageTableFlags::USER_ACCESSIBLE;
        }
        if options.writable {
            flags |= PageTableFlags::WRITABLE;
        }
        if !options.executable {
            flags |= PageTableFlags::NO_EXECUTE;
        }

        let (entry, _level) = l1_page_table_entry(virt_addr, flags);
        let entry = unsafe { &mut *entry };
        without_write_protection(|| entry.set_addr(phys_addr, flags));
        if flush_tlb {
            tlb::flush(virt_addr);
        }
    }
}

/// Size of a 2MB huge page.
const HUGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

/// Allocate frames and map them to a virtual address with RAII guard.
/// Returns a mapping that owns the backing frames.
///
/// Optimisations applied:
/// - Caches intermediate page table levels within 2MB regions
/// - Pre-allocates all frames before mapping for better cache locality
/// - Uses 2MB huge pages for aligned regions >= 2MB (512x fewer page table entries)
pub fn allocate_and_map(
    base_virt_addr: VirtAddr,
    size_bytes: usize,
    options: MemoryMappingOptions,
) -> Mapping {
    use alloc::vec::Vec;
    use core::alloc::Layout;

    assert!(
        base_virt_addr.is_aligned(4096u64),
        "virtual address must be page-aligned"
    );

    let aligned_size = (size_bytes + 4095) & !4095;

    let mut flags = PageTableFlags::PRESENT;
    if options.user {
        flags |= PageTableFlags::USER_ACCESSIBLE;
    }
    if options.writable {
        flags |= PageTableFlags::WRITABLE;
    }
    if !options.executable {
        flags |= PageTableFlags::NO_EXECUTE;
    }

    // Calculate the 2MB-aligned regions for huge page usage.
    // Layout: [head 4KB pages] [2MB huge pages] [tail 4KB pages]
    let start = base_virt_addr.as_u64() as usize;
    let end = start + aligned_size;
    let first_huge_boundary = (start + HUGE_PAGE_SIZE - 1) & !(HUGE_PAGE_SIZE - 1);
    let last_huge_boundary = end & !(HUGE_PAGE_SIZE - 1);

    let head_size = if first_huge_boundary <= end && last_huge_boundary > first_huge_boundary {
        first_huge_boundary - start
    } else {
        aligned_size // No huge pages possible, everything is head
    };
    let huge_size = if head_size < aligned_size {
        last_huge_boundary - first_huge_boundary
    } else {
        0
    };
    let tail_size = aligned_size - head_size - huge_size;

    let head_pages = head_size / 4096;
    let huge_count = huge_size / HUGE_PAGE_SIZE;
    let tail_pages = tail_size / 4096;

    // Pre-allocate all frames
    let total_frame_count = head_pages + huge_count + tail_pages;
    let mut frames = Vec::with_capacity(total_frame_count);

    // Allocate head (4KB pages)
    for _ in 0..head_pages {
        frames.push(allocate_frame());
    }

    // Allocate huge pages (2MB each, 2MB-aligned)
    for _ in 0..huge_count {
        let layout = Layout::from_size_align(HUGE_PAGE_SIZE, HUGE_PAGE_SIZE).unwrap();
        frames.push(allocate_physical(layout));
    }

    // Allocate tail (4KB pages)
    for _ in 0..tail_pages {
        frames.push(allocate_frame());
    }

    // Map head pages (4KB) with cached L1 table
    let mut cached_l1_table: Option<(*mut PageTable, u64)> = None;
    for i in 0..head_pages {
        let frame = &frames[i];
        let phys_addr = frame.start_address();
        let virt_addr = base_virt_addr + (i * 4096) as u64;
        let region_2mb = virt_addr.as_u64() & !0x1F_FFFF;

        let l1_table = match cached_l1_table {
            Some((table_ptr, cached_region)) if cached_region == region_2mb => table_ptr,
            _ => {
                let _ = l1_page_table_entry(virt_addr, flags);
                let table_ptr =
                    unsafe { recursive::table_for_addr_mut(virt_addr, PageTableLevel::One) }
                        as *mut PageTable;
                cached_l1_table = Some((table_ptr, region_2mb));
                table_ptr
            }
        };

        let l1_index = virt_addr.page_table_index(PageTableLevel::One);
        let l1_table_ref = unsafe { &mut *l1_table };
        let entry = &mut l1_table_ref[l1_index];
        without_write_protection(|| entry.set_addr(phys_addr, flags));
    }

    // Map huge pages (2MB) via L2 entries
    let huge_flags = flags | PageTableFlags::HUGE_PAGE;
    for i in 0..huge_count {
        let frame = &frames[head_pages + i];
        let phys_addr = frame.start_address();
        let virt_addr = VirtAddr::new(first_huge_boundary as u64 + (i * HUGE_PAGE_SIZE) as u64);

        let l2_entry = l2_page_table_entry(virt_addr, flags);
        let l2_entry = unsafe { &mut *l2_entry };
        without_write_protection(|| l2_entry.set_addr(phys_addr, huge_flags));
    }

    if huge_count > 0 {
        debug!(
            "Mapped {} x 2MB huge pages at {:#x}..{:#x}",
            huge_count,
            first_huge_boundary,
            last_huge_boundary
        );
    }

    // Map tail pages (4KB) with cached L1 table
    cached_l1_table = None;
    for i in 0..tail_pages {
        let frame = &frames[head_pages + huge_count + i];
        let phys_addr = frame.start_address();
        let virt_addr = VirtAddr::new(last_huge_boundary as u64 + (i * 4096) as u64);
        let region_2mb = virt_addr.as_u64() & !0x1F_FFFF;

        let l1_table = match cached_l1_table {
            Some((table_ptr, cached_region)) if cached_region == region_2mb => table_ptr,
            _ => {
                let _ = l1_page_table_entry(virt_addr, flags);
                let table_ptr =
                    unsafe { recursive::table_for_addr_mut(virt_addr, PageTableLevel::One) }
                        as *mut PageTable;
                cached_l1_table = Some((table_ptr, region_2mb));
                table_ptr
            }
        };

        let l1_index = virt_addr.page_table_index(PageTableLevel::One);
        let l1_table_ref = unsafe { &mut *l1_table };
        let entry = &mut l1_table_ref[l1_index];
        without_write_protection(|| entry.set_addr(phys_addr, flags));
    }

    // Single TLB flush for all pages mapped above
    tlb::flush_all();

    Mapping::new(base_virt_addr, aligned_size, MappingBacking::Frames(frames))
}

/// Map external physical memory (e.g., MMIO) to a virtual address with RAII guard.
/// The backing memory is NOT deallocated when the mapping is dropped.
pub fn map_external(
    base_phys_addr: PhysAddr,
    base_virt_addr: VirtAddr,
    size_bytes: usize,
    options: MemoryMappingOptions,
) -> Mapping {
    map_inner(base_phys_addr, base_virt_addr, size_bytes, &options, true);
    Mapping::new(base_virt_addr, size_bytes, MappingBacking::Mmio)
}

/// Find the leaf page table entry for a virtual address.
fn leaf_page_table_entry(
    addr: VirtAddr,
    flags: PageTableFlags,
) -> (*mut PageTableEntry, PageTableLevel) {
    let mut level = PageTableLevel::Four;

    loop {
        // Access page table at current level via recursive mapping
        let page_table = unsafe { recursive::table_for_addr_mut(addr, level) };
        let entry = &mut page_table[addr.page_table_index(level)];
        if entry.addr() == PhysAddr::zero() {
            return (entry, level);
        }

        let Some(next_level) = level.next_lower_level() else {
            return (entry, level);
        };

        if level == PageTableLevel::Two && entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            return (entry, level);
        }

        let are_flags_valid = entry.flags().contains(flags & !PageTableFlags::NO_EXECUTE);
        if !are_flags_valid {
            return (entry, level);
        }

        level = next_level;
    }
}

/// Get or create the L1 page table entry for a virtual address.
///
/// Note: This function assumes the address is NOT covered by a huge page.
/// Call `is_mapped()` first to check if the address is already accessible.
fn l1_page_table_entry(
    addr: VirtAddr,
    flags: PageTableFlags,
) -> (*mut PageTableEntry, PageTableLevel) {
    loop {
        let (entry, level) = leaf_page_table_entry(addr, flags);
        let entry = unsafe { &mut *entry };

        if level == PageTableLevel::One {
            return (entry, level);
        }

        // Intermediate page table entries need PRESENT and must NOT have NO_EXECUTE
        let entry_flags =
            (entry.flags() | flags | PageTableFlags::PRESENT) & !PageTableFlags::NO_EXECUTE;

        if entry.addr() == PhysAddr::zero() {
            let frame = allocate_frame_raw();
            without_write_protection(|| entry.set_addr(frame.start_address(), entry_flags));
            tlb::flush(VirtAddr::new(entry as *const _ as u64));
        } else {
            without_write_protection(|| entry.set_flags(entry_flags));
        }
    }
}

/// Get or create the L2 page table entry for a virtual address.
///
/// Used for setting up 2MB huge page entries. Ensures L4 and L3 intermediate
/// tables exist, then returns a pointer to the L2 entry.
fn l2_page_table_entry(
    addr: VirtAddr,
    flags: PageTableFlags,
) -> *mut PageTableEntry {
    // Ensure L4 → L3 → L2 path exists
    let intermediate_flags =
        (flags | PageTableFlags::PRESENT) & !PageTableFlags::NO_EXECUTE;

    // Walk L4
    let pml4 = unsafe { recursive::table_for_addr_mut(addr, PageTableLevel::Four) };
    let l4_entry = &mut pml4[addr.page_table_index(PageTableLevel::Four)];
    if l4_entry.addr() == PhysAddr::zero() {
        let frame = allocate_frame_raw();
        without_write_protection(|| l4_entry.set_addr(frame.start_address(), intermediate_flags));
        tlb::flush(VirtAddr::new(l4_entry as *const _ as u64));
    } else if !l4_entry.flags().contains(intermediate_flags & !PageTableFlags::NO_EXECUTE) {
        let new_flags = l4_entry.flags() | intermediate_flags;
        without_write_protection(|| l4_entry.set_flags(new_flags));
    }

    // Walk L3
    let pdpt = unsafe { recursive::table_for_addr_mut(addr, PageTableLevel::Three) };
    let l3_entry = &mut pdpt[addr.page_table_index(PageTableLevel::Three)];
    if l3_entry.addr() == PhysAddr::zero() {
        let frame = allocate_frame_raw();
        without_write_protection(|| l3_entry.set_addr(frame.start_address(), intermediate_flags));
        tlb::flush(VirtAddr::new(l3_entry as *const _ as u64));
    } else if !l3_entry.flags().contains(intermediate_flags & !PageTableFlags::NO_EXECUTE) {
        let new_flags = l3_entry.flags() | intermediate_flags;
        without_write_protection(|| l3_entry.set_flags(new_flags));
    }

    // Return the L2 entry
    let pd = unsafe { recursive::table_for_addr_mut(addr, PageTableLevel::Two) };
    &mut pd[addr.page_table_index(PageTableLevel::Two)] as *mut PageTableEntry
}

/// Unmap a virtual address region, clearing page table entries.
/// Also frees any intermediate page tables that become empty.
pub fn unmap_region(base_virt: VirtAddr, size_bytes: usize) {
    assert!(
        base_virt.is_aligned(4096u64),
        "virtual address must be page-aligned"
    );

    for offset in (0..size_bytes).step_by(4096) {
        let virt_addr = base_virt + offset as u64;
        unmap_page(virt_addr);
    }
}

/// Unmap a single page and free empty intermediate page tables.
pub fn unmap_page(virt_addr: VirtAddr) {
    let levels = [
        PageTableLevel::Four,
        PageTableLevel::Three,
        PageTableLevel::Two,
        PageTableLevel::One,
    ];

    // Walk down to find the depth we can reach
    let mut max_depth = 0;
    for (i, level) in levels.iter().enumerate() {
        let table = unsafe { recursive::table_for_addr(virt_addr, *level) };
        let index = virt_addr.page_table_index(*level);
        let entry = &table[index];

        if !entry.flags().contains(PageTableFlags::PRESENT) {
            return; // Already unmapped
        }

        max_depth = i;

        // Handle huge pages at level 2
        if *level == PageTableLevel::Two && entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            let table = unsafe { recursive::table_for_addr_mut(virt_addr, *level) };
            without_write_protection(|| {
                table[index].set_unused();
            });
            tlb::flush(virt_addr);
            return;
        }

        if *level == PageTableLevel::One {
            break;
        }
    }

    // Clear the L1 entry
    if max_depth == 3 {
        let l1_table = unsafe { recursive::table_for_addr_mut(virt_addr, PageTableLevel::One) };
        let l1_index = virt_addr.page_table_index(PageTableLevel::One);
        without_write_protection(|| {
            l1_table[l1_index].set_unused();
        });
        tlb::flush(virt_addr);
    }

    // Walk back up and free empty intermediate tables
    // Check L1 -> L2 -> L3 (don't free PML4 entries)
    for level in [
        PageTableLevel::One,
        PageTableLevel::Two,
        PageTableLevel::Three,
    ] {
        let child_table = unsafe { recursive::table_for_addr(virt_addr, level) };

        // Check if child table is completely empty
        let is_empty = child_table
            .iter()
            .all(|entry| !entry.flags().contains(PageTableFlags::PRESENT));

        if !is_empty {
            // If this table isn't empty, higher levels won't be either
            break;
        }

        // Safe to unwrap: levels 1, 2, 3 all have a higher level
        let parent_level = level.next_higher_level().unwrap();
        let parent_table = unsafe { recursive::table_for_addr_mut(virt_addr, parent_level) };
        let parent_index = virt_addr.page_table_index(parent_level);

        // Get the physical address of the child table before clearing entry
        let child_frame_addr = parent_table[parent_index].addr();
        let child_frame = PhysFrame::from_start_address(child_frame_addr).unwrap();

        // Clear the parent entry
        without_write_protection(|| {
            parent_table[parent_index].set_unused();
        });

        // Deallocate the empty child table
        unsafe {
            deallocate_frame_raw(child_frame);
        }

        debug!(
            "Freed empty page table at {:?} (level {:?})",
            child_frame_addr, level
        );
    }
}
