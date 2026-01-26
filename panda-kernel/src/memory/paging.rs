//! Page table management and memory mapping.

use log::debug;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::tlb,
    registers::control::{Cr0, Cr0Flags, Cr3},
    structures::paging::{
        PageTable, PageTableFlags, PhysFrame,
        page_table::{PageTableEntry, PageTableLevel},
    },
};

use super::mapping::{Mapping, MappingBacking};
use super::recursive;
use super::{MemoryMappingOptions, allocate_frame, allocate_frame_raw, deallocate_frame_raw};

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

/// Execute a closure with write protection disabled.
pub fn without_write_protection(f: impl FnOnce()) {
    unsafe {
        Cr0::update(|cr0| cr0.remove(Cr0Flags::WRITE_PROTECT));
    }

    f();

    unsafe {
        Cr0::update(|cr0| cr0.insert(Cr0Flags::WRITE_PROTECT));
    }
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

    for i in (0..size_bytes).step_by(4096) {
        let virt_addr = base_virt_addr + i as u64;

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
        without_write_protection(|| entry.set_flags(flags));
        tlb::flush(virt_addr);
    }
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
fn map_inner(
    base_phys_addr: PhysAddr,
    base_virt_addr: VirtAddr,
    size_bytes: usize,
    options: &MemoryMappingOptions,
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
        tlb::flush(virt_addr);
    }
}

/// Allocate frames and map them to a virtual address with RAII guard.
/// Returns a mapping that owns the backing frames.
pub fn allocate_and_map(
    base_virt_addr: VirtAddr,
    size_bytes: usize,
    options: MemoryMappingOptions,
) -> Mapping {
    use alloc::vec::Vec;

    assert!(
        base_virt_addr.is_aligned(4096u64),
        "virtual address must be page-aligned"
    );

    let aligned_size = (size_bytes + 4095) & !4095;
    let mut frames = Vec::new();

    for offset in (0..aligned_size).step_by(4096) {
        let frame = allocate_frame();
        let phys_addr = frame.start_address();
        let virt_addr = base_virt_addr + offset as u64;

        map_inner(phys_addr, virt_addr, 4096, &options);
        frames.push(frame);
    }

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
    map_inner(base_phys_addr, base_virt_addr, size_bytes, &options);
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

/// Free a region by walking page tables, deallocating mapped frames, and clearing PTEs.
/// Unlike unmap_region, this also deallocates the physical frames.
/// Used for demand-paged regions where frames aren't tracked separately.
pub fn free_region(base_virt: VirtAddr, size_bytes: usize) {
    for offset in (0..size_bytes).step_by(4096) {
        let virt_addr = base_virt + offset as u64;
        free_page(virt_addr);
    }
}

/// Free a single page: deallocate its frame (if mapped) and clear the PTE.
/// Unlike unmap_page, this also deallocates the physical frame.
fn free_page(virt_addr: VirtAddr) {
    let levels = [
        PageTableLevel::Four,
        PageTableLevel::Three,
        PageTableLevel::Two,
        PageTableLevel::One,
    ];

    // Walk down to find the L1 entry
    for level in levels.iter() {
        let table = unsafe { recursive::table_for_addr(virt_addr, *level) };
        let index = virt_addr.page_table_index(*level);
        let entry = &table[index];

        if !entry.flags().contains(PageTableFlags::PRESENT) {
            return; // Not mapped, nothing to free
        }

        if *level == PageTableLevel::One {
            // Found L1 entry - get the frame address before clearing
            let frame_addr = entry.addr();
            let frame = PhysFrame::from_start_address(frame_addr).unwrap();

            // Clear the entry
            let table = unsafe { recursive::table_for_addr_mut(virt_addr, *level) };
            without_write_protection(|| {
                table[index].set_unused();
            });
            tlb::flush(virt_addr);

            // Deallocate the frame
            unsafe {
                deallocate_frame_raw(frame);
            }
            break;
        }

        // Handle huge pages at level 2 (not expected for heap, but handle anyway)
        if *level == PageTableLevel::Two && entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            let table = unsafe { recursive::table_for_addr_mut(virt_addr, *level) };
            without_write_protection(|| {
                table[index].set_unused();
            });
            tlb::flush(virt_addr);
            // Note: huge page frame deallocation not implemented
            return;
        }
    }

    // Walk back up and free empty intermediate tables (same as unmap_page)
    for level in [
        PageTableLevel::One,
        PageTableLevel::Two,
        PageTableLevel::Three,
    ] {
        let child_table = unsafe { recursive::table_for_addr(virt_addr, level) };

        let is_empty = child_table
            .iter()
            .all(|entry| !entry.flags().contains(PageTableFlags::PRESENT));

        if !is_empty {
            break;
        }

        // Safe to unwrap: levels 1, 2, 3 all have a higher level
        let parent_level = level.next_higher_level().unwrap();
        let parent_table = unsafe { recursive::table_for_addr_mut(virt_addr, parent_level) };
        let parent_index = virt_addr.page_table_index(parent_level);

        let child_frame_addr = parent_table[parent_index].addr();
        let child_frame = PhysFrame::from_start_address(child_frame_addr).unwrap();

        without_write_protection(|| {
            parent_table[parent_index].set_unused();
        });

        unsafe {
            deallocate_frame_raw(child_frame);
        }
    }
}

/// Try to handle a page fault for userspace heap demand paging.
/// Returns true if handled, false if fault should be treated as error.
/// The allocated frame is intentionally leaked (not tracked by RAII) because
/// heap frames are managed by the page tables themselves and freed via free_region().
pub fn try_handle_heap_page_fault(fault_addr: VirtAddr, brk: VirtAddr) -> bool {
    let heap_base = panda_abi::HEAP_BASE as u64;

    // Check if fault address is within the valid heap region [HEAP_BASE, brk)
    if fault_addr.as_u64() < heap_base || fault_addr.as_u64() >= brk.as_u64() {
        return false;
    }

    // Page-align the fault address
    let page_addr = VirtAddr::new(fault_addr.as_u64() & !0xFFF);

    // Allocate a physical frame (already zeroed by alloc_zeroed)
    let frame = super::allocate_frame();
    let phys_addr = PhysAddr::new(frame.phys_frame().start_address().as_u64());

    // Map it to the faulting address (user, writable, no-execute)
    let mapping = map_external(
        phys_addr,
        page_addr,
        4096,
        MemoryMappingOptions {
            user: true,
            writable: true,
            executable: false,
        },
    );

    // Intentionally leak the frame and mapping - they're now owned by the page tables
    // and will be freed when the heap shrinks or process exits via free_region()
    core::mem::forget(frame);
    core::mem::forget(mapping);

    true
}

/// Try to handle a page fault for userspace stack demand paging.
/// Returns true if handled, false if fault should be treated as error.
/// Stack grows downward within [STACK_BASE, STACK_BASE + STACK_MAX_SIZE).
pub fn try_handle_stack_page_fault(fault_addr: VirtAddr) -> bool {
    let stack_base = panda_abi::STACK_BASE as u64;
    let stack_end = stack_base + panda_abi::STACK_MAX_SIZE as u64;

    // Check if fault address is within the stack region
    if fault_addr.as_u64() < stack_base || fault_addr.as_u64() >= stack_end {
        return false;
    }

    // Page-align the fault address
    let page_addr = VirtAddr::new(fault_addr.as_u64() & !0xFFF);

    // Allocate a physical frame (already zeroed by alloc_zeroed)
    let frame = super::allocate_frame();
    let phys_addr = PhysAddr::new(frame.phys_frame().start_address().as_u64());

    // Map it to the faulting address (user, writable, no-execute)
    let mapping = map_external(
        phys_addr,
        page_addr,
        4096,
        MemoryMappingOptions {
            user: true,
            writable: true,
            executable: false,
        },
    );

    // Intentionally leak the frame and mapping - they're now owned by the page tables
    // and will be freed when the process exits via free_region()
    core::mem::forget(frame);
    core::mem::forget(mapping);

    true
}
