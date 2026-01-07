#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use panda_kernel::pci;

panda_kernel::test_harness!(finds_devices, valid_vendor_ids, finds_virtio_device);

fn finds_devices() {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    COUNT.store(0, Ordering::SeqCst);
    pci::enumerate_pci_devices(|_| {
        COUNT.fetch_add(1, Ordering::SeqCst);
    });
    assert!(
        COUNT.load(Ordering::SeqCst) > 0,
        "Expected at least one PCI device"
    );
}

fn valid_vendor_ids() {
    pci::enumerate_pci_devices(|device| {
        assert_ne!(device.vendor_id(), 0xFFFF);
    });
}

fn finds_virtio_device() {
    // Red Hat/Virtio vendor ID is 0x1AF4
    static FOUND: AtomicBool = AtomicBool::new(false);
    FOUND.store(false, Ordering::SeqCst);
    pci::enumerate_pci_devices(|device| {
        if device.vendor_id() == 0x1AF4 {
            FOUND.store(true, Ordering::SeqCst);
        }
    });
    assert!(
        FOUND.load(Ordering::SeqCst),
        "Expected Virtio device (vendor 0x1AF4)"
    );
}
