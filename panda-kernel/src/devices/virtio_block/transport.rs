//! MSI-X aware PCI transport wrapper for virtio devices.
//!
//! The virtio spec requires MSI-X vectors to be set before queue_enable is written.
//! Since virtio-drivers doesn't expose hooks for this, we intercept queue_set to
//! configure MSI-X vectors at the right time.

use log::debug;
use virtio_drivers::{
    PhysAddr as VirtioPhysAddr,
    transport::pci::PciTransport,
    transport::{DeviceStatus, DeviceType, Transport},
};

use crate::pci::VirtioCommonConfig;

/// A wrapper around PciTransport that configures MSI-X vectors before enabling queues.
pub struct MsixPciTransport {
    inner: PciTransport,
    common_config: Option<VirtioCommonConfig>,
    msix_vector: u16,
}

impl MsixPciTransport {
    /// Create a new MSI-X aware transport wrapper.
    pub fn new(
        inner: PciTransport,
        common_config: Option<VirtioCommonConfig>,
        msix_vector: u16,
    ) -> Self {
        Self {
            inner,
            common_config,
            msix_vector,
        }
    }
}

impl Transport for MsixPciTransport {
    fn device_type(&self) -> DeviceType {
        self.inner.device_type()
    }

    fn read_device_features(&mut self) -> u64 {
        self.inner.read_device_features()
    }

    fn write_driver_features(&mut self, driver_features: u64) {
        self.inner.write_driver_features(driver_features)
    }

    fn max_queue_size(&mut self, queue: u16) -> u32 {
        self.inner.max_queue_size(queue)
    }

    fn notify(&mut self, queue: u16) {
        self.inner.notify(queue);
    }

    fn get_status(&self) -> DeviceStatus {
        self.inner.get_status()
    }

    fn set_status(&mut self, status: DeviceStatus) {
        // Before setting DRIVER_OK, configure the msix_config vector
        if status.contains(DeviceStatus::DRIVER_OK) {
            if let Some(ref common_config) = self.common_config {
                debug!(
                    "MsixPciTransport: Setting msix_config to {} before DRIVER_OK",
                    self.msix_vector
                );
                common_config.set_config_msix_vector(self.msix_vector);
            }
        }
        self.inner.set_status(status)
    }

    fn set_guest_page_size(&mut self, guest_page_size: u32) {
        self.inner.set_guest_page_size(guest_page_size)
    }

    fn requires_legacy_layout(&self) -> bool {
        self.inner.requires_legacy_layout()
    }

    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: VirtioPhysAddr,
        driver_area: VirtioPhysAddr,
        device_area: VirtioPhysAddr,
    ) {
        // Configure MSI-X vector for this queue BEFORE the inner transport enables it
        if let Some(ref common_config) = self.common_config {
            debug!(
                "MsixPciTransport: Setting queue {} MSI-X vector to {} before enable",
                queue, self.msix_vector
            );
            common_config.set_queue_select(queue);
            common_config.set_queue_msix_vector(self.msix_vector);
        }

        // Now delegate to inner transport which will enable the queue
        self.inner
            .queue_set(queue, size, descriptors, driver_area, device_area)
    }

    fn queue_unset(&mut self, queue: u16) {
        self.inner.queue_unset(queue)
    }

    fn queue_used(&mut self, queue: u16) -> bool {
        self.inner.queue_used(queue)
    }

    fn ack_interrupt(&mut self) -> virtio_drivers::transport::InterruptStatus {
        self.inner.ack_interrupt()
    }

    fn read_config_generation(&self) -> u32 {
        self.inner.read_config_generation()
    }

    fn read_config_space<T: zerocopy::FromBytes + zerocopy::IntoBytes>(
        &self,
        offset: usize,
    ) -> virtio_drivers::Result<T> {
        self.inner.read_config_space(offset)
    }

    fn write_config_space<T: zerocopy::IntoBytes + zerocopy::Immutable>(
        &mut self,
        offset: usize,
        value: T,
    ) -> virtio_drivers::Result<()> {
        self.inner.write_config_space(offset, value)
    }
}
