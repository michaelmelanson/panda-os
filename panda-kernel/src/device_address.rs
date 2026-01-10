//! Device address abstraction for bus-agnostic device identification.
//!
//! Different bus types use different addressing schemes:
//! - PCI: bus:device.function (e.g., "00:03.0")
//! - ISA: port number (e.g., "0x60" for PS/2 keyboard)
//! - USB: bus-port.port... (e.g., "1-2.3")

use alloc::vec::Vec;
use core::fmt;

/// Universal device address - can represent any bus type
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceAddress {
    /// PCI device: bus:device.function (e.g., "00:03.0")
    Pci { bus: u8, device: u8, function: u8 },
    /// ISA/legacy port (e.g., "0x60" for PS/2 keyboard)
    Isa { port: u16 },
    /// USB device: bus-port.port... (e.g., "1-2.3")
    Usb { bus: u8, ports: Vec<u8> },
}

impl DeviceAddress {
    /// Parse from path component like "/pci/00:03.0" or "/isa/0x60"
    pub fn from_path(path: &str) -> Option<Self> {
        let path = path.strip_prefix('/')?;
        let (bus_type, addr) = path.split_once('/')?;
        match bus_type {
            "pci" => Self::parse_pci(addr),
            "isa" => Self::parse_isa(addr),
            "usb" => Self::parse_usb(addr),
            _ => None,
        }
    }

    /// Parse PCI address like "00:03.0"
    fn parse_pci(addr: &str) -> Option<Self> {
        let (bus_str, rest) = addr.split_once(':')?;
        let (device_str, function_str) = rest.split_once('.')?;

        let bus = u8::from_str_radix(bus_str, 16).ok()?;
        let device = u8::from_str_radix(device_str, 16).ok()?;
        let function = u8::from_str_radix(function_str, 16).ok()?;

        Some(DeviceAddress::Pci { bus, device, function })
    }

    /// Parse ISA port like "0x60" or "60"
    fn parse_isa(addr: &str) -> Option<Self> {
        let port = if let Some(hex) = addr.strip_prefix("0x") {
            u16::from_str_radix(hex, 16).ok()?
        } else {
            u16::from_str_radix(addr, 16).ok()?
        };
        Some(DeviceAddress::Isa { port })
    }

    /// Parse USB address like "1-2.3" (bus 1, port 2, subport 3)
    fn parse_usb(addr: &str) -> Option<Self> {
        let (bus_str, ports_str) = addr.split_once('-')?;
        let bus = bus_str.parse().ok()?;
        let ports: Result<Vec<u8>, _> = ports_str.split('.').map(|s| s.parse()).collect();
        Some(DeviceAddress::Usb { bus, ports: ports.ok()? })
    }

    /// Check if this is a PCI address
    pub fn is_pci(&self) -> bool {
        matches!(self, DeviceAddress::Pci { .. })
    }

    /// Get PCI address components if this is a PCI address
    pub fn as_pci(&self) -> Option<(u8, u8, u8)> {
        match self {
            DeviceAddress::Pci { bus, device, function } => Some((*bus, *device, *function)),
            _ => None,
        }
    }
}

impl fmt::Display for DeviceAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceAddress::Pci { bus, device, function } => {
                write!(f, "pci/{:02x}:{:02x}.{:x}", bus, device, function)
            }
            DeviceAddress::Isa { port } => {
                write!(f, "isa/0x{:x}", port)
            }
            DeviceAddress::Usb { bus, ports } => {
                write!(f, "usb/{}-", bus)?;
                for (i, port) in ports.iter().enumerate() {
                    if i > 0 {
                        write!(f, ".")?;
                    }
                    write!(f, "{}", port)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pci() {
        let addr = DeviceAddress::from_path("/pci/00:03.0").unwrap();
        assert_eq!(addr, DeviceAddress::Pci { bus: 0, device: 3, function: 0 });
    }

    #[test]
    fn test_parse_isa() {
        let addr = DeviceAddress::from_path("/isa/0x60").unwrap();
        assert_eq!(addr, DeviceAddress::Isa { port: 0x60 });
    }

    #[test]
    fn test_parse_usb() {
        let addr = DeviceAddress::from_path("/usb/1-2.3").unwrap();
        assert_eq!(addr, DeviceAddress::Usb { bus: 1, ports: vec![2, 3] });
    }

    #[test]
    fn test_display() {
        let pci = DeviceAddress::Pci { bus: 0, device: 3, function: 0 };
        assert_eq!(format!("{}", pci), "pci/00:03.0");

        let isa = DeviceAddress::Isa { port: 0x60 };
        assert_eq!(format!("{}", isa), "isa/0x60");

        let usb = DeviceAddress::Usb { bus: 1, ports: vec![2, 3] };
        assert_eq!(format!("{}", usb), "usb/1-2.3");
    }
}
