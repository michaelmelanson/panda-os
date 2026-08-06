pub mod claims;
pub mod virtio_block;
pub mod virtio_gpu;
mod virtio_hal;

use log::debug;

use crate::device;
use crate::pci::{self, device::PciDevice};

pub fn init() {
    // Register enumerated devices with the device driver model's registry
    // (see plans/device-driver-model.md) alongside the existing
    // kernel-resident driver dispatch below.
    device::pci::register_enumerated_devices();

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

        match (
            pci_device.vendor_id(),
            pci_device.device_id(),
            pci_device.subclass(),
        ) {
            // Virtio Block (legacy device ID 0x1001, modern transitional 0x1042)
            (0x1AF4, 0x1001, _) | (0x1AF4, 0x1042, _) => {
                virtio_block::init_from_pci_device(pci_device)
            }
            // Virtio GPU
            (0x1AF4, 0x1050, _) => virtio_gpu::init_from_pci_device(pci_device),
            // Virtio Input - keyboard (subclass 0x00): moved to a userspace
            // driver (userspace/drivers/virtio-keyboard) — see
            // plans/device-driver-model.md Phase 7. No kernel-resident init
            // for this device anymore; it is claimed via the
            // OP_DEVICE_SUBSCRIBE/OP_DEVICE_CLAIM model instead.
            (0x1AF4, 0x1052, 0x00) => {}
            // Virtio Input - mouse (subclass 0x02) - skip for now
            (0x1AF4, 0x1052, 0x02) => {}
            _ => {}
        }
    });
}
