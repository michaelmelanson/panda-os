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

fn without_write_protection(f: impl FnOnce()) {
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
fn unmap_page(virt_addr: VirtAddr) {
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
