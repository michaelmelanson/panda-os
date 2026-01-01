use core::alloc::Layout;

use log::{debug, info};
use uefi::mem::memory_map::MemoryMapOwned;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::tlb,
    registers::control::{Cr0, Cr0Flags, Cr3},
    structures::paging::{
        PageTable, PageTableFlags, PhysFrame,
        page_table::{PageTableEntry, PageTableLevel},
    },
};

pub mod global_alloc;
pub mod heap_allocator;

pub unsafe fn init_from_uefi(memory_map: &MemoryMapOwned) {
    let (heap_phys_base, heap_size) = heap_allocator::init_from_uefi(memory_map);
    unsafe {
        global_alloc::init(heap_phys_base, heap_size);
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

pub fn allocate_frame() -> PhysFrame {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    allocate_physical(layout)
}

pub fn allocate_physical(layout: Layout) -> PhysFrame {
    let virt_addr = global_alloc::allocate(layout);
    let phys_addr = PhysAddr::new(virt_addr.as_u64());
    PhysFrame::from_start_address(phys_addr).unwrap()
}

pub struct MemoryMappingOptions {
    pub user: bool,
    pub executable: bool,
    pub writable: bool,
}

pub fn map(
    base_phys_addr: PhysAddr,
    base_virt_addr: VirtAddr,
    size_bytes: usize,
    options: MemoryMappingOptions,
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
        // seems to be causing a 'reserved write' page fault
        // if !options.executable {
        //     flags |= PageTableFlags::NO_EXECUTE;
        // }

        let (entry, level) = l1_page_table_entry(virt_addr, flags);
        let entry = unsafe { &mut *entry };

        info!(
            "Updating level {level:?} entry {entry:?} with address {phys_addr:?} and flags {flags:?}"
        );
        without_write_protection(|| entry.set_addr(phys_addr, flags));
        tlb::flush(virt_addr);
    }
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

        if !entry.flags().contains(flags) {
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

        if entry.addr() == PhysAddr::zero() {
            let frame = allocate_frame();
            without_write_protection(|| entry.set_addr(frame.start_address(), flags));
            tlb::flush(VirtAddr::new(entry as *const _ as u64));
        } else {
            without_write_protection(|| entry.set_flags(entry.flags().union(flags)));
        }

        if level == PageTableLevel::One {
            return (entry, level);
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
