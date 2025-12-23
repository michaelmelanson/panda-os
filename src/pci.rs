pub mod device;

use ::acpi::sdt::mcfg::Mcfg;
use alloc::vec::Vec;
use spinning_top::RwSpinlock;
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::map_physical_address;

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

pub fn init() {
    crate::acpi::with_table::<Mcfg>(|mcfg| {
        let mcfg = mcfg.expect("No MCFG table found");
        for entry in mcfg.entries() {
            init_pci_bus(
                map_physical_address(PhysAddr::new(entry.base_address)),
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
