pub mod device;

use ::acpi::sdt::mcfg::Mcfg;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spinning_top::RwSpinlock;
use x86_64::{PhysAddr, VirtAddr};

use crate::device_address::DeviceAddress;
use crate::memory::PhysicalMapping;

use device::PciDevice;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PciSegmentGroup {
    group_id: u16,
    base_address: VirtAddr,
    bus_number_start: u8,
    bus_number_end: u8,
}

impl PciSegmentGroup {
    pub fn device(&self, bus: u8, slot: u8, function: u8) -> Option<PciDevice> {
        let mut device = PciDevice::new(&self, bus, slot, 0);
        if device.vendor_id() == 0xffff {
            return None;
        }

        if function > 0 {
            if !device.is_multifunction() {
                return None;
            }

            device = PciDevice::new(&self, bus, slot, function);
            if device.vendor_id() == 0xffff {
                return None;
            }
        }

        Some(device)
    }
}

static PCI_SEGMENT_GROUPS: RwSpinlock<Vec<PciSegmentGroup>> = RwSpinlock::new(Vec::default());

/// ECAM mappings for PCI config space (persist for kernel lifetime).
static ECAM_MAPPINGS: RwSpinlock<Vec<PhysicalMapping>> = RwSpinlock::new(Vec::new());

/// Registry of PCI devices by class code.
/// Maps class code -> Vec of DeviceAddress in discovery order.
static DEVICES_BY_CLASS: RwSpinlock<BTreeMap<u8, Vec<DeviceAddress>>> =
    RwSpinlock::new(BTreeMap::new());

/// PCI device class with human-readable names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    Storage,
    Network,
    Display,
    Multimedia,
    Memory,
    Bridge,
    Serial,
    System,
    Input,
    Usb,
    Wireless,
}

impl DeviceClass {
    /// All known device classes.
    pub const ALL: &'static [DeviceClass] = &[
        DeviceClass::Storage,
        DeviceClass::Network,
        DeviceClass::Display,
        DeviceClass::Multimedia,
        DeviceClass::Memory,
        DeviceClass::Bridge,
        DeviceClass::Serial,
        DeviceClass::System,
        DeviceClass::Input,
        DeviceClass::Usb,
        DeviceClass::Wireless,
    ];

    /// Get the PCI class code for this device class.
    pub fn code(self) -> u8 {
        match self {
            DeviceClass::Storage => 0x01,
            DeviceClass::Network => 0x02,
            DeviceClass::Display => 0x03,
            DeviceClass::Multimedia => 0x04,
            DeviceClass::Memory => 0x05,
            DeviceClass::Bridge => 0x06,
            DeviceClass::Serial => 0x07,
            DeviceClass::System => 0x08,
            DeviceClass::Input => 0x09,
            DeviceClass::Usb => 0x0C,
            DeviceClass::Wireless => 0x0D,
        }
    }

    /// Get the human-readable name for this device class.
    pub fn name(self) -> &'static str {
        match self {
            DeviceClass::Storage => "storage",
            DeviceClass::Network => "network",
            DeviceClass::Display => "display",
            DeviceClass::Multimedia => "multimedia",
            DeviceClass::Memory => "memory",
            DeviceClass::Bridge => "bridge",
            DeviceClass::Serial => "serial",
            DeviceClass::System => "system",
            DeviceClass::Input => "input",
            DeviceClass::Usb => "usb",
            DeviceClass::Wireless => "wireless",
        }
    }

    /// Parse a device class from its name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "storage" => Some(DeviceClass::Storage),
            "network" => Some(DeviceClass::Network),
            "display" => Some(DeviceClass::Display),
            "multimedia" => Some(DeviceClass::Multimedia),
            "memory" => Some(DeviceClass::Memory),
            "bridge" => Some(DeviceClass::Bridge),
            "serial" => Some(DeviceClass::Serial),
            "system" => Some(DeviceClass::System),
            "input" => Some(DeviceClass::Input),
            "usb" => Some(DeviceClass::Usb),
            "wireless" => Some(DeviceClass::Wireless),
            _ => None,
        }
    }

    /// Get a device class from its PCI class code.
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0x01 => Some(DeviceClass::Storage),
            0x02 => Some(DeviceClass::Network),
            0x03 => Some(DeviceClass::Display),
            0x04 => Some(DeviceClass::Multimedia),
            0x05 => Some(DeviceClass::Memory),
            0x06 => Some(DeviceClass::Bridge),
            0x07 => Some(DeviceClass::Serial),
            0x08 => Some(DeviceClass::System),
            0x09 => Some(DeviceClass::Input),
            0x0C => Some(DeviceClass::Usb),
            0x0D => Some(DeviceClass::Wireless),
            _ => None,
        }
    }
}

/// Get the human-readable name for a PCI class code.
pub fn class_name(class_code: u8) -> Option<&'static str> {
    DeviceClass::from_code(class_code).map(|c| c.name())
}

/// Get the PCI class code from a human-readable name.
pub fn class_code(name: &str) -> Option<u8> {
    DeviceClass::from_name(name).map(|c| c.code())
}

/// Register a PCI device in the class registry.
fn register_device_class(class_code: u8, address: DeviceAddress) {
    let mut by_class = DEVICES_BY_CLASS.write();
    by_class.entry(class_code).or_default().push(address);
}

/// Get a device address by class and index.
pub fn get_device_by_class(class_code: u8, index: usize) -> Option<DeviceAddress> {
    let by_class = DEVICES_BY_CLASS.read();
    by_class.get(&class_code)?.get(index).cloned()
}

/// Get a device address by class name and index.
pub fn get_device_by_class_name(class_name: &str, index: usize) -> Option<DeviceAddress> {
    let code = class_code(class_name)?;
    get_device_by_class(code, index)
}

/// List all class codes that have devices.
pub fn list_device_classes() -> Vec<u8> {
    let by_class = DEVICES_BY_CLASS.read();
    by_class.keys().copied().collect()
}

/// Count devices in a class.
pub fn count_devices_in_class(class_code: u8) -> usize {
    let by_class = DEVICES_BY_CLASS.read();
    by_class.get(&class_code).map(|v| v.len()).unwrap_or(0)
}

/// List all device addresses in a class.
pub fn list_devices_in_class(class_code: u8) -> Vec<DeviceAddress> {
    let by_class = DEVICES_BY_CLASS.read();
    by_class.get(&class_code).cloned().unwrap_or_default()
}

pub fn init() {
    crate::acpi::with_table::<Mcfg>(|mcfg| {
        let mcfg = mcfg.expect("No MCFG table found");
        for entry in mcfg.entries() {
            // Calculate ECAM size: 4KB per function * 8 functions * 32 devices * bus_count
            // Cast to usize first to avoid u8 overflow on + 1
            let bus_count =
                (entry.bus_number_end as usize).saturating_sub(entry.bus_number_start as usize) + 1;
            let ecam_size = bus_count * 32 * 8 * 4096;

            // Map the ECAM config space
            let mapping = PhysicalMapping::new(PhysAddr::new(entry.base_address), ecam_size);
            let base_virt = mapping.virt_addr();
            // Store the mapping - PCI config space persists for kernel lifetime
            ECAM_MAPPINGS.write().push(mapping);

            init_pci_bus(
                base_virt,
                entry.bus_number_start,
                entry.bus_number_end,
                entry.pci_segment_group,
            );
        }
    });
}

fn init_pci_bus(
    base_address: VirtAddr,
    bus_number_start: u8,
    bus_number_end: u8,
    pci_segment_group: u16,
) {
    assert_eq!(get_pci_segment_group(pci_segment_group), None);

    {
        let pci_segment_groups = &mut *PCI_SEGMENT_GROUPS.write();
        pci_segment_groups.push(PciSegmentGroup {
            group_id: pci_segment_group,
            base_address,
            bus_number_start,
            bus_number_end,
        });
    }
}

pub fn enumerate_pci_devices(f: impl Fn(PciDevice)) {
    let pci_segment_groups = &*PCI_SEGMENT_GROUPS.read();

    for group in pci_segment_groups {
        for bus in group.bus_number_start..group.bus_number_end {
            for slot in 0..31 {
                let Some(pci_device) = pci_device(group.group_id, bus, slot, 0) else {
                    break;
                };

                // Register device in class registry
                let class = pci_device.class_code();
                let addr = pci_device.address();
                let device_address = DeviceAddress::Pci {
                    bus: addr.bus,
                    device: addr.slot,
                    function: addr.function,
                };
                register_device_class(class, device_address);

                f(pci_device)
            }
        }
    }
}

fn get_pci_segment_group(num: u16) -> Option<PciSegmentGroup> {
    let pci_segment_groups = &*PCI_SEGMENT_GROUPS.read();

    for group in pci_segment_groups {
        if group.group_id == num {
            return Some(*group);
        }
    }

    None
}

fn pci_device(group: u16, bus: u8, slot: u8, function: u8) -> Option<PciDevice> {
    get_pci_segment_group(group).and_then(|group| group.device(bus, slot, function))
}
