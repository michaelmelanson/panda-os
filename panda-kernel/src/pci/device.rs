use core::fmt::UpperHex;

use alloc::vec::Vec;
use log::{debug, trace};
use spinning_top::Spinlock;
use x86_64::VirtAddr;

use crate::memory::PhysicalMapping;
use crate::pci::PciSegmentGroup;

/// PCI device MMIO mappings (MSI-X tables, virtio config, etc.) that persist for device lifetime.
pub(super) static PCI_DEVICE_MAPPINGS: Spinlock<Vec<PhysicalMapping>> = Spinlock::new(Vec::new());

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

    /// Get the capabilities pointer (offset 0x34, only valid if status bit 4 is set)
    pub fn capabilities_pointer(&self) -> u8 {
        self.read(0x34)
    }

    /// Check if the device has a capabilities list
    pub fn has_capabilities(&self) -> bool {
        self.status() & (1 << 4) != 0
    }

    /// Read a BAR (Base Address Register) value (low 32 bits only)
    pub fn bar(&self, index: u8) -> u32 {
        assert!(index < 6, "BAR index must be 0-5");
        self.read(0x10 + index * 4)
    }

    /// Read a BAR address, handling 64-bit BARs correctly.
    /// Returns the full 64-bit address with type bits masked off.
    pub fn bar_address(&self, index: u8) -> u64 {
        assert!(index < 6, "BAR index must be 0-5");
        let low = self.bar(index);

        // Check if this is a memory BAR (bit 0 = 0)
        if low & 1 != 0 {
            // I/O BAR - just return the address with type bit masked
            return (low & !0x3) as u64;
        }

        // Memory BAR - check type in bits 1-2
        let bar_type = (low >> 1) & 0x3;
        let base_addr = (low & !0xF) as u64;

        match bar_type {
            0b00 => base_addr, // 32-bit BAR
            0b10 => {
                // 64-bit BAR - read high 32 bits from next BAR
                assert!(index < 5, "64-bit BAR cannot start at BAR5");
                let high = self.bar(index + 1) as u64;
                base_addr | (high << 32)
            }
            _ => base_addr, // Reserved types, treat as 32-bit
        }
    }

    /// Find MSI-X capability and return its offset, or None if not present
    pub fn find_msix_capability(&self) -> Option<u8> {
        if !self.has_capabilities() {
            return None;
        }

        let mut cap_ptr = self.capabilities_pointer() & 0xFC; // Must be DWORD aligned
        while cap_ptr != 0 {
            let cap_id: u8 = self.read(cap_ptr);
            if cap_id == PCI_CAP_ID_MSIX {
                return Some(cap_ptr);
            }
            cap_ptr = self.read::<u8>(cap_ptr + 1) & 0xFC;
        }
        None
    }

    /// Get MSI-X capability information if present
    pub fn msix_capability(&self) -> Option<MsixCapability> {
        let offset = self.find_msix_capability()?;
        Some(MsixCapability::new(self, offset))
    }

    /// Configure and enable MSI-X for this device.
    ///
    /// Returns the configured MsixCapability on success.
    pub fn enable_msix(&self) -> Option<MsixCapability> {
        let mut cap = self.msix_capability()?;

        debug!(
            "PCI {}: Enabling MSI-X with {} vectors, table in BAR{} at offset {:#x}",
            self.address(),
            cap.table_size(),
            cap.table_bar(),
            cap.table_offset()
        );

        cap.enable();
        Some(cap)
    }
}

use super::msix::{MsixCapability, PCI_CAP_ID_MSIX};
