//! Tests for the device_path module.

#![no_std]
#![no_main]

extern crate alloc;

use panda_kernel::device_address::DeviceAddress;
use panda_kernel::device_path;

panda_kernel::test_harness!(
    resolve_pci_class_path,
    resolve_pci_raw_address,
    resolve_invalid_path,
    resolve_invalid_class,
    resolve_invalid_index,
    list_root,
    list_pci_classes,
    list_pci_class_devices,
    list_invalid_path
);

// =============================================================================
// Path resolution tests
// =============================================================================

fn resolve_pci_class_path() {
    // Resolve a class-based path like "/pci/input/0" (virtio-keyboard is always present)
    let addr = device_path::resolve("/pci/input/0");
    assert!(addr.is_some(), "Should resolve /pci/input/0");

    let addr = addr.unwrap();
    assert!(addr.is_pci(), "Should be a PCI address");

    // Also test display class (virtio-gpu is always present)
    let addr = device_path::resolve("/pci/display/0");
    assert!(addr.is_some(), "Should resolve /pci/display/0");
}

fn resolve_pci_raw_address() {
    // Resolve a raw PCI address like "/pci/00:04.0"
    let addr = device_path::resolve("/pci/00:04.0");
    assert!(addr.is_some(), "Should resolve /pci/00:04.0");

    let addr = addr.unwrap();
    match addr {
        DeviceAddress::Pci {
            bus,
            device,
            function,
        } => {
            assert_eq!(bus, 0);
            assert_eq!(device, 4);
            assert_eq!(function, 0);
        }
        _ => panic!("Expected PCI address"),
    }
}

fn resolve_invalid_path() {
    // Invalid paths should return None
    assert!(device_path::resolve("").is_none());
    assert!(device_path::resolve("/").is_none());
    assert!(device_path::resolve("/usb/0").is_none()); // USB not implemented yet
    assert!(device_path::resolve("/invalid/path").is_none());
}

fn resolve_invalid_class() {
    // Invalid class names should return None
    assert!(device_path::resolve("/pci/invalid/0").is_none());
    assert!(device_path::resolve("/pci/STORAGE/0").is_none()); // Case sensitive
}

fn resolve_invalid_index() {
    // Invalid indices should return None
    assert!(device_path::resolve("/pci/input/999").is_none());
    assert!(device_path::resolve("/pci/input/abc").is_none());
    assert!(device_path::resolve("/pci/input/-1").is_none());
}

// =============================================================================
// Directory listing tests
// =============================================================================

fn list_root() {
    // Root should list bus types
    let entries = device_path::list("/");
    assert!(entries.is_some(), "Should list root");

    let entries = entries.unwrap();
    assert!(!entries.is_empty(), "Root should have entries");

    // Should contain "pci"
    let has_pci = entries.iter().any(|e| e.name == "pci" && e.is_dir);
    assert!(has_pci, "Root should contain 'pci' directory");
}

fn list_pci_classes() {
    // /pci should list device classes that have devices
    let entries = device_path::list("/pci");
    assert!(entries.is_some(), "Should list /pci");

    let entries = entries.unwrap();
    assert!(!entries.is_empty(), "/pci should have device classes");

    // Should contain classes for our QEMU devices (display and input are always present)
    let has_display = entries.iter().any(|e| e.name == "display" && e.is_dir);
    let has_input = entries.iter().any(|e| e.name == "input" && e.is_dir);

    assert!(has_display, "/pci should contain 'display' class");
    assert!(has_input, "/pci should contain 'input' class");
}

fn list_pci_class_devices() {
    // /pci/input should list device indices (virtio-keyboard is always present)
    let entries = device_path::list("/pci/input");
    assert!(entries.is_some(), "Should list /pci/input");

    let entries = entries.unwrap();
    assert!(!entries.is_empty(), "/pci/input should have devices");

    // First device should be "0"
    let has_zero = entries.iter().any(|e| e.name == "0" && !e.is_dir);
    assert!(has_zero, "/pci/input should contain device '0'");
}

fn list_invalid_path() {
    // Invalid paths should return None
    assert!(device_path::list("/invalid").is_none());
    assert!(device_path::list("/pci/invalid").is_none());
    assert!(device_path::list("/pci/input/0").is_none()); // Device path, not directory
}
