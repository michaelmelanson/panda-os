//! MSI-X (Message Signaled Interrupts - Extended) support for PCI devices.
//!
//! MSI-X provides per-device, per-queue interrupt vectors that avoid the
//! limitations of legacy PCI interrupts (shared IRQ lines, limited vectors).

use log::debug;
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::PhysicalMapping;

use super::device::{PCI_DEVICE_MAPPINGS, PciDevice};

/// PCI Capability ID for MSI-X
pub const PCI_CAP_ID_MSIX: u8 = 0x11;

/// MSI-X Message Control register bits
const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_FUNCTION_MASK: u16 = 1 << 14;
const MSIX_TABLE_SIZE_MASK: u16 = 0x07FF;

/// MSI-X table entry vector control bits
const MSIX_ENTRY_MASKED: u32 = 1 << 0;

/// MSI-X table entry offsets (each entry is 16 bytes)
const MSIX_ENTRY_MSG_ADDR: u64 = 0;
const MSIX_ENTRY_MSG_DATA: u64 = 8;
const MSIX_ENTRY_VECTOR_CTRL: u64 = 12;

/// MSI-X Capability structure.
///
/// Represents the MSI-X capability of a PCI device, providing access to
/// the MSI-X table for configuring interrupt vectors.
#[derive(Clone)]
pub struct MsixCapability {
    /// The PCI device this capability belongs to
    device: PciDevice,
    /// Offset of this capability in config space
    cap_offset: u8,
    /// Virtual address of the MSI-X table
    table_vaddr: VirtAddr,
    /// Number of table entries
    table_size: u16,
}

impl MsixCapability {
    /// Create a new MSI-X capability from a device and capability offset.
    pub(super) fn new(device: &PciDevice, cap_offset: u8) -> Self {
        let msg_ctrl: u16 = device.read(cap_offset + 2);
        let table_size = (msg_ctrl & MSIX_TABLE_SIZE_MASK) + 1;

        let table_offset_bir: u32 = device.read(cap_offset + 4);
        let table_bar = (table_offset_bir & 0x7) as u8;
        let table_offset = table_offset_bir & !0x7;

        // Get BAR address (handles 64-bit BARs) and map the table
        let bar_addr = device.bar_address(table_bar);
        let table_phys = PhysAddr::new(bar_addr + table_offset as u64);

        // Each MSI-X entry is 16 bytes (addr_lo, addr_hi, data, ctrl)
        let table_bytes = (table_size as usize) * 16;

        // Map MMIO region to higher-half
        let mapping = PhysicalMapping::new(table_phys, table_bytes);
        let table_vaddr = mapping.virt_addr();
        // Store the mapping - MSI-X table persists for device lifetime
        PCI_DEVICE_MAPPINGS.lock().push(mapping);

        debug!(
            "MSI-X table: BAR{} addr={:#x}, offset={:#x}, phys={:#x}, vaddr={:#x}",
            table_bar,
            bar_addr,
            table_offset,
            table_phys.as_u64(),
            table_vaddr.as_u64()
        );

        Self {
            device: device.clone(),
            cap_offset,
            table_vaddr,
            table_size,
        }
    }

    /// Get the address of an MSI-X table entry field.
    fn entry_addr(&self, index: u16, field_offset: u64) -> VirtAddr {
        self.table_vaddr + (index as u64 * 16) + field_offset
    }

    /// Read a value from an MSI-X table entry field.
    fn read_entry_field<T: Copy>(&self, index: u16, field_offset: u64) -> T {
        let addr = self.entry_addr(index, field_offset);
        unsafe { core::ptr::read_volatile(addr.as_ptr::<T>()) }
    }

    /// Write a value to an MSI-X table entry field.
    fn write_entry_field<T: Copy>(&self, index: u16, field_offset: u64, value: T) {
        let addr = self.entry_addr(index, field_offset);
        unsafe { core::ptr::write_volatile(addr.as_mut_ptr::<T>(), value) }
    }

    /// Get the message control register value.
    pub fn message_control(&self) -> u16 {
        self.device.read(self.cap_offset + 2)
    }

    /// Set the message control register value.
    pub fn set_message_control(&mut self, value: u16) {
        unsafe { self.device.write(self.cap_offset + 2, value) }
    }

    /// Get the number of MSI-X table entries.
    pub fn table_size(&self) -> u16 {
        self.table_size
    }

    /// Get which BAR contains the MSI-X table.
    pub fn table_bar(&self) -> u8 {
        let table_offset_bir: u32 = self.device.read(self.cap_offset + 4);
        (table_offset_bir & 0x7) as u8
    }

    /// Get the offset of the MSI-X table within the BAR.
    pub fn table_offset(&self) -> u32 {
        let table_offset_bir: u32 = self.device.read(self.cap_offset + 4);
        table_offset_bir & !0x7
    }

    /// Configure an MSI-X table entry to deliver an interrupt.
    ///
    /// - `index`: Table entry index (0 to table_size-1)
    /// - `vector`: CPU interrupt vector number
    /// - `destination_cpu`: Target CPU's APIC ID (usually 0 for BSP)
    pub fn configure_entry(&self, index: u16, vector: u8, destination_cpu: u8) {
        assert!(
            index < self.table_size,
            "MSI-X entry index {} out of range (max {})",
            index,
            self.table_size - 1
        );

        // Message Address: 0xFEE00000 | (destination_cpu << 12)
        // This targets the Local APIC at the destination CPU
        let msg_addr: u64 = 0xFEE00000 | ((destination_cpu as u64) << 12);
        let msg_data: u32 = vector as u32;
        let vector_ctrl: u32 = 0; // 0 = unmasked

        self.write_entry_field(index, MSIX_ENTRY_MSG_ADDR, msg_addr);
        self.write_entry_field(index, MSIX_ENTRY_MSG_DATA, msg_data);
        self.write_entry_field(index, MSIX_ENTRY_VECTOR_CTRL, vector_ctrl);

        debug!(
            "MSI-X entry {}: addr={:#x}, data={:#x}, ctrl={:#x}",
            index, msg_addr, msg_data, vector_ctrl
        );
    }

    /// Mask an MSI-X table entry (disable its interrupt).
    pub fn mask_entry(&self, index: u16) {
        assert!(index < self.table_size);
        let ctrl: u32 = self.read_entry_field(index, MSIX_ENTRY_VECTOR_CTRL);
        self.write_entry_field(index, MSIX_ENTRY_VECTOR_CTRL, ctrl | MSIX_ENTRY_MASKED);
    }

    /// Unmask an MSI-X table entry (enable its interrupt).
    pub fn unmask_entry(&self, index: u16) {
        assert!(index < self.table_size);
        let ctrl: u32 = self.read_entry_field(index, MSIX_ENTRY_VECTOR_CTRL);
        self.write_entry_field(index, MSIX_ENTRY_VECTOR_CTRL, ctrl & !MSIX_ENTRY_MASKED);
    }

    /// Check if an entry is masked.
    pub fn is_entry_masked(&self, index: u16) -> bool {
        assert!(index < self.table_size);
        let ctrl: u32 = self.read_entry_field(index, MSIX_ENTRY_VECTOR_CTRL);
        ctrl & MSIX_ENTRY_MASKED != 0
    }

    /// Read back an MSI-X table entry for debugging.
    pub fn read_entry(&self, index: u16) -> (u64, u32, u32) {
        assert!(index < self.table_size);
        let msg_addr: u64 = self.read_entry_field(index, MSIX_ENTRY_MSG_ADDR);
        let msg_data: u32 = self.read_entry_field(index, MSIX_ENTRY_MSG_DATA);
        let vector_ctrl: u32 = self.read_entry_field(index, MSIX_ENTRY_VECTOR_CTRL);
        (msg_addr, msg_data, vector_ctrl)
    }

    /// Enable MSI-X for the device.
    ///
    /// Sets the enable bit and clears the function mask to allow interrupts.
    pub fn enable(&mut self) {
        let msg_ctrl = self.message_control();
        let new_msg_ctrl = (msg_ctrl | MSIX_ENABLE) & !MSIX_FUNCTION_MASK;
        self.set_message_control(new_msg_ctrl);
    }
}
