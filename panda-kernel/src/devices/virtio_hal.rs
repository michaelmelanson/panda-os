//! Shared Virtio HAL implementation for all virtio devices.

use core::{alloc::Layout, ptr::NonNull};

use alloc::collections::BTreeMap;
use spinning_top::Spinlock;
use virtio_drivers::Hal;
use x86_64::PhysAddr;

use crate::memory::{
    Frame, KERNEL_IMAGE_BASE, MmioMapping, allocate_physical, get_kernel_image_phys_base,
    physical_address_to_virtual, virtual_address_to_physical,
};

/// Track leaked DMA frames so we can reclaim them on dealloc.
static LEAKED_FRAMES: Spinlock<BTreeMap<u64, Frame>> = Spinlock::new(BTreeMap::new());

fn leak_frame(paddr: u64, frame: Frame) {
    LEAKED_FRAMES.lock().insert(paddr, frame);
}

fn reclaim_frame(paddr: u64) {
    // Remove from map - Frame will be dropped and memory deallocated
    LEAKED_FRAMES.lock().remove(&paddr);
}

pub struct VirtioHal;

unsafe impl Hal for VirtioHal {
    fn dma_alloc(
        pages: usize,
        _direction: virtio_drivers::BufferDirection,
    ) -> (virtio_drivers::PhysAddr, NonNull<u8>) {
        let layout = Layout::from_size_align(pages * 4096, 4096).unwrap();

        // Allocate using the RAII Frame wrapper
        let frame = allocate_physical(layout);
        let phys_addr = frame.start_address();

        // Get the virtual address via the physical memory window
        let virt_addr = physical_address_to_virtual(phys_addr);
        let ptr = virt_addr.as_mut_ptr::<u8>();

        // Store the Frame so it can be reclaimed on dealloc
        leak_frame(phys_addr.as_u64(), frame);

        (phys_addr.as_u64(), NonNull::new(ptr).unwrap())
    }

    unsafe fn dma_dealloc(
        paddr: virtio_drivers::PhysAddr,
        _vaddr: NonNull<u8>,
        _pages: usize,
    ) -> i32 {
        // Resurrect the Frame and drop it to deallocate
        reclaim_frame(paddr);
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: virtio_drivers::PhysAddr, size: usize) -> NonNull<u8> {
        // Map MMIO region to higher-half MMIO region
        let mmio = MmioMapping::new(PhysAddr::new(paddr), size);
        let ptr: *mut u8 = mmio.virt_addr().as_mut_ptr();
        // Leak the mapping - virtio expects it to persist
        core::mem::forget(mmio);
        NonNull::new(ptr).expect("could not get MMIO virtual address")
    }

    unsafe fn share(
        buffer: NonNull<[u8]>,
        _direction: virtio_drivers::BufferDirection,
    ) -> virtio_drivers::PhysAddr {
        // Convert virtual address to physical address for device access
        let virt_addr = x86_64::VirtAddr::new(buffer.as_ptr() as *const () as u64);
        let addr = virt_addr.as_u64();

        let phys_map_base = crate::memory::get_phys_map_base();
        let kernel_image_phys = get_kernel_image_phys_base();

        let phys_addr = if addr >= KERNEL_IMAGE_BASE && kernel_image_phys != 0 {
            // Address is in the relocated kernel image region
            // Map back to physical: phys = kernel_phys_base + (virt - KERNEL_IMAGE_BASE)
            let offset = addr - KERNEL_IMAGE_BASE;
            PhysAddr::new(kernel_image_phys + offset)
        } else if addr >= phys_map_base && phys_map_base != 0 {
            // Address is in the physical memory window
            virtual_address_to_physical(virt_addr)
        } else {
            // Identity-mapped address (physical == virtual)
            PhysAddr::new(addr)
        };

        phys_addr.as_u64()
    }

    unsafe fn unshare(
        _paddr: virtio_drivers::PhysAddr,
        _buffer: NonNull<[u8]>,
        _direction: virtio_drivers::BufferDirection,
    ) {
        // do nothing
    }
}
