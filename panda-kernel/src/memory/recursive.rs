//! Recursive page table access.
//!
//! PML4[510] points to PML4 itself, making all page tables accessible
//! at calculated virtual addresses without a physical memory window.
//!
//! This works because when the MMU walks the page tables and encounters
//! the recursive entry, it loops back to the PML4. By carefully choosing
//! which indices we use, we can access any level of page table.

use x86_64::VirtAddr;
use x86_64::structures::paging::PageTable;
use x86_64::structures::paging::page_table::PageTableLevel;

/// The PML4 index used for recursive mapping.
/// Entry 510 is chosen to avoid conflicts with kernel space (starts at 256).
pub const RECURSIVE_INDEX: usize = 510;

/// Calculate virtual address to access page table at given level for a virtual address.
///
/// For a given virtual address, this returns the virtual address where the
/// page table at the specified level can be accessed (via the recursive mapping).
pub fn table_addr(virt: VirtAddr, level: PageTableLevel) -> VirtAddr {
    let addr = virt.as_u64();
    let pml4_idx = (addr >> 39) & 0x1FF;
    let pdpt_idx = (addr >> 30) & 0x1FF;
    let pd_idx = (addr >> 21) & 0x1FF;
    let r = RECURSIVE_INDEX as u64;

    // The recursive mapping works by replacing higher-level indices with the
    // recursive index, causing the MMU to loop back through the PML4.
    let result = match level {
        // To access PML4: all indices are recursive
        PageTableLevel::Four => (r << 39) | (r << 30) | (r << 21) | (r << 12),
        // To access PDPT for virt: PML4 index from virt, rest recursive
        PageTableLevel::Three => (r << 39) | (r << 30) | (r << 21) | (pml4_idx << 12),
        // To access PD for virt: PML4+PDPT indices from virt, rest recursive
        PageTableLevel::Two => (r << 39) | (r << 30) | (pml4_idx << 21) | (pdpt_idx << 12),
        // To access PT for virt: PML4+PDPT+PD indices from virt, recursive at top
        PageTableLevel::One => (r << 39) | (pml4_idx << 30) | (pdpt_idx << 21) | (pd_idx << 12),
    };

    // Sign-extend to make it a canonical higher-half address
    VirtAddr::new(0xFFFF_0000_0000_0000 | result)
}

/// Get a reference to the PML4 page table via recursive mapping.
///
/// # Safety
///
/// The recursive page table entry must have been set up before calling this.
pub fn pml4() -> &'static PageTable {
    unsafe { &*table_addr(VirtAddr::new(0), PageTableLevel::Four).as_ptr() }
}

/// Get a mutable reference to the PML4 page table via recursive mapping.
///
/// # Safety
///
/// The recursive page table entry must have been set up before calling this.
/// The caller must ensure no other references to the PML4 exist.
pub fn pml4_mut() -> &'static mut PageTable {
    unsafe { &mut *table_addr(VirtAddr::new(0), PageTableLevel::Four).as_mut_ptr() }
}

/// Get a reference to the page table at the given level for a virtual address.
///
/// # Safety
///
/// The recursive page table entry must have been set up, and the page table
/// at the given level must exist (i.e., the parent entry must be present).
pub unsafe fn table_for_addr(virt: VirtAddr, level: PageTableLevel) -> &'static PageTable {
    unsafe { &*table_addr(virt, level).as_ptr() }
}

/// Get a mutable reference to the page table at the given level for a virtual address.
///
/// # Safety
///
/// The recursive page table entry must have been set up, and the page table
/// at the given level must exist. The caller must ensure no other references exist.
pub unsafe fn table_for_addr_mut(virt: VirtAddr, level: PageTableLevel) -> &'static mut PageTable {
    unsafe { &mut *table_addr(virt, level).as_mut_ptr() }
}
