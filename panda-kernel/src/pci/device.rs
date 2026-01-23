use core::fmt::UpperHex;

use log::{debug, trace};
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::MmioMapping;
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

    /// Configure and enable MSI-X for this device
    /// Returns the configured MsixCapability on success
    pub fn enable_msix(&self) -> Option<MsixCapability> {
        let mut cap = self.msix_capability()?;

        debug!(
            "PCI {}: Enabling MSI-X with {} vectors, table in BAR{} at offset {:#x}",
            self.address(),
            cap.table_size(),
            cap.table_bar(),
            cap.table_offset()
        );

        // Enable MSI-X (set bit 15 of message control)
        // Also clear function mask (bit 14) to allow interrupts
        let msg_ctrl = cap.message_control();
        let new_msg_ctrl = (msg_ctrl | MSIX_ENABLE) & !MSIX_FUNCTION_MASK;
        cap.set_message_control(new_msg_ctrl);

        Some(cap)
    }
}

/// PCI Capability ID for MSI-X
const PCI_CAP_ID_MSIX: u8 = 0x11;

/// PCI Capability ID for vendor-specific (used by virtio)
const PCI_CAP_ID_VENDOR: u8 = 0x09;

/// Virtio PCI capability types
const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;

/// Offsets within virtio common config structure
const VIRTIO_COMMON_CFG_MSIX_CONFIG: u64 = 16;
const VIRTIO_COMMON_CFG_NUM_QUEUES: u64 = 18;
const VIRTIO_COMMON_CFG_DEVICE_STATUS: u64 = 20;
const VIRTIO_COMMON_CFG_QUEUE_SELECT: u64 = 22;
const VIRTIO_COMMON_CFG_QUEUE_MSIX_VECTOR: u64 = 26;

/// Special value indicating no MSI-X vector assigned
pub const VIRTIO_MSI_NO_VECTOR: u16 = 0xFFFF;

/// MSI-X Message Control register bits
const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_FUNCTION_MASK: u16 = 1 << 14;
const MSIX_TABLE_SIZE_MASK: u16 = 0x07FF;

/// MSI-X table entry vector control bits
const MSIX_ENTRY_MASKED: u32 = 1 << 0;

/// MSI-X Capability structure
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

/// Virtio common config accessor for MSI-X configuration.
///
/// This allows configuring MSI-X vectors for virtio devices by writing
/// directly to the common config structure in BAR memory.
#[derive(Clone)]
pub struct VirtioCommonConfig {
    /// Virtual address of the common config structure
    base_vaddr: VirtAddr,
}

impl VirtioCommonConfig {
    /// Read a value from the MMIO config space at the given offset.
    fn read<T: Copy>(&self, offset: u64) -> T {
        let addr = self.base_vaddr + offset;
        unsafe { core::ptr::read_volatile(addr.as_ptr::<T>()) }
    }

    /// Write a value to the MMIO config space at the given offset.
    fn write<T: Copy>(&self, offset: u64, value: T) {
        let addr = self.base_vaddr + offset;
        unsafe { core::ptr::write_volatile(addr.as_mut_ptr::<T>(), value) }
    }

    /// Find and map the virtio common config structure for a PCI device.
    /// Returns None if the device doesn't have a virtio common config capability.
    pub fn find(device: &PciDevice) -> Option<Self> {
        if !device.has_capabilities() {
            return None;
        }

        let mut cap_ptr = device.capabilities_pointer() & 0xFC;
        while cap_ptr != 0 {
            let cap_id: u8 = device.read(cap_ptr);
            if cap_id == PCI_CAP_ID_VENDOR {
                // Check the virtio capability type (at offset +3)
                let cap_type: u8 = device.read(cap_ptr + 3);
                if cap_type == VIRTIO_PCI_CAP_COMMON_CFG {
                    // Found it! Get BAR and offset
                    let bar_index: u8 = device.read(cap_ptr + 4);
                    let offset: u32 = device.read(cap_ptr + 8);
                    let length: u32 = device.read(cap_ptr + 12);

                    // Get BAR address (handles 64-bit BARs correctly)
                    let bar_addr = device.bar_address(bar_index);
                    let config_phys = PhysAddr::new(bar_addr + offset as u64);

                    // Map MMIO region to higher-half
                    let mmio = MmioMapping::new(config_phys, length as usize);
                    let base_vaddr = mmio.virt_addr();
                    // Leak the mapping - config persists for device lifetime
                    core::mem::forget(mmio);

                    debug!(
                        "PCI {}: Found virtio common config in BAR{} at offset {:#x}, bar_addr={:#x}, vaddr={:#x}",
                        device.address(),
                        bar_index,
                        offset,
                        bar_addr,
                        base_vaddr.as_u64()
                    );

                    return Some(Self { base_vaddr });
                }
            }
            cap_ptr = device.read::<u8>(cap_ptr + 1) & 0xFC;
        }
        None
    }

    /// Set the MSI-X vector for device configuration changes.
    /// Use VIRTIO_MSI_NO_VECTOR (0xFFFF) to disable.
    pub fn set_config_msix_vector(&self, vector: u16) {
        self.write(VIRTIO_COMMON_CFG_MSIX_CONFIG, vector);
        // Read back to verify (virtio spec says device may change it)
        let readback: u16 = self.read(VIRTIO_COMMON_CFG_MSIX_CONFIG);
        debug!(
            "virtio msix_config: wrote {}, read back {}",
            vector, readback
        );
    }

    /// Set the MSI-X vector for a specific queue.
    /// Must call set_queue_select first to select the queue.
    pub fn set_queue_msix_vector(&self, vector: u16) {
        self.write(VIRTIO_COMMON_CFG_QUEUE_MSIX_VECTOR, vector);
        // Read back to verify
        let readback: u16 = self.read(VIRTIO_COMMON_CFG_QUEUE_MSIX_VECTOR);
        debug!(
            "virtio queue_msix_vector: wrote {}, read back {}",
            vector, readback
        );
    }

    /// Select a queue for subsequent queue operations.
    pub fn set_queue_select(&self, queue: u16) {
        self.write(VIRTIO_COMMON_CFG_QUEUE_SELECT, queue);
    }

    /// Configure MSI-X for a virtio device with a single vector for all interrupts.
    ///
    /// This sets both the config vector and queue 0's vector to the same MSI-X entry.
    pub fn configure_msix_single_vector(&self, vector: u16) {
        // Set config change interrupt vector
        self.set_config_msix_vector(vector);

        // Set queue 0 interrupt vector
        self.set_queue_select(0);
        self.set_queue_msix_vector(vector);
    }

    /// Read the current msix_config value.
    pub fn read_msix_config(&self) -> u16 {
        self.read(VIRTIO_COMMON_CFG_MSIX_CONFIG)
    }

    /// Read the device_status register.
    pub fn read_device_status(&self) -> u8 {
        self.read(VIRTIO_COMMON_CFG_DEVICE_STATUS)
    }

    /// Read the num_queues register.
    pub fn read_num_queues(&self) -> u16 {
        self.read(VIRTIO_COMMON_CFG_NUM_QUEUES)
    }

    /// Read the current queue_msix_vector for a given queue.
    pub fn read_queue_msix_vector(&self, queue: u16) -> u16 {
        self.set_queue_select(queue);
        self.read(VIRTIO_COMMON_CFG_QUEUE_MSIX_VECTOR)
    }
}

/// MSI-X table entry offsets (each entry is 16 bytes)
const MSIX_ENTRY_MSG_ADDR: u64 = 0;
const MSIX_ENTRY_MSG_DATA: u64 = 8;
const MSIX_ENTRY_VECTOR_CTRL: u64 = 12;

impl MsixCapability {
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

    fn new(device: &PciDevice, cap_offset: u8) -> Self {
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
        let mmio = MmioMapping::new(table_phys, table_bytes);
        let table_vaddr = mmio.virt_addr();
        // Leak the mapping - MSI-X table persists for device lifetime
        core::mem::forget(mmio);

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

    /// Get the message control register value
    pub fn message_control(&self) -> u16 {
        self.device.read(self.cap_offset + 2)
    }

    /// Set the message control register value
    pub fn set_message_control(&mut self, value: u16) {
        unsafe { self.device.write(self.cap_offset + 2, value) }
    }

    /// Get the number of MSI-X table entries
    pub fn table_size(&self) -> u16 {
        self.table_size
    }

    /// Get which BAR contains the MSI-X table
    pub fn table_bar(&self) -> u8 {
        let table_offset_bir: u32 = self.device.read(self.cap_offset + 4);
        (table_offset_bir & 0x7) as u8
    }

    /// Get the offset of the MSI-X table within the BAR
    pub fn table_offset(&self) -> u32 {
        let table_offset_bir: u32 = self.device.read(self.cap_offset + 4);
        table_offset_bir & !0x7
    }

    /// Configure an MSI-X table entry to deliver an interrupt
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

    /// Mask an MSI-X table entry (disable its interrupt)
    pub fn mask_entry(&self, index: u16) {
        assert!(index < self.table_size);
        let ctrl: u32 = self.read_entry_field(index, MSIX_ENTRY_VECTOR_CTRL);
        self.write_entry_field(index, MSIX_ENTRY_VECTOR_CTRL, ctrl | MSIX_ENTRY_MASKED);
    }

    /// Unmask an MSI-X table entry (enable its interrupt)
    pub fn unmask_entry(&self, index: u16) {
        assert!(index < self.table_size);
        let ctrl: u32 = self.read_entry_field(index, MSIX_ENTRY_VECTOR_CTRL);
        self.write_entry_field(index, MSIX_ENTRY_VECTOR_CTRL, ctrl & !MSIX_ENTRY_MASKED);
    }

    /// Check if an entry is masked
    pub fn is_entry_masked(&self, index: u16) -> bool {
        assert!(index < self.table_size);
        let ctrl: u32 = self.read_entry_field(index, MSIX_ENTRY_VECTOR_CTRL);
        ctrl & MSIX_ENTRY_MASKED != 0
    }

    /// Read back an MSI-X table entry for debugging
    pub fn read_entry(&self, index: u16) -> (u64, u32, u32) {
        assert!(index < self.table_size);
        let msg_addr: u64 = self.read_entry_field(index, MSIX_ENTRY_MSG_ADDR);
        let msg_data: u32 = self.read_entry_field(index, MSIX_ENTRY_MSG_DATA);
        let vector_ctrl: u32 = self.read_entry_field(index, MSIX_ENTRY_VECTOR_CTRL);
        (msg_addr, msg_data, vector_ctrl)
    }
}
