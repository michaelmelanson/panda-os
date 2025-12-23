use core::{alloc::Layout, ptr::NonNull};

use log::{debug, info, trace};
use virtio_drivers::{
    Hal,
    device::gpu::VirtIOGpu,
    transport::pci::{
        PciTransport,
        bus::{ConfigurationAccess, DeviceFunction, PciRoot},
    },
};
use x86_64::PhysAddr;

use crate::{
    memory::{self, global_alloc},
    pci::device::{PciDevice, PciDeviceAddress},
};

impl PartialEq<DeviceFunction> for PciDeviceAddress {
    fn eq(&self, other: &DeviceFunction) -> bool {
        self.bus == other.bus && self.slot == other.device && self.function == other.function
    }
}

impl ConfigurationAccess for PciDevice {
    fn read_word(
        &self,
        device_function: virtio_drivers::transport::pci::bus::DeviceFunction,
        register_offset: u8,
    ) -> u32 {
        trace!("Virtio PCI read {device_function} at offset {register_offset:#0X}");
        assert_eq!(self.address(), device_function);
        self.read(register_offset)
    }

    fn write_word(
        &mut self,
        device_function: virtio_drivers::transport::pci::bus::DeviceFunction,
        register_offset: u8,
        data: u32,
    ) {
        trace!(
            "Virtio PCI write {device_function} at offset {register_offset:#0X} with data {data:#0X}"
        );
        assert_eq!(self.address(), device_function);
        unsafe { self.write(register_offset, data) }
    }

    unsafe fn unsafe_clone(&self) -> Self {
        todo!()
    }
}

impl Into<DeviceFunction> for PciDeviceAddress {
    fn into(self) -> DeviceFunction {
        assert_eq!(self.segment, 0);

        DeviceFunction {
            bus: self.bus,
            device: self.slot,
            function: self.function,
        }
    }
}

struct VirtioHal;

unsafe impl Hal for VirtioHal {
    fn dma_alloc(
        pages: usize,
        _direction: virtio_drivers::BufferDirection,
    ) -> (virtio_drivers::PhysAddr, core::ptr::NonNull<u8>) {
        debug!("dma_alloc: pages={pages}");

        let layout = Layout::from_size_align(pages * 4096, 4096).unwrap();
        let virt_addr = global_alloc::allocate(layout);

        // nothing special to do here since all memory is available for DMA use
        (
            virt_addr.as_u64(),
            NonNull::new(virt_addr.as_u64() as *mut u8).unwrap(),
        )
    }

    unsafe fn dma_dealloc(
        _paddr: virtio_drivers::PhysAddr,
        _vaddr: core::ptr::NonNull<u8>,
        _pages: usize,
    ) -> i32 {
        // do nothing
        0
    }

    unsafe fn mmio_phys_to_virt(
        paddr: virtio_drivers::PhysAddr,
        _size: usize,
    ) -> core::ptr::NonNull<u8> {
        let phys_addr = PhysAddr::new(paddr);
        let virt_addr = memory::map_physical_address(phys_addr);
        let ptr: *mut u8 = virt_addr.as_mut_ptr();
        core::ptr::NonNull::new(ptr).expect("could not get MMIO virtual address")
    }

    unsafe fn share(
        buffer: core::ptr::NonNull<[u8]>,
        _direction: virtio_drivers::BufferDirection,
    ) -> virtio_drivers::PhysAddr {
        // nothing special to do here, since all data is shared and we identity map
        buffer.as_ptr() as *const () as u64
    }

    unsafe fn unshare(
        _paddr: virtio_drivers::PhysAddr,
        _buffer: core::ptr::NonNull<[u8]>,
        _direction: virtio_drivers::BufferDirection,
    ) {
        // do nothing
    }
}

pub fn init_from_pci_device(pci_device: PciDevice) {
    let mut root = PciRoot::new(pci_device.clone());
    let device_function: DeviceFunction = pci_device.address().into();
    let transport = PciTransport::new::<VirtioHal, PciDevice>(&mut root, device_function)
        .expect("Could not create PCI transport for Virtio GPU device");
    let mut gpu = VirtIOGpu::<VirtioHal, PciTransport>::new(transport)
        .expect("Could not initialize Virtio GPU device");

    let (width, height) = gpu.resolution().expect("failed to get resolution");

    info!("Allocating framebuffer...");
    let framebuffer = gpu
        .setup_framebuffer()
        .expect("Could not create framebuffer");
    info!("Framebuffer: {:?}", framebuffer.as_ptr());

    for y in 0..height {
        for x in 0..width {
            let index = ((y * width + x) * 4) as usize;

            framebuffer[index + 0] = 0xAA;
            framebuffer[index + 1] = 0xBB;
            framebuffer[index + 2] = 0xCC;
        }
    }

    gpu.flush().expect("flush failed");

    loop {}
}
