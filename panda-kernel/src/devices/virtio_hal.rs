//! Shared Virtio HAL implementation for all virtio devices.

use core::{alloc::Layout, ptr::NonNull};

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spinning_top::Spinlock;
use virtio_drivers::Hal;
use x86_64::PhysAddr;

use crate::memory::{Frame, PhysicalMapping, allocate_physical, virtual_address_to_physical};

/// Track DMA frames by physical address so we can reclaim them on dealloc.
static DMA_FRAMES: Spinlock<BTreeMap<u64, Frame>> = Spinlock::new(BTreeMap::new());

/// Track MMIO mappings (VirtIO HAL has no unmap, so these persist for device lifetime).
static MMIO_MAPPINGS: Spinlock<Vec<PhysicalMapping>> = Spinlock::new(Vec::new());

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

        // Use the frame's heap virtual address directly - avoids physical window aliasing
        let virt_addr = frame.virtual_address();
        let ptr = virt_addr.as_mut_ptr::<u8>();

        // Store the Frame so it can be reclaimed on dealloc
        DMA_FRAMES.lock().insert(phys_addr.as_u64(), frame);

        (phys_addr.as_u64(), NonNull::new(ptr).unwrap())
    }

    unsafe fn dma_dealloc(
        paddr: virtio_drivers::PhysAddr,
        _vaddr: NonNull<u8>,
        _pages: usize,
    ) -> i32 {
        // Remove and drop the Frame to deallocate
        DMA_FRAMES.lock().remove(&paddr);
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: virtio_drivers::PhysAddr, size: usize) -> NonNull<u8> {
        // Map MMIO region to higher-half MMIO region
        let mapping = PhysicalMapping::new(PhysAddr::new(paddr), size);
        let ptr: *mut u8 = mapping.virt_addr().as_mut_ptr();

        // Store mapping - VirtIO HAL has no unmap, so these persist for device lifetime
        MMIO_MAPPINGS.lock().push(mapping);

        NonNull::new(ptr).expect("could not get MMIO virtual address")
    }

    unsafe fn share(
        buffer: NonNull<[u8]>,
        _direction: virtio_drivers::BufferDirection,
    ) -> virtio_drivers::PhysAddr {
        // Convert virtual address to physical address for device access
        let virt_addr = x86_64::VirtAddr::new(buffer.as_ptr() as *const () as u64);

        // Use virtual_address_to_physical which handles all higher-half regions
        // (heap, kernel image, MMIO)
        let phys_addr = virtual_address_to_physical(virt_addr);
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
