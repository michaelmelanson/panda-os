mod virtio_hal;

use log::debug;

use crate::device;
use crate::pci::{self, device::PciDevice};

pub fn init() {
    // Register enumerated devices with the device driver model's registry
    // (see plans/device-driver-model.md).
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
            // Virtio Block (legacy device ID 0x1001, modern transitional
            // 0x1042): served by a userspace driver
            // (userspace/drivers/virtio-blk), claimed via the
            // OP_DEVICE_SUBSCRIBE/OP_DEVICE_CLAIM model. No kernel-resident
            // init for this device.
            (0x1AF4, 0x1001, _) | (0x1AF4, 0x1042, _) => {}
            // Virtio GPU: served by a userspace driver
            // (userspace/drivers/virtio-gpu), claimed via the
            // OP_DEVICE_SUBSCRIBE/OP_DEVICE_CLAIM model. No kernel-resident
            // init for this device.
            (0x1AF4, 0x1050, _) => {}
            // Virtio Input - keyboard (subclass 0x00): served by a userspace
            // driver (userspace/drivers/virtio-keyboard), claimed via the
            // OP_DEVICE_SUBSCRIBE/OP_DEVICE_CLAIM model. No kernel-resident
            // init for this device.
            (0x1AF4, 0x1052, 0x00) => {}
            // Virtio Input - mouse (subclass 0x02) - skip for now
            (0x1AF4, 0x1052, 0x02) => {}
            _ => {}
        }
    });
}
