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

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

use goblin::pe::PE;
use log::{debug, info};
use uefi::mem::memory_map::MemoryMapOwned;
use x86_64::structures::paging::page_table::PageTableLevel;
use x86_64::structures::paging::{PageTable, PageTableFlags, PhysFrame};
use x86_64::{PhysAddr, VirtAddr};

use crate::uefi::KernelImageInfo;

use super::allocate_frame_raw;
use super::paging::current_page_table;
use super::write_protection::without_write_protection;
use super::recursive::RECURSIVE_INDEX;

/// Early boot frame allocator - uses a bump allocator from the end of the heap region.
/// This is used before the heap is initialized to allocate page tables for mapping the heap.
static EARLY_FRAME_ALLOC_PTR: AtomicU64 = AtomicU64::new(0);
static EARLY_FRAME_ALLOC_END: AtomicU64 = AtomicU64::new(0);

/// Initialize the early boot frame allocator.
/// Reserves space at the end of the heap physical region for early page tables.
fn init_early_frame_allocator(heap_phys_end: u64) {
    // Reserve 2MB (512 frames) at the end of the heap for early page tables.
    // This is enough for ~470 page table pages needed to map a 931MB heap with 4KB pages.
    const EARLY_RESERVE: u64 = 2 * 1024 * 1024;
    let start = heap_phys_end - EARLY_RESERVE;
    EARLY_FRAME_ALLOC_PTR.store(start, Ordering::SeqCst);
    EARLY_FRAME_ALLOC_END.store(heap_phys_end, Ordering::SeqCst);
}

/// Allocate a frame during early boot (before heap is available).
/// Returns the physical address of a zeroed 4KB frame.
fn allocate_early_frame() -> PhysFrame {
    let ptr = EARLY_FRAME_ALLOC_PTR.fetch_add(4096, Ordering::SeqCst);
    let end = EARLY_FRAME_ALLOC_END.load(Ordering::SeqCst);
    assert!(ptr + 4096 <= end, "early frame allocator exhausted");

    // Zero the frame (using identity mapping which is still active)
    unsafe {
        core::ptr::write_bytes(ptr as *mut u8, 0, 4096);
    }

    PhysFrame::from_start_address(PhysAddr::new(ptr)).unwrap()
}

/// Stored kernel image physical base address for address translation.
static KERNEL_IMAGE_BASE_PHYS: AtomicU64 = AtomicU64::new(0);

/// Get the physical base address of the kernel image.
/// Returns 0 if the kernel hasn't been relocated yet.
pub fn get_kernel_image_phys_base() -> u64 {
    KERNEL_IMAGE_BASE_PHYS.load(Ordering::SeqCst)
}

/// Base address of the MMIO region.
/// Device memory-mapped I/O is allocated starting at this address.
pub const MMIO_REGION_BASE: u64 = 0xffff_9000_0000_0000;

/// Base address of the kernel heap region.
pub const KERNEL_HEAP_BASE: u64 = 0xffff_a000_0000_0000;

/// Base address for the relocated kernel image.
pub const KERNEL_IMAGE_BASE: u64 = 0xffff_c000_0000_0000;

/// Upper bound of userspace addresses (lower canonical half).
/// Addresses must be < USER_ADDR_MAX to be valid userspace addresses.
/// Imported from syscall::user_ptr.
pub const USER_ADDR_MAX: u64 = 0x0000_7fff_ffff_ffff;

/// Map the kernel heap region at KERNEL_HEAP_BASE.
///
/// This maps the given physical memory to virtual addresses starting at `KERNEL_HEAP_BASE`,
/// using 2MB huge pages where possible for efficiency.
///
/// # Safety
/// Must be called exactly once during early kernel initialization, before
/// the heap allocator is initialized.
pub unsafe fn map_heap_region(phys_base: u64, size: u64) {
    // Initialize early frame allocator - reserve space at end of heap for page tables
    init_early_frame_allocator(phys_base + size);

    without_write_protection(|| {
        let pml4 = unsafe { &mut *current_page_table() };

        // Map heap memory in 4KB pages (heap physical base may not be 2MB aligned)
        let mut offset = 0u64;
        while offset < size {
            let virt_addr = VirtAddr::new(KERNEL_HEAP_BASE + offset);
            let phys_addr = phys_base + offset;

            // Get PML4 entry
            let pml4_index = virt_addr.page_table_index(PageTableLevel::Four);
            let pml4_entry = &mut pml4[pml4_index];
            let pml3 = if pml4_entry.flags().contains(PageTableFlags::PRESENT) {
                unsafe { &mut *(pml4_entry.addr().as_u64() as *mut PageTable) }
            } else {
                let frame = allocate_early_frame();
                let table = frame.start_address().as_u64() as *mut PageTable;
                pml4_entry.set_addr(
                    frame.start_address(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                );
                unsafe { &mut *table }
            };

            // Get PML3 entry
            let pml3_index = virt_addr.page_table_index(PageTableLevel::Three);
            let pml3_entry = &mut pml3[pml3_index];
            let pml2 = if pml3_entry.flags().contains(PageTableFlags::PRESENT) {
                unsafe { &mut *(pml3_entry.addr().as_u64() as *mut PageTable) }
            } else {
                let frame = allocate_early_frame();
                let table = frame.start_address().as_u64() as *mut PageTable;
                pml3_entry.set_addr(
                    frame.start_address(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                );
                unsafe { &mut *table }
            };

            // Get PML2 entry
            let pml2_index = virt_addr.page_table_index(PageTableLevel::Two);
            let pml2_entry = &mut pml2[pml2_index];
            let pml1 = if pml2_entry.flags().contains(PageTableFlags::PRESENT) {
                unsafe { &mut *(pml2_entry.addr().as_u64() as *mut PageTable) }
            } else {
                let frame = allocate_early_frame();
                let table = frame.start_address().as_u64() as *mut PageTable;
                pml2_entry.set_addr(
                    frame.start_address(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                );
                unsafe { &mut *table }
            };

            // Map 4KB page at PML1 level
            let pml1_index = virt_addr.page_table_index(PageTableLevel::One);
            let pml1_entry = &mut pml1[pml1_index];
            pml1_entry.set_addr(
                PhysAddr::new(phys_addr),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            );

            offset += 4096;
        }

        // Flush TLB
        x86_64::instructions::tlb::flush_all();
    });
}

/// Set up the recursive page table entry.
///
/// PML4[510] is set to point back to the PML4 itself. This enables accessing
/// all page tables at calculated virtual addresses without needing a physical
/// memory window for page table walking.
///
/// # Safety
/// Must be called while identity mapping is still active (before `remove_identity_mapping`).
unsafe fn setup_recursive_page_table() {
    use x86_64::registers::control::Cr3;

    let pml4_phys = Cr3::read().0.start_address();

    // Use identity mapping (still available at this point) to access PML4
    let pml4 = pml4_phys.as_u64() as *mut PageTable;

    without_write_protection(|| unsafe {
        (&mut *pml4)[RECURSIVE_INDEX].set_addr(
            pml4_phys,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
        );
    });

    x86_64::instructions::tlb::flush_all();
    info!(
        "Recursive page table mapping established at PML4[{}]",
        RECURSIVE_INDEX
    );
}

/// Initialize the higher-half address space.
///
/// Sets up recursive page tables for page table walking. No physical memory
/// window is created - all physical memory access is done through RAII
/// `PhysicalMapping` wrappers or heap-backed frames.
///
/// # Safety
/// Must be called exactly once during early kernel initialization.
pub unsafe fn init(_memory_map: &MemoryMapOwned) {
    // Set up recursive page tables (uses identity mapping which is still active)
    unsafe {
        setup_recursive_page_table();
    }

    info!("Higher-half address space initialized with recursive page tables");
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

    // Use permissive parse mode since we're parsing an in-memory image
    let opts = goblin::pe::options::ParseOptions::default()
        .with_parse_mode(goblin::options::ParseMode::Permissive);
    let pe = PE::parse_with_opts(image_slice, &opts).expect("failed to parse kernel PE");

    let pe_image_base = pe.image_base as u64;
    let delta = higher_half_base.as_u64() as i64 - pe_image_base as i64;

    // Get the base relocation directory from the data directories
    // We need to manually access it because goblin's parse uses file offsets,
    // but we have an in-memory (loaded) image where RVA == offset from image base
    let reloc_dir = pe
        .header
        .optional_header
        .as_ref()
        .and_then(|opt| opt.data_directories.get_base_relocation_table())
        .expect("kernel PE should have base relocation directory");

    let reloc_rva = reloc_dir.virtual_address as usize;
    let reloc_size = reloc_dir.size as usize;

    if reloc_size == 0 {
        return;
    }

    // In a loaded image, RVA is the offset from image base
    let reloc_bytes = &image_slice[reloc_rva..reloc_rva + reloc_size];

    let mut reloc_count = 0;
    let mut highlow_count = 0;
    let mut block_count = 0;

    // Parse relocation blocks manually
    // Each block starts with: u32 page_rva, u32 block_size
    // Followed by u16 entries until block_size is reached
    let mut offset = 0;
    while offset + 8 <= reloc_bytes.len() {
        let page_rva = u32::from_le_bytes([
            reloc_bytes[offset],
            reloc_bytes[offset + 1],
            reloc_bytes[offset + 2],
            reloc_bytes[offset + 3],
        ]) as u64;
        let block_size = u32::from_le_bytes([
            reloc_bytes[offset + 4],
            reloc_bytes[offset + 5],
            reloc_bytes[offset + 6],
            reloc_bytes[offset + 7],
        ]) as usize;

        if block_size < 8 || offset + block_size > reloc_bytes.len() {
            break; // End of relocations or invalid block
        }

        block_count += 1;

        // Process relocation entries (each is 2 bytes)
        let entries_start = offset + 8;
        let entries_end = offset + block_size;
        let mut entry_offset = entries_start;

        while entry_offset + 2 <= entries_end {
            let entry =
                u16::from_le_bytes([reloc_bytes[entry_offset], reloc_bytes[entry_offset + 1]]);
            entry_offset += 2;

            let reloc_type = (entry >> 12) as u16;
            let reloc_offset = (entry & 0xFFF) as u64;

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

        // Move to next block
        offset += block_size;
    }

    info!(
        "Applied {} DIR64 relocations, {} HIGHLOW relocations in {} blocks",
        reloc_count, highlow_count, block_count
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
    debug!("Relocating kernel to higher half...");

    // Map kernel pages to higher-half addresses
    let higher_half_base = unsafe { map_kernel_to_higher_half(kernel_info) };

    // Apply PE relocations to fix absolute addresses
    unsafe { apply_kernel_relocations(kernel_info, higher_half_base) };

    // Verify relocation by reading a known value before jumping.
    // Use the KERNEL_IMAGE_BASE constant itself as a sanity check.
    let identity_ptr = &KERNEL_IMAGE_BASE as *const u64;
    let higher_half_ptr = (higher_half_base.as_u64()
        + (identity_ptr as u64 - kernel_info.image_base as u64))
        as *const u64;

    let higher_half_value = unsafe { *higher_half_ptr };

    // The higher-half value should equal KERNEL_IMAGE_BASE (the constant's value)
    // since that constant should have been relocated to point to higher-half
    assert_eq!(
        higher_half_value, KERNEL_IMAGE_BASE,
        "relocation verification failed"
    );

    // Store kernel image base for use during higher-half jump
    KERNEL_IMAGE_BASE_PHYS.store(kernel_info.image_base as u64, Ordering::SeqCst);

    higher_half_base
}

/// Convert an identity-mapped kernel address to its higher-half equivalent.
///
/// # Safety
/// The address must be within the kernel image, and `relocate_kernel_to_higher_half`
/// must have been called first.
pub unsafe fn identity_to_higher_half(identity_addr: u64) -> u64 {
    // If the address is already in higher-half (kernel region), return as-is
    // This can happen after relocation when static variables already point to higher-half
    if identity_addr >= KERNEL_IMAGE_BASE {
        return identity_addr;
    }

    let image_base = KERNEL_IMAGE_BASE_PHYS.load(Ordering::SeqCst);
    assert!(image_base != 0, "kernel not relocated yet");

    let offset = identity_addr - image_base;
    KERNEL_IMAGE_BASE + offset
}

/// Jump to higher-half kernel execution.
///
/// This function:
/// 1. Calculates the higher-half address of the boot stack
/// 2. Switches RSP to the higher-half stack
/// 3. Jumps to the continuation function at its higher-half address
///
/// The continuation function must reinitialize GDT/TSS with higher-half stack addresses.
///
/// # Safety
/// - `relocate_kernel_to_higher_half` must have been called first
/// - The `boot_stack_top` must be the identity-mapped address of a valid stack top
/// - The continuation function must never return
pub unsafe fn jump_to_higher_half(
    boot_stack_top: u64,
    continuation: unsafe extern "C" fn() -> !,
) -> ! {
    // Calculate higher-half addresses
    let higher_half_stack = unsafe { identity_to_higher_half(boot_stack_top) };
    let higher_half_continuation = unsafe { identity_to_higher_half(continuation as u64) };

    info!(
        "Jumping to higher half: stack {:#x} -> {:#x}, continuation {:#x} -> {:#x}",
        boot_stack_top, higher_half_stack, continuation as u64, higher_half_continuation
    );

    // Switch stack and jump to higher-half
    // The x86-64 ABI requires RSP % 16 == 8 on function entry (after call pushes return address).
    // Since we use jmp instead of call, we need to subtract 8 to simulate the pushed return address.
    unsafe {
        asm!(
            "mov rsp, {new_stack}",
            "sub rsp, 8",  // Simulate pushed return address for ABI compliance
            "jmp {continuation}",
            new_stack = in(reg) higher_half_stack,
            continuation = in(reg) higher_half_continuation,
            options(noreturn)
        );
    }
}

/// Remove identity mappings from the kernel page table.
///
/// After this function returns, only higher-half addresses (0xffff_8000_0000_0000+)
/// are valid for kernel use. The entire lower half (PML4 entries 0-255) is cleared.
///
/// # Safety
/// - Must only be called after `jump_to_higher_half` has been executed
/// - All code must be running from higher-half addresses
/// - No references to identity-mapped addresses may exist
pub unsafe fn remove_identity_mapping() {
    let pml4 = unsafe { &mut *current_page_table() };

    // Clear all lower-half entries (0-255)
    // These contain the UEFI identity mappings that we no longer need
    for i in 0..256 {
        pml4[i].set_unused();
    }

    // Flush TLB to ensure stale mappings are removed
    x86_64::instructions::tlb::flush_all();

    debug!("Identity mapping removed, kernel running entirely in higher half");
}
