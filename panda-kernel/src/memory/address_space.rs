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

// Phase 2: Physical memory window creation will be implemented here.
// Phase 3: MMIO region allocator will be implemented here.
// Phase 4: PE relocation logic will be implemented here.
// Phase 5: Jump to higher-half will be implemented here.
// Phase 6: Identity mapping removal will be implemented here.
