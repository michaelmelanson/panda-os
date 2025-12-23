mod virtio_gpu;
use log::debug;

use crate::pci::{self, device::PciDevice};

pub fn init() {
    pci::enumerate_pci_devices(|pci_device: PciDevice| {
        let address = pci_device.address();
        debug!(
            "PCI device {}: vendor={:#04X}, device={:#04X} class={:#002X}, subclass={:#02X}",
            address,
            pci_device.vendor_id(),
            pci_device.device_id(),
            pci_device.class_code(),
            pci_device.subclass()
        );

        match (pci_device.vendor_id(), pci_device.device_id()) {
            // Virtio GPU
            (0x1AF4, 0x1050) => virtio_gpu::init_from_pci_device(pci_device),
            _ => {}
        }
    });
}
