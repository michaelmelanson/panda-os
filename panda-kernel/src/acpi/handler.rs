use core::ptr::NonNull;

use x86_64::PhysAddr;

use crate::memory;

#[derive(Clone, Copy)]
pub struct AcpiHandler;

impl ::acpi::Handler for AcpiHandler {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> acpi::PhysicalMapping<Self, T> {
        let physical_start = PhysAddr::new(physical_address as u64);

        let virtual_start = unsafe {
            let virtual_start = memory::physical_address_to_virtual(physical_start);
            NonNull::new_unchecked(virtual_start.as_u64() as *mut _)
        };

        let region_length = size;
        let mapped_length = size;

        acpi::PhysicalMapping {
            physical_start: physical_start.as_u64() as usize,
            virtual_start,
            region_length,
            mapped_length,
            handler: *self,
        }
    }

    fn unmap_physical_region<T>(_region: &acpi::PhysicalMapping<Self, T>) {
        // nothing needed
    }

    fn read_u8(&self, _address: usize) -> u8 {
        todo!("read memory u8");
    }

    fn read_u16(&self, _address: usize) -> u16 {
        todo!("read memory u16");
    }

    fn read_u32(&self, _address: usize) -> u32 {
        todo!("read memory u32");
    }

    fn read_u64(&self, _address: usize) -> u64 {
        todo!("read memory u64")
    }

    fn write_u8(&self, _address: usize, _value: u8) {
        todo!("write memory u8");
    }

    fn write_u16(&self, _address: usize, _value: u16) {
        todo!("write memory u16");
    }

    fn write_u32(&self, _address: usize, _value: u32) {
        todo!("write memory u32");
    }

    fn write_u64(&self, _address: usize, _value: u64) {
        todo!("write memory u64");
    }

    fn read_io_u8(&self, _port: u16) -> u8 {
        todo!("read IO u8");
    }

    fn read_io_u16(&self, _port: u16) -> u16 {
        todo!("read IO u16");
    }

    fn read_io_u32(&self, _port: u16) -> u32 {
        todo!("read IO u32");
    }

    fn write_io_u8(&self, _port: u16, _value: u8) {
        todo!("write IO u8");
    }

    fn write_io_u16(&self, _port: u16, _value: u16) {
        todo!("write IO u16");
    }

    fn write_io_u32(&self, _port: u16, _value: u32) {
        todo!("write IO u32");
    }

    fn read_pci_u8(&self, _address: acpi::PciAddress, _offset: u16) -> u8 {
        todo!("read PCI u8");
    }

    fn read_pci_u16(&self, _address: acpi::PciAddress, _offset: u16) -> u16 {
        todo!("read PCI u16");
    }

    fn read_pci_u32(&self, _address: acpi::PciAddress, _offset: u16) -> u32 {
        todo!("read PCI u32");
    }

    fn write_pci_u8(&self, _address: acpi::PciAddress, _offset: u16, _value: u8) {
        todo!("write PCI u8");
    }

    fn write_pci_u16(&self, _address: acpi::PciAddress, _offset: u16, _value: u16) {
        todo!("write PCI u16");
    }

    fn write_pci_u32(&self, _address: acpi::PciAddress, _offset: u16, _value: u32) {
        todo!("write PCI u32");
    }

    fn nanos_since_boot(&self) -> u64 {
        todo!("nanos_since_boot");
    }

    fn stall(&self, _microseconds: u64) {
        todo!("stall");
    }

    fn sleep(&self, _milliseconds: u64) {
        todo!("sleep");
    }

    fn create_mutex(&self) -> acpi::Handle {
        todo!("create_mutex");
    }

    fn acquire(&self, _mutex: acpi::Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        todo!("acquire");
    }

    fn release(&self, _mutex: acpi::Handle) {
        todo!("release");
    }
}
