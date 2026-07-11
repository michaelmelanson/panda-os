//! Device address abstraction for bus-agnostic device identification.
//!
//! Currently only PCI devices are supported:
//! - PCI: bus:device.function (e.g., "00:03.0")

use core::fmt;

/// Universal device address - can represent any bus type
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceAddress {
    /// PCI device: bus:device.function (e.g., "00:03.0")
    Pci { bus: u8, device: u8, function: u8 },
}

impl DeviceAddress {
    /// Parse a raw PCI BDF (bus:device.function) address, e.g. "00:03.0".
    pub fn parse_bdf(addr: &str) -> Option<Self> {
        let (bus_str, rest) = addr.split_once(':')?;
        let (device_str, function_str) = rest.split_once('.')?;

        let bus = u8::from_str_radix(bus_str, 16).ok()?;
        let device = u8::from_str_radix(device_str, 16).ok()?;
        let function = u8::from_str_radix(function_str, 16).ok()?;

        Some(DeviceAddress::Pci { bus, device, function })
    }
}

impl fmt::Display for DeviceAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceAddress::Pci { bus, device, function } => {
                write!(f, "pci/{:02x}:{:02x}.{:x}", bus, device, function)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bdf() {
        let addr = DeviceAddress::parse_bdf("00:03.0").unwrap();
        assert_eq!(addr, DeviceAddress::Pci { bus: 0, device: 3, function: 0 });
    }

    #[test]
    fn test_parse_bdf_invalid() {
        assert!(DeviceAddress::parse_bdf("").is_none());
        assert!(DeviceAddress::parse_bdf("00").is_none());
        assert!(DeviceAddress::parse_bdf("gg:03.0").is_none());
    }

    #[test]
    fn test_display() {
        let pci = DeviceAddress::Pci { bus: 0, device: 3, function: 0 };
        assert_eq!(format!("{}", pci), "pci/00:03.0");
    }
}
