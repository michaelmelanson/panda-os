//! Virtio PCI common configuration access.
//!
//! This module provides access to the virtio common config structure in BAR
//! memory, which is used for configuring MSI-X vectors for virtio devices.

use log::debug;
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::PhysicalMapping;

use super::device::{PCI_DEVICE_MAPPINGS, PciDevice};

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

/// Special value indicating no MSI-X vector assigned.
pub const VIRTIO_MSI_NO_VECTOR: u16 = 0xFFFF;

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
    ///
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
                    let mapping = PhysicalMapping::new(config_phys, length as usize);
                    let base_vaddr = mapping.virt_addr();
                    // Store the mapping - config persists for device lifetime
                    PCI_DEVICE_MAPPINGS.lock().push(mapping);

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
    ///
    /// Use `VIRTIO_MSI_NO_VECTOR` (0xFFFF) to disable.
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
    ///
    /// Must call `set_queue_select` first to select the queue.
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
