//! Userspace test for device path resolution and discovery.
//!
//! Tests the unified device path system using devices that are always present
//! (virtio-gpu and virtio-keyboard) rather than virtio-blk which is only
//! added for specific tests.

#![no_std]
#![no_main]

use libpanda::{DirEntry, environment, file, process};

libpanda::main! {
    environment::log("device_path_test starting");

    // Test 1: List device classes via readdir on keyboard scheme
    test_readdir_pci_classes();

    // Test 2: List devices in input class
    test_readdir_input_devices();

    // Test 3: Cross-scheme discovery for input device
    test_cross_scheme_discovery();

    environment::log("device_path_test passed");
    0
}

fn test_readdir_pci_classes() {
    environment::log("Test 1: List PCI device classes");

    // Open keyboard:/pci as directory to list device classes
    let result = environment::opendir("keyboard:/pci");
    if result.is_err() {
        environment::log("FAIL: Could not opendir keyboard:/pci");
        process::exit(1);
    }

    let handle = result.unwrap();

    let mut entry = DirEntry {
        name_len: 0,
        is_dir: false,
        name: [0; 255],
    };

    let mut found_input = false;
    loop {
        let result = file::readdir(handle, &mut entry);
        if result < 0 {
            environment::log("FAIL: readdir returned error");
            file::close(handle);
            process::exit(1);
        }
        if result == 0 {
            break;
        }

        // Check if we found "input"
        let name = entry.name();
        if name == "input" {
            found_input = true;
        }
    }

    file::close(handle);

    if !found_input {
        environment::log("FAIL: Did not find 'input' class in keyboard:/pci");
        process::exit(1);
    }

    environment::log("  PCI classes listing OK");
}

fn test_readdir_input_devices() {
    environment::log("Test 2: List input devices");

    // Open keyboard:/pci/input as directory to list device indices
    let result = environment::opendir("keyboard:/pci/input");
    if result.is_err() {
        environment::log("FAIL: Could not opendir keyboard:/pci/input");
        process::exit(1);
    }

    let handle = result.unwrap();

    let mut entry = DirEntry {
        name_len: 0,
        is_dir: false,
        name: [0; 255],
    };

    let mut found_zero = false;
    loop {
        let result = file::readdir(handle, &mut entry);
        if result < 0 {
            environment::log("FAIL: readdir returned error");
            file::close(handle);
            process::exit(1);
        }
        if result == 0 {
            break;
        }

        // Check if we found "0" (first device)
        let name = entry.name();
        if name == "0" {
            found_zero = true;
        }
    }

    file::close(handle);

    if !found_zero {
        environment::log("FAIL: Did not find device '0' in keyboard:/pci/input");
        process::exit(1);
    }

    environment::log("  Input devices listing OK");
}

fn test_cross_scheme_discovery() {
    environment::log("Test 3: Cross-scheme discovery");

    // Use *:/pci/input/0 to discover which schemes support the first input device
    let result = environment::opendir("*:/pci/input/0");
    if result.is_err() {
        environment::log("FAIL: Could not opendir *:/pci/input/0");
        process::exit(1);
    }

    let handle = result.unwrap();

    let mut entry = DirEntry {
        name_len: 0,
        is_dir: false,
        name: [0; 255],
    };

    let mut found_keyboard = false;
    loop {
        let result = file::readdir(handle, &mut entry);
        if result < 0 {
            environment::log("FAIL: readdir returned error");
            file::close(handle);
            process::exit(1);
        }
        if result == 0 {
            break;
        }

        // Check if "keyboard" scheme supports this device
        let name = entry.name();
        if name == "keyboard" {
            found_keyboard = true;
        }
    }

    file::close(handle);

    if !found_keyboard {
        environment::log("FAIL: 'keyboard' scheme not found for input device");
        process::exit(1);
    }

    environment::log("  Cross-scheme discovery OK");
}
