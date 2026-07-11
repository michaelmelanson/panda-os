//! Unified device path resolution.
//!
//! This module provides shared path resolution logic used by all device schemes.
//! Paths follow the pattern: `/pci/<class>/<index>` or `/pci/<bus:dev.fn>`
//!
//! Examples:
//! - `/pci/storage/0` - first storage device
//! - `/pci/input/0` - first input device
//! - `/pci/00:04.0` - device by raw PCI address

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::device_address::DeviceAddress;
use crate::pci::{self, DeviceClass};
use crate::resource::DirEntry;

/// Resolve a device path to a DeviceAddress.
///
/// Supports:
/// - `/pci/<class>/<index>` - by class name and index (e.g., `/pci/storage/0`)
/// - `/pci/<bus:dev.fn>` - by raw PCI address (e.g., `/pci/00:04.0`)
pub fn resolve(path: &str) -> Option<DeviceAddress> {
    let path = path.strip_prefix('/').unwrap_or(path);

    if let Some(rest) = path.strip_prefix("pci/") {
        resolve_pci(rest)
    } else {
        None
    }
}

/// Resolve a PCI path component.
fn resolve_pci(path: &str) -> Option<DeviceAddress> {
    // Try class/index format first: "storage/0", "input/0"
    if let Some((class_name, index_str)) = path.split_once('/') {
        let class = DeviceClass::from_name(class_name)?;
        let index: usize = index_str.parse().ok()?;
        return pci::get_device_by_class(class.code(), index);
    }

    // Fall back to raw address: "00:04.0"
    DeviceAddress::parse_bdf(path)
}

/// List directory contents at a device path.
///
/// Supports:
/// - `/` or `` - list bus types (currently just "pci")
/// - `/pci` - list device classes with devices
/// - `/pci/<class>` - list device indices in that class
pub fn list(path: &str) -> Option<Vec<DirEntry>> {
    let path = path.strip_prefix('/').unwrap_or(path);

    if path.is_empty() {
        // Root: list bus types
        Some(vec![DirEntry {
            name: String::from("pci"),
            is_dir: true,
        }])
    } else if path == "pci" {
        // List device classes that have devices
        let classes = pci::list_device_classes();
        Some(
            classes
                .into_iter()
                .filter_map(|code| {
                    DeviceClass::from_code(code).map(|class| DirEntry {
                        name: String::from(class.name()),
                        is_dir: true,
                    })
                })
                .collect(),
        )
    } else if let Some(class_name) = path.strip_prefix("pci/") {
        // Check if this is a class name (list devices) or a device path
        if !class_name.contains('/') {
            // It's a class name - list device indices
            let class = DeviceClass::from_name(class_name)?;
            let count = pci::count_devices_in_class(class.code());
            Some(
                (0..count)
                    .map(|i| DirEntry {
                        name: i.to_string(),
                        is_dir: false,
                    })
                    .collect(),
            )
        } else {
            // It's a device path - no children
            None
        }
    } else {
        None
    }
}
