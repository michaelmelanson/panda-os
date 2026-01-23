//! Shared Virtio HAL implementation for all virtio devices.

use core::{alloc::Layout, ptr::NonNull};

use virtio_drivers::Hal;
use x86_64::PhysAddr;

use crate::memory::{global_alloc, map_mmio};

pub struct VirtioHal;

unsafe impl Hal for VirtioHal {
    fn dma_alloc(
        pages: usize,
        _direction: virtio_drivers::BufferDirection,
    ) -> (virtio_drivers::PhysAddr, NonNull<u8>) {
        let layout = Layout::from_size_align(pages * 4096, 4096).unwrap();
        let virt_addr = global_alloc::allocate(layout); // allocate uses alloc_zeroed

        (
            virt_addr.as_u64(),
            NonNull::new(virt_addr.as_u64() as *mut u8).unwrap(),
        )
    }

    unsafe fn dma_dealloc(
        _paddr: virtio_drivers::PhysAddr,
        _vaddr: NonNull<u8>,
        _pages: usize,
    ) -> i32 {
        // do nothing
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: virtio_drivers::PhysAddr, size: usize) -> NonNull<u8> {
        // Map MMIO region with identity mapping (virt = phys)
        let virt_addr = map_mmio(PhysAddr::new(paddr), size);
        let ptr: *mut u8 = virt_addr.as_mut_ptr();
        NonNull::new(ptr).expect("could not get MMIO virtual address")
    }

    unsafe fn share(
        buffer: NonNull<[u8]>,
        _direction: virtio_drivers::BufferDirection,
    ) -> virtio_drivers::PhysAddr {
        // nothing special to do here, since all data is shared and we identity map
        buffer.as_ptr() as *const () as u64
    }

    unsafe fn unshare(
        _paddr: virtio_drivers::PhysAddr,
        _buffer: NonNull<[u8]>,
        _direction: virtio_drivers::BufferDirection,
    ) {
        // do nothing
    }
}
