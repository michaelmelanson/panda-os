#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use panda_kernel::pci::{self, DeviceClass};

panda_kernel::test_harness!(
    finds_devices,
    valid_vendor_ids,
    finds_virtio_device,
    device_class_code_roundtrip,
    device_class_name_roundtrip,
    device_class_all_variants,
    device_class_unknown_code,
    device_class_unknown_name,
    devices_registered_by_class,
    get_device_by_class_name
);

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

// =============================================================================
// DeviceClass enum tests
// =============================================================================

fn device_class_code_roundtrip() {
    // Test that code -> DeviceClass -> code is identity
    for class in DeviceClass::ALL {
        let code = class.code();
        let recovered = DeviceClass::from_code(code).expect("Should recover class from code");
        assert_eq!(*class, recovered, "Roundtrip failed for {:?}", class);
    }
}

fn device_class_name_roundtrip() {
    // Test that name -> DeviceClass -> name is identity
    for class in DeviceClass::ALL {
        let name = class.name();
        let recovered = DeviceClass::from_name(name).expect("Should recover class from name");
        assert_eq!(*class, recovered, "Roundtrip failed for {:?}", class);
    }
}

fn device_class_all_variants() {
    // Verify ALL contains expected classes
    assert!(DeviceClass::ALL.contains(&DeviceClass::Storage));
    assert!(DeviceClass::ALL.contains(&DeviceClass::Network));
    assert!(DeviceClass::ALL.contains(&DeviceClass::Display));
    assert!(DeviceClass::ALL.contains(&DeviceClass::Input));
    assert!(
        DeviceClass::ALL.len() >= 11,
        "Should have at least 11 device classes"
    );
}

fn device_class_unknown_code() {
    // Unknown codes should return None
    assert!(DeviceClass::from_code(0x00).is_none());
    assert!(DeviceClass::from_code(0xFF).is_none());
    assert!(DeviceClass::from_code(0x0A).is_none()); // Gap between Input (0x09) and USB (0x0C)
}

fn device_class_unknown_name() {
    // Unknown names should return None
    assert!(DeviceClass::from_name("unknown").is_none());
    assert!(DeviceClass::from_name("").is_none());
    assert!(DeviceClass::from_name("STORAGE").is_none()); // Case sensitive
}

// =============================================================================
// Class registry tests
// =============================================================================

fn devices_registered_by_class() {
    // After enumeration, devices should be registered by class
    // QEMU always provides virtio-gpu (display) and virtio-keyboard (input)
    // virtio-blk (storage) is only added for specific tests
    let display_count = pci::count_devices_in_class(DeviceClass::Display.code());
    let input_count = pci::count_devices_in_class(DeviceClass::Input.code());

    assert!(display_count > 0, "Expected at least one display device");
    assert!(input_count > 0, "Expected at least one input device");
}

fn get_device_by_class_name() {
    // Test the helper function that uses class names
    // Use "input" since virtio-keyboard is always present
    let input_device = pci::get_device_by_class_name("input", 0);
    assert!(input_device.is_some(), "Should find input device by name");

    let nonexistent = pci::get_device_by_class_name("input", 999);
    assert!(
        nonexistent.is_none(),
        "Should not find device at invalid index"
    );

    let invalid_class = pci::get_device_by_class_name("invalid", 0);
    assert!(
        invalid_class.is_none(),
        "Should not find device with invalid class name"
    );
}
