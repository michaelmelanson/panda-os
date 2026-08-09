//! PCI device registration for the userspace driver model.

use panda_abi::device::PciAddress;

use crate::device::{DEVICE_REGISTRY, DeviceInfo};
use crate::pci::{self, device::PciDevice};

/// Register every currently-enumerated PCI device with [`DEVICE_REGISTRY`],
/// posting `EVENT_DEVICE_ADDED` to any subscriber that already matches it.
///
/// Called once, after `pci::init()`, from `devices::init()`.
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

/// Find the live [`PciDevice`] (config-space accessor) for a `PciAddress`
/// recorded in [`DeviceInfo::Pci`], for the hardware-access
/// syscalls (`OP_DEVICE_MAP_MMIO`/`OP_DMA_ALLOC`/`OP_DEVICE_SUBSCRIBE_IRQ`),
/// which need to read BARs / configure MSI-X, not just the bus-agnostic
/// identity `DeviceRegistry` stores.
///
/// `enumerate_pci_devices` is the only way to recover a `PciDevice` from an
/// address today (there is no bus/slot/function-indexed lookup), so this
/// re-enumerates and matches — the same approach `register_enumerated_devices`
/// itself uses.
pub fn find_by_address(target: PciAddress) -> Option<PciDevice> {
    let found = core::cell::RefCell::new(None);
    pci::enumerate_pci_devices(|pci_device: PciDevice| {
        let addr = pci_device.address();
        if addr.bus == target.bus && addr.slot == target.device && addr.function == target.function
        {
            *found.borrow_mut() = Some(pci_device);
        }
    });
    found.into_inner()
}
