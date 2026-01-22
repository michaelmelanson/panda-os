//! Virtio block device driver.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use log::debug;
use spinning_top::{RwSpinlock, Spinlock};
use virtio_drivers::{
    device::blk::VirtIOBlk,
    transport::pci::{PciTransport, bus::PciRoot},
};

use crate::device_address::DeviceAddress;
use crate::pci::device::PciDevice;
use crate::resource::{BlockDevice, BlockError};

use super::virtio_hal::VirtioHal;

/// A virtio block device.
pub struct VirtioBlockDevice {
    device: VirtIOBlk<VirtioHal, PciTransport>,
    address: DeviceAddress,
    capacity_sectors: u64,
    sector_size: u32,
}

impl VirtioBlockDevice {
    /// Get the device address.
    pub fn address(&self) -> &DeviceAddress {
        &self.address
    }
}

impl BlockDevice for Spinlock<VirtioBlockDevice> {
    fn read_sectors(&self, start_sector: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let mut device = self.lock();
        let sector_size = device.sector_size as usize;

        if buf.len() % sector_size != 0 {
            return Err(BlockError::IoError);
        }

        let num_sectors = buf.len() / sector_size;
        for i in 0..num_sectors {
            let sector = start_sector + i as u64;
            let offset = i * sector_size;
            device
                .device
                .read_blocks(sector as usize, &mut buf[offset..offset + sector_size])
                .map_err(|_| BlockError::IoError)?;
        }

        Ok(())
    }

    fn write_sectors(&self, start_sector: u64, buf: &[u8]) -> Result<(), BlockError> {
        let mut device = self.lock();
        let sector_size = device.sector_size as usize;

        if buf.len() % sector_size != 0 {
            return Err(BlockError::IoError);
        }

        let num_sectors = buf.len() / sector_size;
        for i in 0..num_sectors {
            let sector = start_sector + i as u64;
            let offset = i * sector_size;
            device
                .device
                .write_blocks(sector as usize, &buf[offset..offset + sector_size])
                .map_err(|_| BlockError::IoError)?;
        }

        Ok(())
    }

    fn sector_size(&self) -> u32 {
        self.lock().sector_size
    }

    fn sector_count(&self) -> u64 {
        self.lock().capacity_sectors
    }

    fn flush(&self) -> Result<(), BlockError> {
        // VirtIO block doesn't have an explicit flush in the virtio-drivers crate
        // The device should handle write-through or we could implement flush via
        // the VIRTIO_BLK_T_FLUSH request type if needed
        Ok(())
    }
}

/// Global registry of block devices keyed by DeviceAddress.
static BLOCK_DEVICES: RwSpinlock<BTreeMap<DeviceAddress, Arc<Spinlock<VirtioBlockDevice>>>> =
    RwSpinlock::new(BTreeMap::new());

/// Get a block device by its device address.
pub fn get_device(address: &DeviceAddress) -> Option<Arc<Spinlock<VirtioBlockDevice>>> {
    BLOCK_DEVICES.read().get(address).cloned()
}

/// List all block device addresses.
pub fn list_devices() -> Vec<DeviceAddress> {
    BLOCK_DEVICES.read().keys().cloned().collect()
}

/// Initialize a virtio block device from a PCI device.
pub fn init_from_pci_device(pci_device: PciDevice) {
    let pci_address = pci_device.address();
    let address = DeviceAddress::Pci {
        bus: pci_address.bus,
        device: pci_address.slot,
        function: pci_address.function,
    };

    debug!("Initializing virtio block device at {}", address);

    let mut root = PciRoot::new(pci_device.clone());
    let device_function = pci_address.into();
    let transport = PciTransport::new::<VirtioHal, PciDevice>(&mut root, device_function)
        .expect("Could not create PCI transport for virtio block device");

    let device = VirtIOBlk::<VirtioHal, PciTransport>::new(transport)
        .expect("Could not initialize virtio block device");

    let capacity_sectors = device.capacity();
    // VirtIO block devices use 512-byte sectors by default
    let sector_size = 512u32;

    debug!(
        "Virtio block device: {} sectors, {} bytes/sector, total {} bytes",
        capacity_sectors,
        sector_size,
        capacity_sectors * sector_size as u64
    );

    let block_device = VirtioBlockDevice {
        device,
        address: address.clone(),
        capacity_sectors,
        sector_size,
    };

    let block_device = Arc::new(Spinlock::new(block_device));

    // Register in global map
    BLOCK_DEVICES.write().insert(address, block_device);

    debug!("Virtio block device initialized");
}
