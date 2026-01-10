use core::alloc::Layout;

use log::{debug, info};
use uefi::mem::memory_map::MemoryMapOwned;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::tlb,
    registers::control::{Cr0, Cr0Flags, Cr3, Efer, EferFlags},
    structures::paging::{
        PageTable, PageTableFlags, PhysFrame,
        page_table::{PageTableEntry, PageTableLevel},
    },
};

mod frame;
pub mod global_alloc;
pub mod heap_allocator;
mod mapping;

pub use frame::Frame;
pub use mapping::{Mapping, MappingBacking};

pub unsafe fn init_from_uefi(memory_map: &MemoryMapOwned) {
    let (heap_phys_base, heap_size) = heap_allocator::init_from_uefi(memory_map);
    unsafe {
        global_alloc::init(heap_phys_base, heap_size);
        Efer::update(|efer| efer.insert(EferFlags::NO_EXECUTE_ENABLE));
    }
}

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

pub fn physical_address_to_virtual(addr: PhysAddr) -> VirtAddr {
    // we identity map physical addresses
    VirtAddr::new(addr.as_u64())
}

/// Allocate a single 4KB frame with RAII guard.
pub fn allocate_frame() -> Frame {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    allocate_physical(layout)
}

/// Allocate physical memory with RAII guard.
pub fn allocate_physical(layout: Layout) -> Frame {
    let virt_addr = global_alloc::allocate(layout);
    let phys_addr = PhysAddr::new(virt_addr.as_u64());
    let frame = PhysFrame::from_start_address(phys_addr).unwrap();
    unsafe { Frame::new(frame, layout) }
}

/// Allocate a raw frame without RAII (for page table internals).
fn allocate_frame_raw() -> PhysFrame {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    let virt_addr = global_alloc::allocate(layout);
    let phys_addr = PhysAddr::new(virt_addr.as_u64());
    PhysFrame::from_start_address(phys_addr).unwrap()
}

/// Deallocate a raw frame.
///
/// # Safety
/// The frame must have been allocated with allocate_frame_raw().
unsafe fn deallocate_frame_raw(frame: PhysFrame) {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    let ptr = frame.start_address().as_u64() as *mut u8;
    unsafe {
        alloc::alloc::dealloc(ptr, layout);
    }
}

/// Create a new PML4 page table with kernel mappings copied from the current one.
/// Returns the physical address of the new page table.
///
/// Copies kernel identity mappings from the current page table, but NOT the
/// userspace mappings. This ensures each process has its own userspace
/// address space while sharing kernel mappings.
pub fn create_user_page_table() -> PhysAddr {
    let frame = allocate_frame_raw();
    let new_pml4 = frame.start_address().as_u64() as *mut PageTable;

    // PML4 entry indices for userspace virtual addresses:
    // 0xa0000000000 >> 39 = 20 (0x14)
    // 0xb0000000000 >> 39 = 22 (0x16)
    // We skip these entries to give each process its own userspace mappings.
    const USERSPACE_PML4_START: usize = 20;
    const USERSPACE_PML4_END: usize = 23; // exclusive

    let current_pml4 = current_page_table();
    unsafe {
        let src_table = &*current_pml4;
        let dst_table = &mut *new_pml4;

        // Zero the entire table first
        core::ptr::write_bytes(dst_table, 0, 1);

        // Copy only kernel entries (skip userspace range)
        for i in 0..512 {
            if i >= USERSPACE_PML4_START && i < USERSPACE_PML4_END {
                continue; // Skip userspace entries
            }
            dst_table[i] = src_table[i].clone();
        }
    }

    frame.start_address()
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

/// Get the current page table's physical address.
pub fn current_page_table_phys() -> PhysAddr {
    Cr3::read().0.start_address()
}

pub struct MemoryMappingOptions {
    pub user: bool,
    pub executable: bool,
    pub writable: bool,
}

/// Map physical memory to virtual address.
///
/// Note: This does not return an RAII guard. For managed mappings, use
/// `allocate_and_map` or `map_external` instead.
pub fn map(
    base_phys_addr: PhysAddr,
    base_virt_addr: VirtAddr,
    size_bytes: usize,
    options: MemoryMappingOptions,
) {
    map_inner(base_phys_addr, base_virt_addr, size_bytes, &options);
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

        debug!(
            "Mapping {phys_addr:#0X} to virtual {virt_addr:#0X} ({user}, {writable}, {no_execute})",
            user = if options.user {
                "user accessible"
            } else {
                "kernel-only"
            },
            writable = if options.writable {
                "writable"
            } else {
                "read-only"
            },
            no_execute = if options.executable {
                "executable"
            } else {
                "non-executable"
            }
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

        let (entry, level) = l1_page_table_entry(virt_addr, flags);
        let entry = unsafe { &mut *entry };

        info!(
            "Updating level {level:?} entry {entry:?} with address {phys_addr:?} and flags {flags:?}"
        );
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

fn current_page_table() -> *mut PageTable {
    let (page_table_frame, _flags) = Cr3::read();
    let page_table_vaddr = physical_address_to_virtual(page_table_frame.start_address());
    page_table_vaddr.as_mut_ptr::<PageTable>()
}

fn leaf_page_table_entry(
    addr: VirtAddr,
    flags: PageTableFlags,
) -> (*mut PageTableEntry, PageTableLevel) {
    let mut page_table = unsafe { &mut *current_page_table() };
    let mut level = PageTableLevel::Four;

    loop {
        let entry = &mut page_table[addr.page_table_index(level)];
        if entry.addr() == PhysAddr::zero() {
            return (entry, level);
        }

        let next_level = PageTableLevel::next_lower_level(level);
        if next_level == None {
            return (entry, level);
        }

        if level == PageTableLevel::Two && entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            return (entry, level);
        }

        let are_flags_valid = entry.flags().contains(flags & !PageTableFlags::NO_EXECUTE);
        if !are_flags_valid {
            return (entry, level);
        }

        level = next_level.unwrap();
        page_table = unsafe { &mut *(entry.addr().as_u64() as *mut PageTable) };
    }
}

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

        let entry_flags = (entry.flags() | flags) & !PageTableFlags::NO_EXECUTE;

        if entry.addr() == PhysAddr::zero() {
            let frame = allocate_frame_raw();

            info!(
                "Updating level {level:?} entry {entry:?} with address {phys_addr:?} and flags {flags:?}",
                phys_addr = frame.start_address()
            );
            without_write_protection(|| entry.set_addr(frame.start_address(), entry_flags));
            tlb::flush(VirtAddr::new(entry as *const _ as u64));
        } else {
            info!("Updating level {level:?} entry {entry:?} with flags {flags:?}");
            without_write_protection(|| entry.set_flags(entry_flags));
        }
    }
}

pub fn without_write_protection(f: impl FnOnce()) {
    unsafe {
        Cr0::update(|cr0| cr0.remove(Cr0Flags::WRITE_PROTECT));
    }

    f();

    unsafe {
        Cr0::update(|cr0| cr0.insert(Cr0Flags::WRITE_PROTECT));
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
    let page_table = current_page_table();

    // Walk down to find all tables in the path
    let mut tables: [Option<(*mut PageTable, usize)>; 4] = [None; 4];
    let mut table = page_table;

    for (i, level) in [
        PageTableLevel::Four,
        PageTableLevel::Three,
        PageTableLevel::Two,
        PageTableLevel::One,
    ]
    .iter()
    .enumerate()
    {
        let index = virt_addr.page_table_index(*level);
        let entry = unsafe { &(&*table)[index] };

        if !entry.flags().contains(PageTableFlags::PRESENT) {
            return; // Already unmapped
        }

        tables[i] = Some((table, index.into()));

        if *level == PageTableLevel::One {
            break;
        }

        // Handle huge pages at level 2
        if *level == PageTableLevel::Two && entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            // Clear the huge page entry
            without_write_protection(|| {
                unsafe { &mut (&mut *table)[index] }.set_unused();
            });
            tlb::flush(virt_addr);
            return;
        }

        table = entry.addr().as_u64() as *mut PageTable;
    }

    // Clear the L1 entry
    if let Some((l1_table, l1_index)) = tables[3] {
        without_write_protection(|| {
            unsafe { &mut (&mut *l1_table)[l1_index] }.set_unused();
        });
        tlb::flush(virt_addr);
    }

    // Walk back up and free empty intermediate tables
    // Start from L1 (index 3), check if empty, then free and clear L2 entry, etc.
    for level_idx in (0..3).rev() {
        let Some((child_table, _)) = tables[level_idx + 1] else {
            break;
        };
        let Some((parent_table, parent_index)) = tables[level_idx] else {
            break;
        };

        // Check if child table is completely empty
        let is_empty = unsafe {
            (*child_table)
                .iter()
                .all(|entry| !entry.flags().contains(PageTableFlags::PRESENT))
        };

        if is_empty {
            // Get the physical address of the child table before clearing entry
            let child_frame_addr = unsafe { (&*parent_table)[parent_index].addr() };
            let child_frame = PhysFrame::from_start_address(child_frame_addr).unwrap();

            // Clear the parent entry
            without_write_protection(|| {
                unsafe { &mut (&mut *parent_table)[parent_index] }.set_unused();
            });

            // Deallocate the empty child table
            unsafe {
                deallocate_frame_raw(child_frame);
            }

            debug!(
                "Freed empty page table at {:?} (level {})",
                child_frame_addr,
                3 - level_idx
            );
        } else {
            // If this table isn't empty, higher levels won't be either
            break;
        }
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
    let page_table = current_page_table();

    // Walk down to find the L1 entry
    let mut tables: [Option<(*mut PageTable, usize)>; 4] = [None; 4];
    let mut table = page_table;

    for (i, level) in [
        PageTableLevel::Four,
        PageTableLevel::Three,
        PageTableLevel::Two,
        PageTableLevel::One,
    ]
    .iter()
    .enumerate()
    {
        let index = virt_addr.page_table_index(*level);
        let entry = unsafe { &(&*table)[index] };

        if !entry.flags().contains(PageTableFlags::PRESENT) {
            return; // Not mapped, nothing to free
        }

        tables[i] = Some((table, index.into()));

        if *level == PageTableLevel::One {
            // Found L1 entry - get the frame address before clearing
            let frame_addr = entry.addr();
            let frame = PhysFrame::from_start_address(frame_addr).unwrap();

            // Clear the entry
            without_write_protection(|| {
                unsafe { &mut (&mut *table)[index] }.set_unused();
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
            without_write_protection(|| {
                unsafe { &mut (&mut *table)[index] }.set_unused();
            });
            tlb::flush(virt_addr);
            // Note: huge page frame deallocation not implemented
            return;
        }

        table = entry.addr().as_u64() as *mut PageTable;
    }

    // Walk back up and free empty intermediate tables (same as unmap_page)
    for level_idx in (0..3).rev() {
        let Some((child_table, _)) = tables[level_idx + 1] else {
            break;
        };
        let Some((parent_table, parent_index)) = tables[level_idx] else {
            break;
        };

        let is_empty = unsafe {
            (*child_table)
                .iter()
                .all(|entry| !entry.flags().contains(PageTableFlags::PRESENT))
        };

        if is_empty {
            let child_frame_addr = unsafe { (&*parent_table)[parent_index].addr() };
            let child_frame = PhysFrame::from_start_address(child_frame_addr).unwrap();

            without_write_protection(|| {
                unsafe { &mut (&mut *parent_table)[parent_index] }.set_unused();
            });

            unsafe {
                deallocate_frame_raw(child_frame);
            }
        } else {
            break;
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

    log::debug!("Demand paging heap page at {:#x}", page_addr.as_u64());

    // Allocate a physical frame
    let frame = allocate_frame();
    let phys_addr = PhysAddr::new(frame.phys_frame().start_address().as_u64());

    // Map it to the faulting address (user, writable, no-execute)
    map(
        phys_addr,
        page_addr,
        4096,
        MemoryMappingOptions {
            user: true,
            writable: true,
            executable: false,
        },
    );

    // Zero the page for security
    unsafe {
        core::ptr::write_bytes(page_addr.as_u64() as *mut u8, 0, 4096);
    }

    // Intentionally leak the frame - it's now owned by the page tables
    // and will be freed when the heap shrinks or process exits via free_region()
    core::mem::forget(frame);

    true
}
