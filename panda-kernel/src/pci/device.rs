use core::fmt::UpperHex;

use log::trace;
use x86_64::VirtAddr;

use crate::pci::PciSegmentGroup;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PciDeviceAddress {
    pub segment: u16,
    pub bus: u8,
    pub slot: u8,
    pub function: u8,
}

impl core::fmt::Display for PciDeviceAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!(
            "{:02X}:{:02X}:{:02X}:{:02X}",
            self.segment, self.bus, self.slot, self.function
        ))
    }
}

#[derive(Clone)]
pub struct PciDevice(VirtAddr, PciDeviceAddress);

#[allow(unused)]
impl PciDevice {
    pub fn new(pci_segment_group: &PciSegmentGroup, bus: u8, slot: u8, function: u8) -> Self {
        let base_address = pci_segment_group.base_address
            + (((bus as u64 * 256) + (slot as u64 * 8) + function as u64) * 4096);
        let device_address = PciDeviceAddress {
            segment: pci_segment_group.group_id,
            bus,
            slot,
            function,
        };

        PciDevice(base_address, device_address)
    }

    pub fn address(&self) -> PciDeviceAddress {
        self.1
    }

    pub fn read<T: Clone + Copy>(&self, offset: u8) -> T {
        let addr = self.0 + offset.into();
        trace!(
            "PCI read: device={}, offset={offset:#04X}, addr={addr:#0X}",
            self.address()
        );
        unsafe { *addr.as_ptr::<T>() }
    }

    pub unsafe fn write<T: UpperHex>(&self, offset: u8, data: T) {
        let addr = self.0 + offset.into();
        trace!(
            "PCI write: device={}, offset={offset:#04X}, addr={addr:#0X}, data={data:#0X}",
            self.address()
        );

        unsafe { *addr.as_mut_ptr::<T>() = data }
    }

    pub fn vendor_id(&self) -> u16 {
        self.read(0x00)
    }
    pub fn device_id(&self) -> u16 {
        self.read(0x02)
    }
    pub fn command(&self) -> u16 {
        self.read(0x04)
    }
    pub fn status(&self) -> u16 {
        self.read(0x06)
    }
    pub fn revision_id(&self) -> u8 {
        self.read(0x08)
    }
    pub fn prog_if(&self) -> u8 {
        self.read(0x09)
    }
    pub fn subclass(&self) -> u8 {
        self.read(0x0A)
    }
    pub fn class_code(&self) -> u8 {
        self.read(0x0B)
    }
    pub fn cache_line_size(&self) -> u8 {
        self.read(0x0C)
    }
    pub fn latency_timer(&self) -> u8 {
        self.read(0x0D)
    }
    pub fn header_type(&self) -> u8 {
        self.read(0x0E)
    }
    pub fn bist(&self) -> u8 {
        self.read(0x0F)
    }

    pub fn is_multifunction(&self) -> bool {
        self.header_type() & 0x80 != 0
    }

    /// Get the interrupt line (legacy PCI interrupt)
    pub fn interrupt_line(&self) -> u8 {
        self.read(0x3C)
    }

    /// Get the interrupt pin (INTA=1, INTB=2, INTC=3, INTD=4, 0=none)
    pub fn interrupt_pin(&self) -> u8 {
        self.read(0x3D)
    }
}
