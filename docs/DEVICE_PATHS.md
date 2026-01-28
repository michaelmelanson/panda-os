# Device Paths

Panda OS uses a unified device path scheme that provides human-friendly, stable device identification across all resource schemes.

## Path Structure

All device schemes share a unified path namespace based on PCI device classes:

```
scheme:/pci/<class>/<index>
```

Where:
- `scheme` determines the interface type (block, keyboard, surface, etc.)
- `class` is a human-readable PCI class name (storage, input, display, etc.)
- `index` is the zero-based device index within that class

## PCI Class Names

| Class Code | Name | Description |
|------------|------|-------------|
| 0x01 | `storage` | Mass storage (SATA, NVMe, virtio-blk) |
| 0x02 | `network` | Network controllers (Ethernet, virtio-net) |
| 0x03 | `display` | Display controllers (GPU, virtio-gpu) |
| 0x04 | `multimedia` | Audio, video capture |
| 0x09 | `input` | Input devices (keyboard, mouse, gamepad) |

## Examples

```
keyboard:/pci/input/0       # First input device, opened as keyboard
block:/pci/storage/0        # First storage device, opened as block device
surface:/pci/display/0      # First display, opened as surface

# Legacy address format still supported
block:/pci/00:04.0          # By raw PCI address
```

## Cross-Scheme Discovery with `*:`

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

All schemes use shared path resolution via `device_path::resolve()`.

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

## Summary

| URI Pattern | Meaning |
|-------------|---------|
| `scheme:/pci/class/index` | Open device by class/index with specific interface |
| `scheme:/pci/BB:DD.F` | Open device by PCI address (legacy) |
| `*:/pci` | List device classes |
| `*:/pci/class` | List devices in class |
| `*:/pci/class/index` | List schemes supporting device |
