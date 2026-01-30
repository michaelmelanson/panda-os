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
use crate::resource::init_framebuffer;

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

    // Use the EDID preferred resolution (DTD1) if available.
    let (width, height) = gpu.edid_preferred_resolution().unwrap_or((1920, 1080));
    log::info!("Display resolution: {}x{}", width, height);

    let framebuffer = gpu
        .change_resolution(width, height)
        .expect("Could not create framebuffer");
    let framebuffer = VirtAddr::new(framebuffer.as_ptr() as u64);

    // Initialize the framebuffer surface for userspace access
    unsafe {
        init_framebuffer(framebuffer.as_mut_ptr(), width, height);
    }

    // Initialize compositor with a framebuffer surface
    if let Some(surface) = crate::resource::get_framebuffer_surface() {
        crate::compositor::init(*surface);
    }

    let mut device = VIRTIO_GPU_DEVICE.write();
    *device = Some(VirtioGpuDevice {
        gpu,
        framebuffer,
        resolution: (width, height),
    });
}

/// Flush the framebuffer to the display.
pub fn flush_framebuffer() {
    let mut device = VIRTIO_GPU_DEVICE.write();
    if let Some(ref mut dev) = *device {
        dev.gpu.flush().ok();
    }
}

/// Change the display resolution at runtime.
///
/// Tears down the existing GPU framebuffer resource, creates a new one at the
/// specified dimensions, and updates the global framebuffer surface and compositor.
pub fn change_resolution(width: u32, height: u32) -> Result<(), &'static str> {
    let mut device = VIRTIO_GPU_DEVICE.write();
    let dev = device.as_mut().ok_or("GPU not initialized")?;

    let framebuffer = dev
        .gpu
        .change_resolution(width, height)
        .map_err(|_| "GPU resolution change failed")?;
    let framebuffer_ptr = framebuffer.as_mut_ptr();
    dev.framebuffer = VirtAddr::new(framebuffer_ptr as u64);
    dev.resolution = (width, height);

    unsafe {
        init_framebuffer(framebuffer_ptr, width, height);
    }

    let new_surface = crate::resource::get_framebuffer_surface()
        .ok_or("Failed to get new framebuffer surface")?;
    crate::compositor::replace_framebuffer(*new_surface);

    Ok(())
}
