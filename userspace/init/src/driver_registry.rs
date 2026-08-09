//! Driver registry: scans a directory of ELF driver binaries and builds a
//! per-bus lookup from device match tables to binary paths, so the service
//! manager can spawn the right driver when a device appears — without any
//! `[device]` section in a TOML config. See
//! `plans/device-driver-model.md` ("Service manager role").
//!
//! `main.rs` spawns every driver `binary_paths()` discovers at boot (see
//! that method's doc comment for why, given `DeviceEvent`'s payload, it
//! spawns unconditionally rather than pre-filtering by identity).
//! `find_driver` and the `DeviceIdentityBytes`-based matching it depends on
//! remain unused for now — a true "spawn only the driver(s) a specific
//! enumerated device needs" path would need a way to read a not-yet-claimed
//! device's identity, which doesn't exist yet (see `binary_paths`'s doc
//! comment). `#[allow(dead_code)]` documents that gap rather than hiding
//! it.

#![allow(dead_code)]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libpanda::environment;
use libpanda::io::{File, Read};
use panda_abi::device::{AcpiDeviceId, BusType, IoPortDeviceId, PciDeviceId, UsbDeviceId};

/// One parsed entry from a driver's `.panda_devices.<bus>` section, together
/// with the path to the binary that declared it.
struct Entry {
    bus_type: BusType,
    match_bytes: Vec<u8>,
    binary_path: String,
}

/// Maps device match tables (read from driver ELF metadata) to the binary
/// path that declared them.
///
/// Built via repeated calls to [`DriverRegistry::scan`] (once for
/// `initrd:/drivers/`, once for `file:/mnt/drivers/`) — later scans add to
/// the registry without clearing earlier entries, so a driver found in the
/// initrd remains matchable after the root filesystem is scanned too.
#[derive(Default)]
pub struct DriverRegistry {
    entries: Vec<Entry>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan every file directly under `dir_uri` (e.g. `"initrd:/drivers"` or
    /// `"file:/mnt/drivers"`) for ELF binaries with `.panda_devices.*`
    /// sections, adding their match entries to the registry. Entries from a
    /// previous `scan` call are kept.
    ///
    /// Unreadable directories, non-ELF files, and binaries with no
    /// `.panda_devices.*` sections are skipped rather than treated as
    /// errors — a `drivers/` directory is allowed to contain other files,
    /// and this is best-effort discovery, not validation.
    pub fn scan(&mut self, dir_uri: &str) {
        let Ok(dir_handle) = environment::opendir(dir_uri) else {
            return;
        };

        let mut entry = panda_abi::DirEntry {
            name_len: 0,
            is_dir: false,
            name: [0; panda_abi::DIRENT_NAME_MAX],
        };

        loop {
            let result = libpanda::file::readdir(dir_handle, &mut entry);
            if result <= 0 {
                break;
            }
            if entry.is_dir {
                continue;
            }

            let name = entry.name();
            let path = alloc::format!("{}/{}", dir_uri.trim_end_matches('/'), name);
            self.scan_binary(&path);
        }
    }

    /// Read one binary's `.panda_devices.*` sections and add its entries.
    fn scan_binary(&mut self, path: &str) {
        let Ok(mut file) = File::open(path) else {
            return;
        };
        let mut data = Vec::new();
        if file.read_to_end(&mut data).is_err() {
            return;
        }

        for (bus_type, section_name, entry_size) in [
            (BusType::Pci, BusType::Pci.section_name(), core::mem::size_of::<PciDeviceId>()),
            (BusType::Usb, BusType::Usb.section_name(), core::mem::size_of::<UsbDeviceId>()),
            (BusType::Acpi, BusType::Acpi.section_name(), core::mem::size_of::<AcpiDeviceId>()),
            (
                BusType::IoPort,
                BusType::IoPort.section_name(),
                core::mem::size_of::<IoPortDeviceId>(),
            ),
        ] {
            let Some(section) = panda_elf::read_section(&data, section_name) else {
                continue;
            };
            if entry_size == 0 || section.len() % entry_size != 0 {
                continue;
            }
            for chunk in section.chunks_exact(entry_size) {
                self.entries.push(Entry {
                    bus_type,
                    match_bytes: chunk.to_vec(),
                    binary_path: path.to_string(),
                });
            }
        }
    }

    /// Find the first driver binary whose match table matches the given
    /// device identity, for `bus_type`.
    pub fn find_driver(&self, bus_type: BusType, match_against: &DeviceIdentityBytes) -> Option<&str> {
        self.entries.iter().find_map(|entry| {
            if entry.bus_type != bus_type {
                return None;
            }
            if matches(bus_type, &entry.match_bytes, match_against) {
                Some(entry.binary_path.as_str())
            } else {
                None
            }
        })
    }

    /// Every distinct driver binary path discovered by `scan`, in
    /// first-seen order, with no duplicates (a driver can declare several
    /// match-table entries across one or more buses, all sharing the same
    /// `binary_path`).
    ///
    /// Used by `main.rs` to spawn every discovered driver at boot: `Self`
    /// has no way to identify a *specific* enumerated device's vendor/
    /// product ids from a bare `DeviceEvent` (its payload carries only a
    /// bus address — see `panda_abi::device::DeviceEvent`/`DeviceIdentity` —
    /// not identity, and reading that back out requires a claimed device
    /// handle a not-yet-spawned driver doesn't have), so rather than
    /// building a separate pre-claim identification path, every discovered
    /// driver is spawned unconditionally; each one only actually claims
    /// hardware for the device(s) matching its own `.panda_devices.<bus>`
    /// table (via its own `device_subscribe` call, which is itself the
    /// filter).
    pub fn binary_paths(&self) -> Vec<&str> {
        let mut paths: Vec<&str> = Vec::new();
        for entry in &self.entries {
            let path = entry.binary_path.as_str();
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }
}

/// The device attributes to match a driver's table entry against, in the
/// same encoding as the corresponding `*DeviceId` struct's fields (so
/// `matches` can compare them field-by-field per bus type).
pub enum DeviceIdentityBytes {
    Pci { vendor_id: u16, device_id: u16, class: u32 },
    Usb { vendor_id: u16, product_id: u16, class: u8, subclass: u8, protocol: u8 },
    Acpi { hid: [u8; 8] },
    IoPort { base: u16, size: u16 },
}

fn read_pod<T: Copy>(bytes: &[u8]) -> Option<T> {
    if bytes.len() != core::mem::size_of::<T>() {
        return None;
    }
    // Safety: T is a #[repr(C)] POD struct (see panda_abi::device); bytes
    // length matches exactly, so any bit pattern is a valid T.
    Some(unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const T) })
}

fn matches(bus_type: BusType, match_bytes: &[u8], against: &DeviceIdentityBytes) -> bool {
    match (bus_type, against) {
        (BusType::Pci, DeviceIdentityBytes::Pci { vendor_id, device_id, class }) => {
            match read_pod::<PciDeviceId>(match_bytes) {
                Some(id) => id.matches(*vendor_id, *device_id, *class),
                None => false,
            }
        }
        (
            BusType::Usb,
            DeviceIdentityBytes::Usb {
                vendor_id,
                product_id,
                class,
                subclass,
                protocol,
            },
        ) => match read_pod::<UsbDeviceId>(match_bytes) {
            Some(id) => {
                use panda_abi::device::{
                    USB_MATCH_CLASS, USB_MATCH_PRODUCT, USB_MATCH_PROTOCOL, USB_MATCH_SUBCLASS,
                    USB_MATCH_VENDOR,
                };
                let flags = id.match_flags;
                (flags & USB_MATCH_VENDOR == 0 || id.vendor_id == 0xFFFF || id.vendor_id == *vendor_id)
                    && (flags & USB_MATCH_PRODUCT == 0
                        || id.product_id == 0xFFFF
                        || id.product_id == *product_id)
                    && (flags & USB_MATCH_CLASS == 0 || id.device_class == *class)
                    && (flags & USB_MATCH_SUBCLASS == 0 || id.device_subclass == *subclass)
                    && (flags & USB_MATCH_PROTOCOL == 0 || id.device_protocol == *protocol)
            }
            None => false,
        },
        (BusType::Acpi, DeviceIdentityBytes::Acpi { hid }) => {
            match read_pod::<AcpiDeviceId>(match_bytes) {
                Some(id) => id.hid == *hid,
                None => false,
            }
        }
        (BusType::IoPort, DeviceIdentityBytes::IoPort { base, size }) => {
            match read_pod::<IoPortDeviceId>(match_bytes) {
                Some(id) => id.base == *base && id.size == *size,
                None => false,
            }
        }
        _ => false,
    }
}
