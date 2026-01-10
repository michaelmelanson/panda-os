use log::trace;
use spinning_top::RwSpinlock;
use virtio_drivers::{
    device::gpu::VirtIOGpu,
    transport::pci::{
        PciTransport,
        bus::{ConfigurationAccess, DeviceFunction, PciRoot},
    },
};
use x86_64::VirtAddr;

use crate::pci::device::{PciDevice, PciDeviceAddress};

use super::virtio_hal::VirtioHal;

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

#[allow(unused)]
struct VirtioGpuDevice {
    gpu: VirtIOGpu<VirtioHal, PciTransport>,
    framebuffer: VirtAddr,
    resolution: (u32, u32),
}

static VIRTIO_GPU_DEVICE: RwSpinlock<Option<VirtioGpuDevice>> = RwSpinlock::new(None);

pub fn init_from_pci_device(pci_device: PciDevice) {
    let mut root = PciRoot::new(pci_device.clone());
    let device_function: DeviceFunction = pci_device.address().into();
    let transport = PciTransport::new::<VirtioHal, PciDevice>(&mut root, device_function)
        .expect("Could not create PCI transport for Virtio GPU device");
    let mut gpu = VirtIOGpu::<VirtioHal, PciTransport>::new(transport)
        .expect("Could not initialize Virtio GPU device");

    let (width, height) = gpu.resolution().expect("failed to get resolution");

    let framebuffer = gpu
        .setup_framebuffer()
        .expect("Could not create framebuffer");
    let framebuffer = VirtAddr::new(framebuffer.as_ptr() as u64);

    let mut device = VIRTIO_GPU_DEVICE.write();
    *device = Some(VirtioGpuDevice {
        gpu,
        framebuffer,
        resolution: (width, height),
    });
}
