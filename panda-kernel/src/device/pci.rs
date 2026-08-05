//! PCI device registration for the userspace driver model.

use panda_abi::device::PciAddress;

use crate::device::{DEVICE_REGISTRY, DeviceInfo};
use crate::pci::{self, device::PciDevice};

/// Register every currently-enumerated PCI device with [`DEVICE_REGISTRY`],
/// posting `EVENT_DEVICE_ADDED` to any subscriber that already matches it.
///
/// Called once, after `pci::init()`, from `devices::init()`. This is
/// additive: it does not replace the existing kernel-resident driver
/// dispatch in `devices::init()` (virtio-blk/gpu/keyboard) — removing that
/// in favour of userspace drivers is Phase 7's atomic cutover, out of scope
/// here (Phases 1-5 are IOMMU-independent and explicitly do not touch
/// driver ownership).
pub fn register_enumerated_devices() {
    pci::enumerate_pci_devices(|pci_device: PciDevice| {
        let addr = pci_device.address();
        let info = DeviceInfo::Pci {
            address: PciAddress {
                segment: 0,
                bus: addr.bus,
                device: addr.slot,
                function: addr.function,
                _pad: [0; 3],
            },
            vendor_id: pci_device.vendor_id(),
            device_id: pci_device.device_id(),
            class: ((pci_device.class_code() as u32) << 8) | pci_device.subclass() as u32,
        };
        DEVICE_REGISTRY.lock().register(info);
    });
}
