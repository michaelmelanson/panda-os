# Device drivers

Panda's device drivers run in userspace, as ordinary processes spawned by
the service manager (`userspace/init/`). This document describes the
landed model: what a driver process's ELF metadata looks like, the syscalls
it uses to discover and own hardware, and the lifecycle it follows from
process start to device removal. See `plans/device-driver-model.md` for the
original design and a record of where the implementation deviated from it.

## 1. Overview

The model is driver-centric and subscription-based: a driver process
declares (via ELF metadata, not code that has to run first) which devices
it can handle, subscribes to that device class at startup, and reacts to
`EVENT_DEVICE_ADDED`/`EVENT_DEVICE_REMOVED` mailbox notifications for the
rest of its life. Hot-plug is the default case, not a special one — boot-time
enumeration is just the first batch of `EVENT_DEVICE_ADDED` notifications a
subscriber receives, replayed immediately at subscribe time so a driver
that starts after its device was already enumerated doesn't miss it (see
"Subscription replay" below).

The kernel never runs driver code to find out what a driver handles. It
reads a binary's ELF sections directly.

## 2. ELF device table format

A driver's device-match tables live in named ELF sections, one per bus
type:

```
.panda_devices.pci
.panda_devices.usb
.panda_devices.acpi
.panda_devices.ioport
```

Each section is a flat array of fixed-size `#[repr(C)]` match structs
(`PciDeviceId`, `UsbDeviceId`, `AcpiDeviceId`, `IoPortDeviceId` —
`panda-abi/src/device.rs`). Fixed size means no parsing is needed to read
the table: `entry_count = section_size / size_of::<T>()`. This is what lets
the kernel/service-manager-side scanner (`panda_elf::read_section`) treat
every driver binary the same way regardless of what language or build
system produced it, as long as it emits a correctly-named, correctly-sized
section.

`panda_elf::read_section(bytes, name) -> Option<&[u8]>` returns the raw
section bytes for a named section, or `None` if the section is absent or
the ELF is truncated/malformed. It performs no allocation and does not
execute the binary.

## 3. Declaring device tables

`userspace/libpanda/src/device.rs` provides one macro per bus type:
`pci_device_table!`, `usb_device_table!`, `acpi_device_table!`,
`ioport_device_table!`. Each emits a `#[link_section = ".panda_devices.<bus>"]`
static array of the corresponding match struct.

```rust
libpanda::pci_device_table![
    { vendor: 0x1AF4, device: 0x1052 },
];
```

Wildcards: `vendor_id`/`device_id` of `0xFFFF` (`PCI_MATCH_ANY`) match any
value; `class_mask == 0` ignores the class code entirely. The service
manager's own boot-time subscriptions use full wildcards (`OP_DEVICE_SUBSCRIBE`
with an all-`0xFFFF`/`0` match struct) to hear about every device on a bus,
since it isn't itself a driver — it's what decides which driver to spawn.

A driver can declare tables for more than one bus type (e.g. a device with
both a PCI and an ACPI binding) by invoking more than one `*_device_table!`
macro; each emits its own section.

## 4. Driver lifecycle

1. Process starts (spawned by the service manager once, when a matching
   device first appears — see §10).
2. Create a mailbox (`Mailbox::new`).
3. `device_subscribe(bus_type, match_bytes, mailbox_handle)`. This
   immediately replays `EVENT_DEVICE_ADDED` for every currently-known
   device that matches, so a driver spawned after its device was already
   enumerated (e.g. respawned after a crash) doesn't need special-case
   startup logic — subscribe-then-wait is the only code path.
4. Wait on the mailbox. On `EVENT_DEVICE_ADDED`:
   a. `device_poll_event(subscription_handle)` to read the actual
      `DeviceEvent` (bus type, identity, claim token) — see §6 for why this
      is a separate call from the mailbox wakeup.
   b. `device_claim(token)` — consumes the token, returns an owned
      `Device` handle.
   c. `device_map_mmio(device, bar_index)` for each BAR the driver needs.
   d. `dma_alloc(device, size)` for any DMA-visible buffers (e.g. a virtio
      ring).
   e. `device_subscribe_irq(device, mailbox_handle)` to have
      `EVENT_DEVICE_IRQ` posted to the same mailbox.
   f. Register the driver's scheme (`OP_SCHEME_REGISTER`, e.g.
      `SchemeProvider::register("keyboard")`) — only after every step above
      succeeds, so the driver never advertises a scheme it can't actually
      serve.
5. Serve scheme clients and drain `EVENT_DEVICE_IRQ` notifications for as
   long as the device is owned.
6. On `EVENT_DEVICE_REMOVED`: unregister the scheme, drop the `Device`
   handle (or let it go out of scope — see §5's "closing the handle is
   sufficient" note), and go back to waiting on the mailbox for the next
   `EVENT_DEVICE_ADDED`.

**Crash safety.** If the driver process crashes or exits without doing step
6 explicitly, the kernel's process-exit path still runs the same cleanup a
`Device` handle's `Drop` would: every MMIO mapping is unmapped, every DMA
allocation is freed and unmapped from the IOMMU, every IRQ subscription is
torn down, the IOMMU domain (if any) is destroyed, and
`DeviceRegistry::release` posts `EVENT_DEVICE_REMOVED` to subscribers. A
respawned driver (or the service manager, on its next scan) sees a clean
slate.

## 5. Syscall reference

All operate through `send(handle, op, arg0, arg1, arg2, arg3) -> isize`
(negative = `ErrorCode`). Wrappers live in `userspace/libpanda/src/device.rs`
(high-level) and `userspace/libpanda/src/sys/device.rs` (raw). Kernel
handlers are in `panda-kernel/src/syscall/device.rs`.

| Op | Args | Returns | Notes |
|----|------|---------|-------|
| `OP_DEVICE_SUBSCRIBE` | `bus_type: u32, match_ptr, match_len, mailbox_handle` | subscription handle | Attaches to `mailbox_handle` with `EVENT_DEVICE_ADDED\|EVENT_DEVICE_REMOVED` if non-zero. Replays `EVENT_DEVICE_ADDED` for existing matches. |
| `OP_DEVICE_POLL_EVENT` | `subscription_handle, out: *mut DeviceEvent` | `1` (event written) or `0` (queue empty) | Drains the subscription's pending-event queue one at a time. See §6. |
| `OP_DEVICE_CLAIM` | `device_token: Handle` | owned `Device` handle | Consumes the token (single-use). `InvalidHandle` if the token is unknown/already used/not owned by the caller; `Busy` if another process already claimed the device. |
| `OP_DEVICE_MAP_MMIO` | `device_handle, bar_index: u32, out: *mut MmioMapOut` | `0` | `out` filled with `{virt_addr, size}` — `size` is the BAR's real size, read from PCI config space. `InvalidArgument` for a bad BAR index or an I/O (non-memory) BAR. |
| `OP_DMA_ALLOC` | `device_handle, size: usize, out: *mut DmaAllocOut` | `0` | `out` filled with `{virt_addr, iova}`. Allocates a physically-contiguous buffer and maps it into the device's IOMMU domain. |
| `OP_DMA_FREE` | `device_handle, virt_addr, size` | `0` | Frees a buffer allocated by `OP_DMA_ALLOC` and removes its IOMMU mapping. |
| `OP_DEVICE_SUBSCRIBE_IRQ` | `device_handle, mailbox_handle` | `0` | Routes the device's interrupt to post `EVENT_DEVICE_IRQ` to `mailbox_handle`. See `DeviceResource::subscribe_irq`'s doc comment for the current backend's real limits (a small fixed pool of MSI-X vectors). |

Closing the `Device` handle (explicitly, or implicitly via process exit) is
sufficient for full cleanup — see §4's crash-safety note. There is no
separate "release device" call.

## 6. Mailbox events

| Event | Bit | Meaning |
|-------|-----|---------|
| `EVENT_DEVICE_ADDED` | `1 << 6` | A device matching an active subscription has appeared (or already existed at subscribe time). |
| `EVENT_DEVICE_REMOVED` | `1 << 7` | A previously-added device has been removed. |
| `EVENT_DEVICE_IRQ` | `1 << 8` | A claimed device's subscribed IRQ fired. |

A mailbox event is only a wakeup bit — `OP_MAILBOX_WAIT`/`OP_MAILBOX_POLL`
return `(handle_id, events: u32)`, not a payload. `EVENT_DEVICE_ADDED`/
`EVENT_DEVICE_REMOVED` need to carry a `DeviceEvent` (bus type, per-bus
identity, and — for `ADDED` — a single-use claim token), which doesn't fit
in an event bitmask. `OP_DEVICE_POLL_EVENT` is where that payload is
actually read: the subscription resource owns a small FIFO queue
(`resource::SubscriptionResource`) that `DeviceRegistry`'s replay/post-added/
post-removed paths push onto, and a driver drains it — typically once per
wakeup, but potentially more than once if several devices matched before
the driver got scheduled. `EVENT_DEVICE_REMOVED` events carry `token == 0`
(no claim capability associated with a removal).

This syscall is not in `plans/device-driver-model.md`'s original syscall
table — seting up the mailbox-bit-only convention without a payload-read
path was noticed to be a dead end only once a real driver
(`userspace/drivers/virtio-keyboard`) tried to call `device_claim` with
nothing to pass it. See that plan's Phase 6/7 landed-deviations note.

`EVENT_DEVICE_IRQ` carries no payload beyond the wakeup — a driver that
subscribed IRQs for exactly one device already knows which one fired.

## 7. Security model

A device token (`OP_DEVICE_SUBSCRIBE`'s replayed/posted `DeviceEvent::token`)
is not a device address or index — it's an opaque `u64` minted by
`DeviceRegistry` and bound to `(device_id, owner_pid)` at mint time.
`OP_DEVICE_CLAIM` checks two things independently before it succeeds: that
the token exists and belongs to the calling process (`InvalidHandle`
otherwise — the same error whether the token never existed, was already
consumed, or belongs to someone else, so a caller can't distinguish "wrong
token" from "someone else's still-valid token" by probing), and that no
other process has already claimed the underlying device
(`Busy`/`AlreadyClaimed` otherwise). A token is single-use: it's removed
from the registry the moment `OP_DEVICE_CLAIM` is called with it,
regardless of whether the claim itself succeeds.

`OP_DEVICE_CLAIM` deliberately does not accept a raw device address —
accepting one would let any process claim any device it could guess or
enumerate the address of, bypassing the subscription step entirely. A
process can only claim a device it was told about via its own subscription.

This is a capability, not a full authorisation system: any process can
currently call `OP_DEVICE_SUBSCRIBE` with a wildcard match and receive
tokens for every device on a bus. A future capability gate (referenced but
not built by this phase) would restrict which processes may subscribe to
which bus types/match patterns at all — e.g. via a manifest the service
manager checks before spawning a driver — without changing the token/claim
mechanism itself, since that mechanism already treats "does this caller
hold a valid token" as the sole authority to claim.

## 8. Bus type reference

| `BusType` | ELF section | Match struct | Identity fields |
|-----------|-------------|---------------|------------------|
| `Pci` (0) | `.panda_devices.pci` | `PciDeviceId { vendor_id, device_id, class, class_mask }` | `PciAddress { segment, bus, device, function }` |
| `Usb` (1) | `.panda_devices.usb` | `UsbDeviceId { vendor_id, product_id, device_class, device_subclass, device_protocol, match_flags }` | `UsbAddress { bus, address }` |
| `Acpi` (2) | `.panda_devices.acpi` | `AcpiDeviceId { hid: [u8; 8] }` | `AcpiPath { path: [u8; 32] }` |
| `IoPort` (3) | `.panda_devices.ioport` | `IoPortDeviceId { base, size }` | `IoPortAddress { base, size }` |

Only PCI devices are assigned an IOMMU domain today (`OP_DEVICE_CLAIM`'s
hardware isolation is PCI-specific, since it's built on BAR/MSI-X access);
other bus types claim successfully but get no DMA isolation from the
IOMMU.

## 9. Writing a driver

A minimal PCI driver, start to finish:

```rust
#![no_std]
#![no_main]

use libpanda::device::{self, BusType, EVENT_DEVICE_ADDED, EVENT_DEVICE_REMOVED, EVENT_DEVICE_IRQ};
use libpanda::mailbox::Mailbox;
use libpanda::scheme::SchemeProvider;

libpanda::pci_device_table![{ vendor: 0x1AF4, device: 0x1052 }];

#[unsafe(no_mangle)]
fn main() {
    let mailbox = Mailbox::new();
    let match_bytes = /* serialize a wildcard-free PciDeviceId matching the table above */;
    let subscription = device::device_subscribe(BusType::Pci, &match_bytes, mailbox.handle())
        .expect("subscribe");

    let mut provider: Option<SchemeProvider> = None;

    loop {
        let events = mailbox.wait();
        if events.contains(EVENT_DEVICE_ADDED) {
            while let Some(event) = device::device_poll_event(subscription).unwrap() {
                if event.token == 0 { continue; }
                let dev = device::device_claim(event.token).expect("claim");
                let mmio = device::device_map_mmio(dev, 0).expect("map bar0");
                // ... negotiate/initialise hardware using `mmio` ...
                device::device_subscribe_irq(dev, mailbox.handle()).expect("irq");
                provider = SchemeProvider::register("my-driver").ok();
                break;
            }
        }
        if events.contains(EVENT_DEVICE_IRQ) {
            // ... drain hardware state, serve pending client requests ...
        }
        if events.contains(EVENT_DEVICE_REMOVED) {
            provider = None; // dropping unregisters the scheme
            // dropping the claimed `Device` handle (out of scope here)
            // triggers full kernel-side cleanup.
        }
    }
}
```

`userspace/drivers/virtio-keyboard/src/main.rs` is the real, non-simplified
version of this pattern — see it for how PendingRead-style deferred replies,
virtio feature negotiation, and scheme-client serving fit together. Its
module doc comment also records the one piece of this driver that is not
yet real: the virtqueue (descriptor table/avail ring/used ring) is not
implemented, so `poll_device` never actually decodes hardware-provided
events yet, even though the claim/map/DMA/IRQ/scheme plumbing around it is
real and working.

## 10. Service manager integration

At startup, the service manager (`userspace/init/`) builds a
`DriverRegistry` by scanning driver binaries for `.panda_devices.*`
sections (`userspace/init/src/driver_registry.rs`) — first
`initrd:/drivers/` (available before any filesystem is mounted, backed by
the in-memory boot tar), then, once the root filesystem is mounted,
`file:/mnt/drivers/` via `DriverRegistry::extend`, which adds entries
without clearing the initrd-derived ones.

The manager itself subscribes to every bus type with an all-wildcard match
(one `OP_DEVICE_SUBSCRIBE` call per `BusType`) so it hears about every
device that appears, at any point in the boot sequence or afterwards. When
a device's `EVENT_DEVICE_ADDED` arrives, `driver_registry.find_driver(bus_type,
&device_event)` looks up which driver binary (if any) declared a matching
entry, and the manager spawns it — lazily, only when a matching device
shows up, with no per-driver TOML configuration required for the matching
itself. This is what makes hot-plug and boot-time enumeration the same code
path: whether a device was already present or shows up later, the manager
reacts to the same `EVENT_DEVICE_ADDED` notification either way.

TOML service configs (`plans/system-init-tool.md`) are still how
non-device-triggered services are described (what to run, restart policy,
environment) — device drivers found via ELF metadata are a specific case
that's spawned by device presence rather than declared explicitly, but once
spawned, are ordinary processes like any other service.
