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

## Exclusive ownership

Some devices are exclusively claimable: the kernel's claim table
(`panda-kernel/src/devices/claims.rs`) allows at most one owner per device
address at a time. An open that loses the race fails with `Busy`
(`ErrorCode::Busy`); the claim is released when the owning handle is closed
or its process exits (RAII — the claim guard lives inside the resource).

| Operation | Claim |
|-----------|-------|
| `open("surface:/fb0")` | Claims the display device; a second concurrent open fails `Busy` |
| `mount("ext2", ...)` | Claims the backing block device for the lifetime of the mount |
| `open("block:/pci/storage/N")` | Claims the block device; fails `Busy` if it is mounted or already open |

Note: the in-kernel compositor currently accesses the framebuffer without a
claim; that bypass closes when the compositor moves to userspace (see
`plans/userspace-compositor.md`).
