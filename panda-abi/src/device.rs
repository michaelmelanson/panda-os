//! Shared types for the userspace device driver model.
//!
//! These types are used both by the kernel (device registry, subscription
//! registry, `OP_DEVICE_*` syscall handlers) and by userspace (driver ELF
//! metadata sections, `libpanda::device` syscall wrappers). See
//! `plans/device-driver-model.md` for the full design.

// =============================================================================
// Bus types
// =============================================================================

/// Identifies which bus a device lives on.
///
/// Each bus type has its own match struct (see below), ELF section name
/// (`.panda_devices.<bus>`), and identity representation in [`DeviceEvent`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusType {
    Pci = 0,
    Usb = 1,
    Acpi = 2,
    IoPort = 3,
}

impl BusType {
    /// Convert to the raw `u32` used on the wire (syscall args, ELF section
    /// contents where applicable).
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Try to convert from a raw `u32`.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Pci),
            1 => Some(Self::Usb),
            2 => Some(Self::Acpi),
            3 => Some(Self::IoPort),
            _ => None,
        }
    }

    /// The ELF section name a driver's match table for this bus lives in.
    pub const fn section_name(self) -> &'static str {
        match self {
            Self::Pci => ".panda_devices.pci",
            Self::Usb => ".panda_devices.usb",
            Self::Acpi => ".panda_devices.acpi",
            Self::IoPort => ".panda_devices.ioport",
        }
    }
}

// =============================================================================
// Per-bus match structs
//
// Each struct is a fixed-size, `#[repr(C)]` entry in a driver's
// `.panda_devices.<bus>` ELF section. Fixed size means the section can be
// read as a flat array with no parsing: `entry_count = section_size /
// size_of::<T>()`.
// =============================================================================

/// PCI match entry: vendor/device ID and/or class code.
///
/// `vendor_id == 0xFFFF` and/or `device_id == 0xFFFF` act as wildcards.
/// `class_mask == 0` ignores the class code entirely.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciDeviceId {
    /// PCI vendor ID. `0xFFFF` = wildcard.
    pub vendor_id: u16,
    /// PCI device ID. `0xFFFF` = wildcard.
    pub device_id: u16,
    /// Device class code to match against (masked by `class_mask`).
    pub class: u32,
    /// Bitmask applied to `class` before comparison. `0` = ignore class.
    pub class_mask: u32,
}

const _: () = assert!(core::mem::size_of::<PciDeviceId>() == 12);

/// PCI wildcard vendor/device ID.
pub const PCI_MATCH_ANY: u16 = 0xFFFF;

impl PciDeviceId {
    /// Whether this match entry matches the given vendor/device/class.
    pub fn matches(&self, vendor_id: u16, device_id: u16, class: u32) -> bool {
        let vendor_ok = self.vendor_id == PCI_MATCH_ANY || self.vendor_id == vendor_id;
        let device_ok = self.device_id == PCI_MATCH_ANY || self.device_id == device_id;
        let class_ok = self.class_mask == 0 || (self.class & self.class_mask) == (class & self.class_mask);
        vendor_ok && device_ok && class_ok
    }
}

/// Bitmask flags for [`UsbDeviceId::match_flags`], selecting which fields
/// participate in matching.
pub const USB_MATCH_VENDOR: u8 = 1 << 0;
pub const USB_MATCH_PRODUCT: u8 = 1 << 1;
pub const USB_MATCH_CLASS: u8 = 1 << 2;
pub const USB_MATCH_SUBCLASS: u8 = 1 << 3;
pub const USB_MATCH_PROTOCOL: u8 = 1 << 4;

/// USB match entry: vendor/product ID and/or interface class.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbDeviceId {
    /// USB vendor ID. `0xFFFF` = wildcard.
    pub vendor_id: u16,
    /// USB product ID. `0xFFFF` = wildcard.
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    /// Bitmask of `USB_MATCH_*`: which fields are active for matching.
    pub match_flags: u8,
    pub _pad: u32,
}

const _: () = assert!(core::mem::size_of::<UsbDeviceId>() == 12);

/// ACPI match entry: hardware ID string.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiDeviceId {
    /// e.g. `b"PNP0501\0"` (16550 UART), null-padded.
    pub hid: [u8; 8],
}

const _: () = assert!(core::mem::size_of::<AcpiDeviceId>() == 8);

/// I/O port match entry: port address range.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoPortDeviceId {
    pub base: u16,
    pub size: u16,
    pub _pad: u32,
}

const _: () = assert!(core::mem::size_of::<IoPortDeviceId>() == 8);

// =============================================================================
// Device identity (used in DeviceEvent)
// =============================================================================

/// PCI device address: segment/bus/device/function.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciAddress {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub _pad: [u8; 3],
}

const _: () = assert!(core::mem::size_of::<PciAddress>() == 8);

/// USB device address: bus number + device address.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbAddress {
    pub bus: u8,
    pub address: u8,
    pub _pad: [u8; 6],
}

const _: () = assert!(core::mem::size_of::<UsbAddress>() == 8);

/// ACPI device address: null-padded ACPI object path string.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPath {
    pub path: [u8; 32],
}

const _: () = assert!(core::mem::size_of::<AcpiPath>() == 32);

/// I/O port device address: base port + size.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoPortAddress {
    pub base: u16,
    pub size: u16,
    pub _pad: u32,
}

const _: () = assert!(core::mem::size_of::<IoPortAddress>() == 8);

/// Per-bus device identity, selected by `DeviceEvent::bus_type`.
///
/// A union rather than an enum because `DeviceEvent` must be a fixed-size,
/// `#[repr(C)]` struct usable in both kernel and userspace without a
/// discriminant-dependent layout.
#[repr(C)]
#[derive(Clone, Copy)]
pub union DeviceIdentity {
    pub pci: PciAddress,
    pub usb: UsbAddress,
    pub acpi: AcpiPath,
    pub ioport: IoPortAddress,
}

const _: () = assert!(core::mem::size_of::<DeviceIdentity>() == 32);

/// An opaque handle value, as passed across the syscall ABI.
///
/// Device tokens are just handle IDs (see `panda_abi::WellKnownHandle` for
/// the handle encoding). A zero token means "no token" (used for
/// `EVENT_DEVICE_REMOVED`, which carries no claim capability).
pub type Handle = u64;

/// Event payload for device arrival/removal, delivered alongside
/// `EVENT_DEVICE_ADDED` / `EVENT_DEVICE_REMOVED`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DeviceEvent {
    pub bus_type: BusType,
    pub _pad: [u8; 4],
    /// Per-bus identity; read the union arm matching `bus_type`.
    pub identity: DeviceIdentity,
    /// Opaque, single-use claim token. Zero for `EVENT_DEVICE_REMOVED`.
    pub token: Handle,
}

const _: () = assert!(core::mem::size_of::<DeviceEvent>() == 48);

// =============================================================================
// Mailbox event constants
// =============================================================================

/// A device matching an active subscription has appeared (or was already
/// present at subscribe time — see subscription replay).
pub const EVENT_DEVICE_ADDED: u32 = 1 << 6;
/// A previously-added device has been removed.
pub const EVENT_DEVICE_REMOVED: u32 = 1 << 7;
/// A claimed device's subscribed IRQ has fired.
pub const EVENT_DEVICE_IRQ: u32 = 1 << 8;

// =============================================================================
// Operation codes
//
// Device operations (0xA_0000 - 0xA_FFFF). Defined here for ABI completeness;
// only OP_DEVICE_SUBSCRIBE and OP_DEVICE_CLAIM have kernel handlers today
// (see panda-kernel/src/syscall/device.rs). The rest are reserved pending the
// IOMMU-dependent Phase 6 work.
// =============================================================================

/// Subscribe to device add/remove events for a bus type + match filter:
/// `(bus_type: u32, match_data: *const u8, len) -> subscription handle`.
/// Immediately replays `EVENT_DEVICE_ADDED` for already-present matches.
pub const OP_DEVICE_SUBSCRIBE: u32 = 0xA_0000;
/// Claim a device using a token received via `EVENT_DEVICE_ADDED`:
/// `(device_token: Handle) -> owned device handle`. Consumes the token.
pub const OP_DEVICE_CLAIM: u32 = 0xA_0001;
/// Map a claimed device's MMIO BAR into the caller's address space:
/// `(device_handle, bar_index: u32) -> *const u8`. Not yet implemented
/// (requires IOMMU-aware cleanup, Phase 6).
pub const OP_DEVICE_MAP_MMIO: u32 = 0xA_0002;
/// Allocate IOMMU-mapped DMA memory for a claimed device:
/// `(device_handle, size: usize) -> (virt_addr, iova)`. Not yet implemented.
pub const OP_DMA_ALLOC: u32 = 0xA_0003;
/// Free memory allocated by `OP_DMA_ALLOC`:
/// `(device_handle, virt_addr, size) -> ()`. Not yet implemented.
pub const OP_DMA_FREE: u32 = 0xA_0004;
/// Subscribe to IRQ events for a claimed device:
/// `(device_handle, mailbox_handle) -> ()`. Not yet implemented.
pub const OP_DEVICE_SUBSCRIBE_IRQ: u32 = 0xA_0005;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pci_device_id_size() {
        assert_eq!(core::mem::size_of::<PciDeviceId>(), 12);
    }

    #[test]
    fn usb_device_id_size() {
        assert_eq!(core::mem::size_of::<UsbDeviceId>(), 12);
    }

    #[test]
    fn acpi_device_id_size() {
        assert_eq!(core::mem::size_of::<AcpiDeviceId>(), 8);
    }

    #[test]
    fn ioport_device_id_size() {
        assert_eq!(core::mem::size_of::<IoPortDeviceId>(), 8);
    }

    #[test]
    fn pci_wildcard_matching() {
        let any_vendor = PciDeviceId {
            vendor_id: PCI_MATCH_ANY,
            device_id: 0x1052,
            class: 0,
            class_mask: 0,
        };
        assert!(any_vendor.matches(0x1AF4, 0x1052, 0));
        assert!(any_vendor.matches(0xDEAD, 0x1052, 0));
        assert!(!any_vendor.matches(0xDEAD, 0x1053, 0));

        let ignore_class = PciDeviceId {
            vendor_id: 0x1AF4,
            device_id: 0x1052,
            class: 0x0900_00,
            class_mask: 0,
        };
        assert!(ignore_class.matches(0x1AF4, 0x1052, 0xFFFFFF));

        let match_all = PciDeviceId {
            vendor_id: PCI_MATCH_ANY,
            device_id: PCI_MATCH_ANY,
            class: 0,
            class_mask: 0,
        };
        assert!(match_all.matches(0x1234, 0x5678, 0xABCDEF));
    }
}
