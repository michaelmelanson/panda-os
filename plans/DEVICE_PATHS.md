# Unified Device Path Design

## Overview

This document describes a unified device path scheme for Panda OS that provides human-friendly, stable device identification across all resource schemes.

## Current State

Devices are currently identified by their PCI bus address:

```
block:/pci/00:04.0      # Block device at PCI address 00:04.0
keyboard:/pci/00:03.0   # Keyboard at PCI address 00:03.0
surface:/pci/00:01.0    # Display at PCI address 00:01.0
```

**Problems:**
- PCI addresses are not human-friendly
- Addresses change if hardware is added/removed or slots are reordered
- No way to discover available devices (must know the address)
- No way to discover what interfaces a device supports

## Design

### Path Structure

All device schemes share a unified path namespace based on PCI device classes:

```
scheme:/pci/<class>/<index>
```

Where:
- `scheme` determines the interface type (block, keyboard, surface, etc.)
- `class` is a human-readable PCI class name (storage, input, display, etc.)
- `index` is the zero-based device index within that class

### PCI Class Names

| Class Code | Name | Description |
|------------|------|-------------|
| 0x01 | `storage` | Mass storage (SATA, NVMe, virtio-blk) |
| 0x02 | `network` | Network controllers (Ethernet, virtio-net) |
| 0x03 | `display` | Display controllers (GPU, virtio-gpu) |
| 0x04 | `multimedia` | Audio, video capture |
| 0x09 | `input` | Input devices (keyboard, mouse, gamepad) |

### Examples

```
keyboard:/pci/input/0       # First input device, opened as keyboard
block:/pci/storage/0        # First storage device, opened as block device
surface:/pci/display/0      # First display, opened as surface

# Legacy address format still supported
block:/pci/00:04.0          # By raw PCI address
```

### Cross-Scheme Discovery with `*:`

The special `*:` scheme prefix queries across all schemes without opening a device.

**List device classes:**
```
readdir("*:/pci")
// Returns: ["storage", "display", "input"]
```

**List devices in a class:**
```
readdir("*:/pci/storage")
// Returns: ["0", "1"]  (two storage devices)
```

**List schemes that support a device:**
```
readdir("*:/pci/storage/0")
// Returns: ["block"]

readdir("*:/pci/input/0")
// Returns: ["keyboard"]

// Future: a device might support multiple interfaces
readdir("*:/pci/multimedia/0")
// Returns: ["audio", "video"]
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                 Shared Path Resolution                       │
│  device_path::resolve("/pci/storage/0") → DeviceAddress     │
│  device_path::list("/pci") → ["storage", "display", ...]    │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌──────────────┐      ┌──────────────┐      ┌──────────────┐
│ BlockScheme  │      │KeyboardScheme│      │SurfaceScheme │
│              │      │              │      │              │
│ Uses shared  │      │ Uses shared  │      │ Uses shared  │
│ path resolve │      │ path resolve │      │ path resolve │
│              │      │              │      │              │
│ Returns      │      │ Returns      │      │ Returns      │
│ BlockDevice  │      │ EventSource  │      │ Surface      │
└──────────────┘      └──────────────┘      └──────────────┘
```

### Path Resolution

All schemes use shared path resolution:

```rust
mod device_path {
    /// Resolve a device path to a DeviceAddress.
    pub fn resolve(path: &str) -> Option<DeviceAddress> {
        let path = path.strip_prefix('/').unwrap_or(path);
        
        if let Some(rest) = path.strip_prefix("pci/") {
            resolve_pci(rest)
        } else {
            None
        }
    }
    
    fn resolve_pci(path: &str) -> Option<DeviceAddress> {
        // Try class/index format: "storage/0"
        if let Some((class_name, index_str)) = path.split_once('/') {
            let class_code = class_code(class_name)?;
            let index: usize = index_str.parse().ok()?;
            return pci::get_device_by_class(class_code, index);
        }
        
        // Fall back to raw address: "00:04.0"
        DeviceAddress::from_pci_bdf(path)
    }
}
```

### Cross-Scheme Query

The `*:` prefix is handled specially:

```rust
pub async fn open_dir(uri: &str) -> Option<Box<dyn Resource>> {
    if let Some(path) = uri.strip_prefix("*:") {
        return open_cross_scheme_dir(path).await;
    }
    scheme::opendir(uri).await
}

async fn open_cross_scheme_dir(path: &str) -> Option<Box<dyn Resource>> {
    // If path resolves to a device, list supporting schemes
    if let Some(address) = device_path::resolve(path) {
        let schemes = find_schemes_for_device(&address);
        return Some(Box::new(DirectoryResource::new(schemes)));
    }
    
    // Otherwise, list the path hierarchy
    let entries = device_path::list(path)?;
    Some(Box::new(DirectoryResource::new(entries)))
}

fn find_schemes_for_device(address: &DeviceAddress) -> Vec<&'static str> {
    let mut result = Vec::new();
    if virtio_block::has_device(address) { result.push("block"); }
    if virtio_keyboard::has_device(address) { result.push("keyboard"); }
    if virtio_gpu::has_device(address) { result.push("surface"); }
    result
}
```

## Usage Examples

### Device Discovery

```rust
// List all device classes
for class in readdir("*:/pci") {
    // List devices in each class
    for device in readdir(&format!("*:/pci/{}", class.name())) {
        let path = format!("pci/{}/{}", class.name(), device.name());
        
        // What schemes support this device?
        let schemes = readdir(&format!("*:/{}", path));
        println!("{}: {:?}", path, schemes);
    }
}
// Output:
// pci/storage/0: ["block"]
// pci/display/0: ["surface"]
// pci/input/0: ["keyboard"]
```

### Opening Devices

```rust
// Open first storage device as block device
let disk = open("block:/pci/storage/0")?;

// Open first input device as keyboard
let kbd = open("keyboard:/pci/input/0")?;

// Open first display as surface
let screen = open("surface:/pci/display/0")?;
```

### Init Process

```rust
// Mount filesystem from first storage device
mount("block:/pci/storage/0", "/mnt", "ext2")?;

// Open keyboard for input
let kbd = open("keyboard:/pci/input/0")?;
```

## Future Extensions

### Additional Identification Methods

The path structure can be extended with other identification schemes:

```
block:/uuid/3e4a5f6b-...      # By filesystem UUID
block:/label/my-disk          # By filesystem label
block:/serial/QM00001         # By device serial number
```

These would be additional path prefixes alongside `/pci/`:

```rust
fn resolve(path: &str) -> Option<DeviceAddress> {
    if let Some(rest) = path.strip_prefix("pci/") {
        resolve_pci(rest)
    } else if let Some(rest) = path.strip_prefix("uuid/") {
        resolve_uuid(rest)
    } else if let Some(rest) = path.strip_prefix("label/") {
        resolve_label(rest)
    } else {
        None
    }
}
```

### Non-PCI Devices

The same pattern works for non-PCI devices:

```
keyboard:/usb/0               # USB keyboard
block:/nvme/0                 # NVMe drive (has its own bus)
serial:/isa/com1              # Legacy serial port
```

## Implementation Plan

1. **Add PCI class tracking** - Store class code during PCI enumeration
2. **Add `pci::get_device_by_class()`** - Query devices by class and index
3. **Create `device_path` module** - Shared path resolution logic
4. **Update scheme handlers** - Use shared path resolution
5. **Implement `*:` scheme** - Cross-scheme directory queries
6. **Update userspace** - Use new paths in init, terminal, tests

## Summary

| URI Pattern | Meaning |
|-------------|---------|
| `scheme:/pci/class/index` | Open device by class/index with specific interface |
| `scheme:/pci/BB:DD.F` | Open device by PCI address (legacy) |
| `*:/pci` | List device classes |
| `*:/pci/class` | List devices in class |
| `*:/pci/class/index` | List schemes supporting device |
