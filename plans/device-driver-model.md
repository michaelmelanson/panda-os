# Userspace device driver model

> **Naming re-scope (`plans/ROADMAP.md` M2, "typed schemes" decision).**
> Wherever this plan says `OP_SERVICE_REGISTER("keyboard")` /
> `service:/keyboard`, read: the driver registers a *scheme* via
> `OP_SCHEME_REGISTER` (roadmap M2.2) — e.g. the keyboard driver registers
> `keyboard:` (taking over the name from the kernel driver it replaces) and
> clients keep opening `keyboard:/...` unchanged. There is no `service:`
> namespace. Discovery is `readdir("scheme:/")`.

## Prerequisites

- IOMMU support (`plans/iommu.md`) — must be complete before Phase 6. Phases 1–5 can proceed in parallel.
- Service protocol framework (`plans/system-init-tool.md` Phase 3) — drivers register their schemes after initialisation (see naming re-scope above).

## Problem

All device drivers currently run in the kernel. Moving them to userspace requires:

1. A way for drivers to declare what devices they handle without running code
2. A mechanism for the kernel to notify drivers when devices appear or disappear
3. Syscalls for a userspace process to claim a device and access its hardware (MMIO, DMA, IRQs)
4. Kernel enforcement that a claimed device's DMA cannot reach memory outside its IOMMU domain

This plan covers pieces 1, 2, and 3. Piece 4 is covered by the IOMMU plan.

## Goals

1. Device drivers are self-describing ELF binaries: device match tables are stored in named ELF sections, readable without executing the binary.
2. The model is bus-agnostic: PCI, USB, ACPI, I/O port, and future bus types share the same infrastructure with per-bus match structs and section names.
3. Hot-plug is the default case. A driver process starts, subscribes to device events, and reacts when devices appear or disappear. Boot-time enumeration is identical to hot-plug.
4. The service manager spawns drivers lazily — only when a matching device is added — without needing per-driver TOML configuration for device matching.
5. The design is structurally compatible with a future capability-based authorisation system; no aspect of the model forecloses adding capability gates later.
6. The first concrete userspace driver (virtio keyboard) validates the full stack end-to-end, replacing the kernel driver in an atomic cutover.

## Design

### ELF metadata format

Each driver binary contains one or more named ELF sections, one per bus type it handles. The section name encodes the bus type:

```
.panda_devices.pci
.panda_devices.usb
.panda_devices.acpi
.panda_devices.ioport
```

Each section is a flat C array of fixed-size match structs. Fixed size means reading the table requires no parsing: `entry_count = section_size / size_of::<T>()`. The structs are defined in `panda-abi` and are `#[repr(C)]` for stable layout.

```rust
// PCI: match on vendor/device ID and/or class code
#[repr(C)]
pub struct PciDeviceId {
    pub vendor_id:  u16,   // 0xFFFF = wildcard
    pub device_id:  u16,   // 0xFFFF = wildcard
    pub class:      u32,   // device class code
    pub class_mask: u32,   // 0x000000 = ignore class entirely
}

// USB: match on vendor/product ID and/or interface class
#[repr(C)]
pub struct UsbDeviceId {
    pub vendor_id:        u16,   // 0xFFFF = wildcard
    pub product_id:       u16,   // 0xFFFF = wildcard
    pub device_class:     u8,
    pub device_subclass:  u8,
    pub device_protocol:  u8,
    pub match_flags:      u8,    // bitmask: which fields are active
    pub _pad:             u32,
}

// ACPI: match on hardware ID string
#[repr(C)]
pub struct AcpiDeviceId {
    pub hid: [u8; 8],   // e.g. b"PNP0501\0" (16550 UART), null-padded
}

// I/O port: match on port address range
#[repr(C)]
pub struct IoPortDeviceId {
    pub base: u16,
    pub size: u16,
    pub _pad: u32,
}
```

Drivers declare their tables using macros that emit the correct section:

```rust
// In a virtio keyboard driver:
panda::pci_device_table![
    { vendor: 0x1AF4, device: 0x1052 },
];

// In a USB HID driver:
panda::usb_device_table![
    { vendor: 0xFFFF, product: 0xFFFF,
      device_class: 0x03, match_flags: USB_MATCH_CLASS },
];

// A driver can declare multiple bus types:
panda::pci_device_table![{ vendor: 0x1AF4, device: 0x1050 }];
panda::acpi_device_table![{ hid: b"PNP0501\0" }];
```

### Security and capability compatibility

The model is designed so that a capability-based authorisation system can be layered on without structural changes. Two properties make this possible now:

**Device tokens are opaque handles, not raw addresses.** When the kernel posts `EVENT_DEVICE_ADDED`, the event payload includes an opaque one-time-use `Handle` (the device token) alongside the informational device identity. `OP_DEVICE_CLAIM` takes this token handle — not a raw PCI address — and the kernel invalidates the token on use. A process cannot claim a device it has not been given a token for, even if it knows the device's address. Handles in panda are not forgeable (they are kernel-allocated integers in the calling process's handle table, not globally guessable).

**Subscriptions and tokens are the natural capability boundary.** A subscription handle is the capability to receive device arrival and removal events for a given device type. A device token is the capability to claim one specific device instance. In a future capability model:

- Creating a subscription would require holding a "device observation" capability for that bus type
- The service manager would hold a broad observation capability; individual driver processes would hold narrower ones
- Spawning a driver and passing it a device token is delegation: the service manager transfers its claim right to the driver
- The kernel already tracks handle ownership per-process, which is the foundation capability propagation requires

For now, any process may create a subscription and any process holding a valid device token may claim it. The future gates slot in at `OP_DEVICE_SUBSCRIBE` (check observation capability) and `OP_DEVICE_CLAIM` (check token validity — already enforced structurally). No API surface needs to change when those gates are added.

### Driver lifecycle

Drivers are long-lived processes decoupled from device lifetime. The sequence:

```
1. Service manager spawns driver binary (because a matching device was added)

2. Driver calls DEVICE_SUBSCRIBE(bus_type, match_data) → subscription handle
   Kernel immediately replays EVENT_DEVICE_ADDED for any already-present
   matching devices (eliminates start-order race)

3. Driver attaches handle to mailbox:
   mailbox.attach(sub_handle, EVENT_DEVICE_ADDED | EVENT_DEVICE_REMOVED)

4. Driver calls mailbox.recv() and blocks

5. EVENT_DEVICE_ADDED fires:
   payload contains { bus_type, device_info, device_token: Handle }

6. Driver calls DEVICE_CLAIM(device_token)
   → kernel grants exclusive ownership, invalidates token
   → kernel assigns device to an isolated IOMMU domain (IOMMU plan)
   → returns an owned device handle

7. Driver calls DEVICE_MAP_MMIO(device_handle, bar_index)
   → kernel maps device BAR into driver's virtual address space

8. Driver calls DMA_ALLOC(device_handle, size)
   → kernel allocates contiguous physical memory
   → kernel creates IOMMU mapping in device's domain
   → returns (virt_addr for CPU, iova for device)

9. Driver calls DEVICE_SUBSCRIBE_IRQ(device_handle, mailbox_handle)
   → future device interrupts arrive as EVENT_DEVICE_IRQ on the mailbox

10. Driver initialises device hardware (virtio feature negotiation, etc.)

11. Driver calls OP_SERVICE_REGISTER("keyboard")
    → driver is now reachable as service:/keyboard

12. Driver serves client requests via channels while handling
    EVENT_DEVICE_IRQ events from the mailbox

13. EVENT_DEVICE_REMOVED fires:
    → driver unregisters from service scheme
    → driver closes device_handle (kernel unmaps MMIO, frees DMA buffers,
      removes IRQ subscription, releases IOMMU domain)
    → driver returns to step 4, ready to handle the next device
```

Closing the device handle is the single cleanup action — the kernel tears down all resources associated with that handle atomically. The driver does not need to call separate teardown syscalls for MMIO, DMA, and IRQ; closing the handle is sufficient. This also means a crashed driver is cleaned up correctly: when the process exits, all its handles are closed by the kernel.

### Subscription replay

When a driver calls `DEVICE_SUBSCRIBE` after the kernel has already enumerated devices, the kernel immediately posts `EVENT_DEVICE_ADDED` for every currently-present device matching the subscription. This eliminates start-order dependencies: a driver that starts late gets the same events as one that starts before enumeration.

### Service manager role

The service manager gains a driver registry built from ELF metadata rather than TOML configuration:

1. At boot, scan `file:/drivers/` for driver binaries
2. For each binary, call `panda_elf::read_section` for each known section name
3. Build per-bus registries: `(vendor_id, device_id) → binary_path` etc.
4. Subscribe to all device events (wildcard subscription)
5. On `EVENT_DEVICE_ADDED`: look up registry → spawn driver, passing device token via startup channel message
6. Spawned driver receives token, calls `DEVICE_SUBSCRIBE` (gets replay if needed), calls `DEVICE_CLAIM(token)`

Driver binaries in `file:/drivers/` require no TOML config entry for device matching. A TOML config is still valid for restart policies, environment variables, or other service manager features — but the `[device]` section is gone from the schema entirely; matching is always from ELF metadata.

### Bus type reference

| Bus type   | Section name            | Match struct      | Identity in event        |
|------------|-------------------------|-------------------|--------------------------|
| PCI        | `.panda_devices.pci`    | `PciDeviceId`     | `PciAddress` (seg/bus/dev/fn) |
| USB        | `.panda_devices.usb`    | `UsbDeviceId`     | bus number + device address |
| ACPI       | `.panda_devices.acpi`   | `AcpiDeviceId`    | ACPI path string         |
| I/O port   | `.panda_devices.ioport` | `IoPortDeviceId`  | base port + size         |

Adding a new bus type requires: a new struct in `panda-abi`, a new macro in `libpanda`, a new section name in the service manager's scan list, and kernel-side event posting when the bus enumerates. No changes to existing bus types.

### New syscalls

| Syscall                  | Args                                        | Returns                  |
|--------------------------|---------------------------------------------|--------------------------|
| `OP_DEVICE_SUBSCRIBE`    | `bus_type: u32, match_data: *const u8, len` | subscription handle      |
| `OP_DEVICE_CLAIM`        | `device_token: Handle`                      | owned device handle      |
| `OP_DEVICE_MAP_MMIO`     | `device_handle, bar_index: u32`             | `*const u8` in caller's address space |
| `OP_DMA_ALLOC`           | `device_handle, size: usize`                | `(virt_addr, iova)` pair |
| `OP_DMA_FREE`            | `device_handle, virt_addr, size`            | —                        |
| `OP_DEVICE_SUBSCRIBE_IRQ`| `device_handle, mailbox_handle`             | —                        |

`OP_DEVICE_CLAIM` consumes and invalidates the device token. The kernel rejects a second claim attempt with the same token (already used) and rejects any claim by a process that never held the token (handle not in their table). When the owning process closes its device handle or exits, the kernel releases the claim and posts `EVENT_DEVICE_REMOVED` to all subscribers.

### New mailbox events

| Event constant       | Source                  | Payload                                    |
|----------------------|-------------------------|--------------------------------------------|
| `EVENT_DEVICE_ADDED` | device subscription     | `DeviceEvent` (bus type, device info, token handle) |
| `EVENT_DEVICE_REMOVED` | device subscription   | `DeviceEvent` (bus type, device info, no token)     |
| `EVENT_DEVICE_IRQ`   | `DEVICE_SUBSCRIBE_IRQ`  | IRQ vector number                          |

### Module layout

```
panda-abi/src/
  device.rs           -- BusType enum, PciDeviceId, UsbDeviceId, AcpiDeviceId,
                         IoPortDeviceId, DeviceEvent, DeviceInfo; new OP_ constants;
                         EVENT_DEVICE_ADDED, EVENT_DEVICE_REMOVED, EVENT_DEVICE_IRQ

crates/panda-elf/src/
  lib.rs              -- add read_section(bytes: &[u8], name: &str) -> Option<&[u8]>

userspace/libpanda/src/
  device.rs           -- pci_device_table![], usb_device_table![], acpi_device_table![],
                         ioport_device_table![] macros; device_subscribe(), device_claim(),
                         device_map_mmio(), dma_alloc(), dma_free(), device_subscribe_irq()

panda-kernel/src/
  resource/
    initrd.rs           -- InitrdScheme: read-only scheme backed by in-memory ustar archive;
                           open() and opendir() over archive entries without disk extraction
  device/
    mod.rs            -- device registry: known devices per bus type; claim table
    subscription.rs   -- SubscriptionRegistry: (BusType, match_bytes) → Vec<MailboxRef>;
                         subscribe(), replay_to_new_subscriber(), post_added(), post_removed()
    pci.rs            -- post_added events after PCI enumeration; wire into pci::init()
  syscall/
    device.rs         -- handle_device_subscribe(), handle_device_claim(),
                         handle_device_map_mmio(), handle_dma_alloc(), handle_dma_free(),
                         handle_device_subscribe_irq()

userspace/init/src/
  driver_registry.rs  -- scan file:/drivers/, read .panda_devices.* sections via panda_elf,
                         build per-bus BTreeMap<MatchKey, BinaryPath>; subscribe to all
                         device events; spawn driver + pass token on EVENT_DEVICE_ADDED

userspace/drivers/
  virtio-keyboard/
    src/main.rs       -- first userspace driver; validates full stack

docs/
  DEVICE_DRIVERS.md   -- new: userspace driver model reference (see Phase 8)
```

## Implementation plan

### Phase 1: panda-abi device types

Define all shared types so every subsequent phase can depend on them.

**Files:**
- `panda-abi/src/device.rs` — `BusType` enum (`Pci = 0, Usb = 1, Acpi = 2, IoPort = 3`); `PciDeviceId`, `UsbDeviceId`, `AcpiDeviceId`, `IoPortDeviceId` match structs (all `#[repr(C)]`); compile-time size assertions for each (`assert_eq!(size_of::<PciDeviceId>(), 8)` etc.); `PciAddress` struct; `DeviceEvent` struct with `bus_type: BusType`, per-bus identity union, and `token: Handle` (zero for `EVENT_DEVICE_REMOVED`); `EVENT_DEVICE_ADDED`, `EVENT_DEVICE_REMOVED`, `EVENT_DEVICE_IRQ` constants; `OP_DEVICE_SUBSCRIBE`, `OP_DEVICE_CLAIM`, `OP_DEVICE_MAP_MMIO`, `OP_DMA_ALLOC`, `OP_DMA_FREE`, `OP_DEVICE_SUBSCRIBE_IRQ` operation constants
- `panda-abi/src/lib.rs` — add `pub mod device;`

### Phase 2: ELF section reading

Extend `crates/panda-elf` with section lookup so the service manager can read driver metadata without executing binaries.

**Files:**
- `crates/panda-elf/src/lib.rs` — add `pub fn read_section<'a>(elf_bytes: &'a [u8], name: &str) -> Option<&'a [u8]>`: parse ELF64 header, iterate section headers, match `sh_name` against the string table, return the section's byte slice. Returns `None` if the section is absent or the ELF is malformed. No panics on malformed input.

**Tests:** correct section present returns correct bytes; absent section returns `None`; truncated ELF header returns `None`; wrong ELF magic returns `None`.

### Phase 3: Driver macro infrastructure

Add the `device_table!` macros to `libpanda` so driver authors can declare device IDs idiomatically.

**Files:**
- `userspace/libpanda/src/device.rs` — four macros using `#[link_section]` and `#[used]`:

```rust
#[macro_export]
macro_rules! pci_device_table {
    ($({ vendor: $v:expr, device: $d:expr }),+ $(,)?) => {
        #[link_section = ".panda_devices.pci"]
        #[used]
        static _PANDA_PCI_DEVICES: [$crate::device::PciDeviceId; ${count($v)}] = [
            $($crate::device::PciDeviceId {
                vendor_id: $v, device_id: $d,
                class: 0, class_mask: 0,
            }),+
        ];
    };
}
```

Analogous macros for `usb_device_table!`, `acpi_device_table!`, `ioport_device_table!`.

Also in `userspace/libpanda/src/device.rs`:

Raw syscall wrappers: `device_subscribe(bus_type, match_data) -> Result<Handle>`, `device_claim(token: Handle) -> Result<Handle>`, `device_map_mmio(device, bar) -> Result<MmioRegion>`, `dma_alloc(device, size) -> Result<(usize, u64)>`, `dma_free(device, virt, size)`, `device_subscribe_irq(device, mailbox) -> Result<()>`.

`MmioRegion` — a safe wrapper around the raw pointer returned by `DEVICE_MAP_MMIO`. Enforces bounds and ensures all accesses are volatile, making it impossible to form a Rust reference to device memory (which would violate aliasing rules and allow the compiler to cache reads):

```rust
pub struct MmioRegion {
    base: *mut u8,
    size: usize,
}

// Not Sync — concurrent MMIO access must be coordinated by the driver.
unsafe impl Send for MmioRegion {}

impl MmioRegion {
    pub fn size(&self) -> usize { self.size }

    /// Read a value from the given byte offset.
    /// Panics if the access would exceed the region's bounds.
    pub fn read<T: Copy>(&self, offset: usize) -> T {
        assert!(offset + core::mem::size_of::<T>() <= self.size);
        unsafe { core::ptr::read_volatile(self.base.add(offset) as *const T) }
    }

    /// Write a value to the given byte offset.
    /// Panics if the access would exceed the region's bounds.
    pub fn write<T: Copy>(&self, offset: usize, value: T) {
        assert!(offset + core::mem::size_of::<T>() <= self.size);
        unsafe { core::ptr::write_volatile(self.base.add(offset) as *mut T, value) }
    }
}
```

`MmioRegion::read` and `write` are the only way to access device registers in userspace driver code. The kernel's `handle_device_map_mmio` records the mapped size in the device record so `MmioRegion` can be constructed with the correct bounds.

### Phase 4: Kernel subscription registry

Implement the kernel-side subscription mechanism. Independent of IOMMU; can land before it.

**Files:**
- `panda-kernel/src/device/subscription.rs` — `SubscriptionRegistry`: `BTreeMap<(BusType, MatchBytes), Vec<MailboxRef>>`; `subscribe(bus_type, match_bytes, mailbox)`: adds entry, then calls `replay_to_new_subscriber`; `replay_to_new_subscriber(bus_type, match_bytes, mailbox)`: iterates currently-known devices, posts `EVENT_DEVICE_ADDED` (with a fresh token handle) for each match; `post_added(device_event)`: allocates a token handle per matching subscriber and posts the event; `post_removed(device_event)`: posts removal event (no token) to all subscribers regardless of whether they previously received an `EVENT_DEVICE_ADDED`
- `panda-kernel/src/device/mod.rs` — `DeviceRegistry`: `BTreeMap<(BusType, DeviceAddress), DeviceRecord>` tracking known devices and their claimed owner PID; `register(bus_type, address, info)` called during bus enumeration; `claim(token_handle, pid) -> Result<DeviceHandle>` validates token, marks exclusive ownership, invalidates token; `release(address)` on process exit — calls `subscription::post_removed`
- `panda-kernel/src/device/pci.rs` — after `pci::init()` enumeration, call `subscription::post_added` for each discovered device; remove inline virtio driver initialisation (atomic cutover: no fallback kernel init)
- `panda-kernel/src/syscall/device.rs` — `handle_device_subscribe`, `handle_device_claim`

### Phase 5: Service manager driver registry

Extend the service manager to scan driver binaries and react to device events. Drivers come from two sources: the initrd (available at boot) and the root filesystem (available after a block driver mounts it). This ordering resolves the chicken-and-egg dependency — the block driver itself must come from the initrd.

**Two-phase scanning:**

*Phase 5a — initrd drivers (at service manager startup, before any mount):*
- Scan `initrd:/drivers/` for driver binaries (`initrd:` is a kernel scheme backed by the in-memory ustar archive — see Risk 2)
- Build driver registry from ELF metadata
- Subscribe to all device events: call `OP_DEVICE_SUBSCRIBE(bus_type, all-wildcard)` once per known bus type (`Pci`, `Usb`, `Acpi`, `IoPort`)
- Process initial events — the kernel's subscription replay delivers `EVENT_DEVICE_ADDED` for each already-enumerated device, causing the right initrd drivers to be spawned
- At minimum, the virtio-blk driver must be in the initrd and must claim its device here before Phase 5b can proceed

*Phase 5b — root filesystem drivers (after root fs is mounted):*
- Service manager waits for `service:/block` to become available (the virtio-blk driver from Phase 5a registers it)
- Mounts root filesystem via the block service
- Scans `file:/mnt/drivers/` for additional driver binaries
- Adds new entries to the driver registry
- Subscription replay fires immediately for any device not yet claimed whose driver is now registered

The subscription replay mechanism handles the gap cleanly: devices that appeared during Phase 5a but had no matching initrd driver receive `EVENT_DEVICE_ADDED` again when a Phase 5b driver subscribes, with a fresh token.

**Files:**
- `panda-kernel/src/resource/initrd.rs` — `InitrdScheme`: kernel scheme handler backed by the ustar archive kept in memory after boot; `open("/{path}")` searches the archive for a matching entry and returns a read-only file resource; `opendir("/drivers/")` returns an iterator over archive entries under that prefix; no extraction to disk
- `panda-kernel/src/resource/scheme.rs` — register `initrd:` scheme in `init()`
- `userspace/init/src/driver_registry.rs` — `DriverRegistry`: scans a given path for binaries, reads all `.panda_devices.*` sections via `panda_elf::read_section`, builds per-bus `BTreeMap<MatchKey, String>` with wildcard handling; `extend(path)` adds entries from a second scan without clearing existing ones; `find_driver(bus_type, device_event) -> Option<&str>`
- `userspace/init/src/manager.rs` — Phase 5a at startup: construct `DriverRegistry`, scan `initrd:/drivers/`, subscribe per-bus (`Pci`/`Usb`/`Acpi`/`IoPort` with all-wildcard), process events; wait for block service; Phase 5b: mount root fs, call `driver_registry.extend("file:/mnt/drivers/")`, which triggers replay for newly matched devices via existing subscriptions

**Makefile:** add `drivers/` subdirectory to `build/run/initrd/`; include `virtio-keyboard` and (eventually) `virtio-blk` binaries; update `tar` invocation to include `drivers/` in the initrd archive

### Phase 6: Device ownership syscalls (requires IOMMU)

Implement the full device claim and hardware access syscalls.

**Files:**
- `panda-kernel/src/syscall/device.rs` — extend with:
  - `handle_device_claim`: validate token handle exists in caller's handle table and has not been used; call `iommu::manager::create_device_domain(address)` to assign an isolated IOMMU domain; record claim in `DeviceRegistry`; invalidate token handle; return owned device handle. The device handle acts as the root capability: all subsequent syscalls on this device require it.
  - `handle_device_map_mmio`: validate caller owns the device handle; read BAR from PCI config space; map BAR physical range into caller's page table using non-cacheable flags (PAT/PCD); return virtual address. Tracked in device record so it can be unmapped when handle is closed.
  - `handle_dma_alloc`: allocate physically contiguous memory via `DmaBuffer`; call `iommu::manager::dma_map(device, phys, size, READ_WRITE)`; return `(virt_addr, iova)`. Tracked in device record for cleanup.
  - `handle_dma_free`: call `iommu::manager::dma_unmap`; drop `DmaBuffer`; remove from device record.
  - `handle_device_subscribe_irq`: configure MSI-X or IOAPIC IRQ to post `EVENT_DEVICE_IRQ` to the given mailbox; store subscription in device record.
  - **Handle close / process exit path**: when a device handle is closed (explicitly or via process exit), the kernel: unmaps all MMIO regions mapped for this device, frees all DMA buffers and removes their IOMMU mappings, removes all IRQ subscriptions, releases the IOMMU domain, calls `DeviceRegistry::release` which posts `EVENT_DEVICE_REMOVED` to all subscribers.

### Phase 7: Virtio keyboard userspace driver (atomic cutover)

Delete the kernel virtio keyboard driver and replace it with a userspace driver in a single commit. There is no coexistence period: `panda-kernel/src/devices/virtio_keyboard.rs` is removed, `pci::init()` no longer initialises it, and the new userspace binary handles the device via the subscription model.

**Files to delete:**
- `panda-kernel/src/devices/virtio_keyboard.rs` — removed entirely
- Any references to it in `panda-kernel/src/devices.rs` / `pci.rs`

**Files to add:**
- `userspace/drivers/virtio-keyboard/Cargo.toml`
- `userspace/drivers/virtio-keyboard/src/main.rs` — declares `pci_device_table![{ vendor: 0x1AF4, device: 0x1052 }]`; on `EVENT_DEVICE_ADDED`: `device_claim(token)`, `device_map_mmio` for virtio config BAR, `dma_alloc` for event ring, `device_subscribe_irq`; virtio feature negotiation via `read_volatile`/`write_volatile` on mapped BAR pointer; event loop: on `EVENT_DEVICE_IRQ`, drain event ring, post key events to subscribed clients; calls `OP_SERVICE_REGISTER("keyboard")` after init; on `EVENT_DEVICE_REMOVED`: unregisters service, closes device handle (triggers kernel cleanup), returns to waiting

**Makefile:** add `virtio-keyboard` driver to the ext2 image build; place binary in `file:/drivers/`

**Validation:** run `keyboard_test` and `mailbox_keyboard_test` userspace tests. Pass means the full stack — subscription, claim, MMIO, IRQ delivery, and service registration — works end-to-end.

---

### Phases 6–8: landed status and deviations (commit `<fill in after commit>`)

**Phase 6 — ✅ landed.** All five `OP_DEVICE_*`/`OP_DMA_*` hardware-access
syscalls have real kernel handlers, plus one syscall this plan didn't
anticipate:

- `iommu::manager::create_device_domain(PciAddress) -> Result<DomainId, _>`
  (not the plan's assumed name/signature, which predates the landed IOMMU
  crate's actual API) creates a genuine per-device domain via the `Iommu`
  trait's `create_domain`/`assign_device` — this is real per-device
  isolation, not a passthrough shim. `destroy_device_domain`,
  `dma_map_for_device`/`dma_unmap_for_device` round out the API the plan
  called `dma_map`/`dma_unmap` (renamed to make the per-device vs.
  passthrough-global distinction explicit at the call site).
- **Deviation — MMIO caching flags:** `OP_DEVICE_MAP_MMIO` maps the BAR via
  the existing `map_external` helper without setting non-cacheable
  (PAT/PCD) bits, because no PAT/PCD support exists anywhere in
  `panda-kernel/src/memory/` yet. The "MMIO non-cacheable mapping" kernel
  test this plan's Testing section describes was not written for the same
  reason — there is no PAT plumbing yet to assert against. Tracked as a
  follow-up in `panda-kernel/src/resource/device.rs`.
- **Deviation — IRQ subscription is a fixed pool, not general dynamic
  MSI-X:** `OP_DEVICE_SUBSCRIBE_IRQ` draws from a fixed pool of 8
  pre-generated vector handlers (MSI-X entry 0 only per device), because
  the interrupt layer only supports statically-known per-vector handlers.
  This caps concurrent device IRQ subscriptions at 8 system-wide. See
  `DeviceResource::subscribe_irq`'s doc comment.
- **Addition — `OP_DEVICE_POLL_EVENT` (not in this plan's syscall table):**
  discovered missing while wiring Phase 7. `OP_DEVICE_SUBSCRIBE`'s mailbox
  attachment can only post an event *bit*
  (`EVENT_DEVICE_ADDED`/`EVENT_DEVICE_REMOVED`); it has no way to carry the
  matched `DeviceEvent` payload (bus type, identity, and — critically — the
  claim token `OP_DEVICE_CLAIM` needs). `OP_DEVICE_POLL_EVENT
  (subscription_handle, out: *mut DeviceEvent) -> bool` drains one event at
  a time off a per-subscription queue (`resource::SubscriptionResource`),
  the same `MailboxEventResult`-out-parameter convention `OP_MAILBOX_POLL`
  uses. Without this syscall, no driver can ever get a real token to claim
  with. See `panda-kernel/src/syscall/device.rs`'s module doc comment.
- **Addition — `MmioMapOut` out-parameter:** `OP_DEVICE_MAP_MMIO` was
  originally speced to return just a virtual address as the syscall's
  `isize`. It also needs to return the BAR's real size (so userspace can
  safely bounds-check `MmioRegion` accesses) — landed as a `DmaAllocOut`-style
  out-parameter (`{virt_addr, size}`) instead.

**Phase 7 — ✅ landed, `keyboard_test`/`mailbox_keyboard_test` pass.** The
atomic cutover is complete: `panda-kernel/src/devices/virtio_keyboard.rs`
and every reference to it are deleted (`grep -rn virtio_keyboard
panda-kernel/` is empty), and `userspace/drivers/virtio-keyboard/` hand-rolls
the full virtio-pci modern transport (capability walk, feature negotiation,
split-virtqueue setup/kick/drain, MSI-X) against the Phase 6 syscalls, with
no dependency on the kernel-only `vendor/virtio-drivers` crate. Getting from
"builds and runs the syscall sequence" to "actually receives key events"
took five separate, independently-diagnosed bugs — each confirmed with a
concrete repro (mostly QEMU's own `-trace virtqueue_pop`/`virtio_pci_notify_write`/
`msix_write_config` output, cross-checked against `vendor/virtio-drivers`'
known-good sequence) before being fixed:

1. **A kernel deadlock in `OP_DEVICE_SUBSCRIBE`.** `handle_device_subscribe`
   called `scheduler::current_process_id()` (takes `SCHEDULER.read()`) from
   inside `scheduler::with_current_process`'s closure (already holds
   `SCHEDULER.write()`) — a self-deadlock on the non-reentrant
   `RwSpinlock`, hit on *every* call with a non-zero mailbox. Never
   exercised end-to-end before (`panda-kernel/tests/device.rs`'s unit tests
   call `DeviceRegistry::subscribe` directly, bypassing the syscall
   handler). Fixed by using `proc.id()` instead — see
   `panda-kernel/src/syscall/device.rs::handle_device_subscribe`.
2. **Wrong device claimed.** `vendor_id`/`device_id` alone don't identify a
   virtio-input device: QEMU's virtio-mouse-pci and virtio-keyboard-pci
   share the identical PCI ID `0x1AF4:0x1052`, distinguished only by PCI
   class code (0x0900 = keyboard, 0x0902 = mouse). The driver's match
   entry had `class: 0, class_mask: 0` (matches both), so it claimed
   whichever device's `EVENT_DEVICE_ADDED` replayed first — the mouse.
   Every virtqueue/capability/notify computation was correct but wired to
   a device that structurally never receives keyboard input. Found by
   noticing the claimed device's PCI address (00:02.0, via a kernel debug
   log) didn't match `info pci`'s keyboard entry (00:03.0). Fixed by
   adding `class: 0x0900, class_mask: 0xFFFF` to the match entry — see
   `userspace/drivers/virtio-keyboard/src/main.rs`'s `main!` block.
3. **`EVENT_DEVICE_IRQ` silently dropped.** `handle_device_subscribe_irq`
   built a `MailboxRef` and handed it to `DeviceResource::subscribe_irq`
   without ever calling `mailbox.attach(device_handle, EVENT_DEVICE_IRQ)`
   first — `MailboxInner::post_event`'s `self.attached.get(&handle_id)`
   check silently drops any event for an unattached handle. Confirmed via
   a kernel debug log that the MSI-X interrupt handler (`dynamic_irq_common`)
   *did* fire on every real keypress; the driver process just never saw it.
   Fixed by attaching before constructing the `MailboxRef` — see
   `panda-kernel/src/syscall/device.rs::handle_device_subscribe_irq`.
4. **`OP_FILE_READ`'s `FILE_NONBLOCK` flag had no effect for scheme-proxy
   resources.** `handle_read_scheme_proxy` always awaited a full
   request/response round trip regardless of the caller's blocking
   intent, and the wire protocol (`scheme_protocol::Request::Read`) had no
   field to express "don't wait" to the provider. Any scheme provider that
   defers an empty read to give *blocking* reads real semantics (this
   driver's `pending_reads`, needed for `keyboard_test`'s `file::read`)
   made `file::try_read` block too, hanging `mailbox_keyboard_test`'s
   non-blocking drain loop forever once it caught up with the buffered
   events. Fixed by adding a `nonblocking: bool` field to `Request::Read`,
   threading `FILE_NONBLOCK` through `SchemeProxyResource::read`, and
   having the driver reply immediately with zero bytes instead of
   deferring when set.
5. **The persistent driver process had no way to let the test's QEMU
   instance shut down.** `keyboard_test`/`mailbox_keyboard_test` act as
   their own `init` (no service manager runs in the userspace-test
   harness) and spawn `virtio-keyboard` directly, but it — correctly, by
   design — never exits. The kernel only shuts QEMU down via
   isa-debug-exit once *no* processes remain
   (`scheduler::exec_next_runnable`), so the test hung past its own
   `PASSED` point. `OP_PROCESS_SIGNAL`/kill was considered and abandoned:
   forcibly removing a process holding an active `DeviceResource` from
   *outside* its own call stack caused a page fault in the caller (see
   `resource::spawn_handle::SpawnHandle::signal`'s doc comment for why).
   Fixed with a cooperative shutdown request instead: the test sends
   `b"shutdown"` on the spawn channel, and the driver's main loop checks
   for it each iteration and exits normally through the same
   `OP_PROCESS_EXIT` path every other process uses.

Fixing bug 4 (a persistent background reader task now owns every
`ProviderState`'s channel — see the M2.2-era doc comment on that struct)
surfaced two more pre-existing deadlock/hang classes, general to *any*
userspace scheme provider, not specific to virtio-keyboard, caught by
`scheme_provider_test`/`scheme_provider_concurrency_test`/the compositor
and window tests going from PASSED to TIMEOUT:

6. **Same deadlock class as bug 1, independently, in `OP_FILE_CLOSE`.**
   `handle_close` dropped the removed handle's resource (e.g. a
   `ChannelEndpoint`, whose `Drop` wakes the peer's blocked reader) while
   still holding `with_current_process`'s scheduler lock. Once a *kernel
   task* (the new background reader) could be a channel's blocked reader
   too, this became reachable and hung `scheme_provider_test`'s
   provider-process-exit case. Fixed generally, not just at this one call
   site: added `syscall::helpers::remove_handle`, which returns the
   removed handle to the caller *after* releasing the lock, so the
   resource drops outside it. `handle_close` now uses it; see that
   helper's doc comment for the full hazard writeup.
7. **A round trip started after a provider's channel already disconnected
   had nothing to ever fail it.** The background reader task's disconnect
   handling only wakes round trips already registered *at the moment it
   observes the disconnect*, then returns permanently — a later
   `round_trip_with_attachment` call would register a pending entry and
   wait forever for a reader that no longer exists. Fixed with a
   `disconnected: AtomicBool` on `ProviderState`, set right before the
   reader task's final return and checked by every round trip (both before
   registering and inside its poll, to close the race between the two).

`scheme_registry_test` and `state_test` needed small updates independent of
the above: both hardcoded `keyboard:` as an always-available built-in
scheme, true when it was kernel-resident and false now that it's an opt-in
userspace driver — the same adjustment `keyboard_test`/
`mailbox_keyboard_test` themselves needed (see Phase 7's file list above).
`scripts/list-tests.sh`'s `check-extras` validator was extended to accept
`_EXTRAS` crates living under `userspace/drivers/`, not just
`userspace/tests/`, so `keyboard_test_EXTRAS`/`mailbox_keyboard_test_EXTRAS
:= virtio-keyboard` validates. `block_test` (the plan's named
DMA-regression check) passes with no regression. Full `nix develop -c make
test` is clean, twice in a row.

### Roadmap M4 — display driver service: landed status and deviations

**⚠️ Landed with a known, verified-non-hang flakiness gap — see "Verification
status" below.** `panda-kernel/src/devices/virtio_gpu/mod.rs` and
`panda-kernel/src/resource/display.rs` are deleted (`grep -rn virtio_gpu
panda-kernel/src` finds only a stale doc-comment reference in
`devices/virtio_hal.rs`, no live code) in the same commit as
`userspace/drivers/virtio-gpu/`, which hand-rolls the virtio-pci modern
transport the same way `userspace/drivers/virtio-keyboard/` does (capability
walk, feature negotiation, split-virtqueue setup/kick/drain) against a
controlq only — no cursorq, since basic 2D scanout via `RESOURCE_CREATE_2D`/
`RESOURCE_ATTACH_BACKING`/`SET_SCANOUT`/`TRANSFER_TO_HOST_2D`/
`RESOURCE_FLUSH` never touches it. The `display:` scheme's client-facing
protocol (info/map/flush, exclusive claim) is unchanged, per the roadmap —
only the provider moved.

**Deviation — transport code duplicated, not factored into a shared
crate.** `userspace/drivers/virtio-gpu/src/main.rs` (~860 lines) largely
mirrors `virtio-keyboard/src/main.rs`'s PCI-capability-walk/feature-
negotiation/virtqueue plumbing rather than sharing it, and the concurrently-
landed virtio-blk driver duplicates it a third time. Deliberately not
extracted here (or interrupted mid-flight to extract) — tracked as a
separate follow-up task to run after all three drivers have landed.

**Two real bugs found and fixed while landing this, both applicable beyond
GPU:**

1. **`OP_DISPLAY_MAP` could map the framebuffer into the wrong process's
   page tables.** `syscall::display::handle_map` awaits a cross-process
   round trip to the userspace driver (`proxy.display_map().await`) before
   doing the actual `map_external` page-table work — unlike a plain syscall
   entry (always starts with the caller's CR3 already loaded), nothing
   guarantees the CPU's current CR3 is still the calling process's after an
   `.await` that crossed a process boundary; the scheduler only switches
   CR3 when dispatching back to *userspace*, not on every intermediate
   future-poll. In practice this mapped the framebuffer into whatever
   process happened to be current when the future was polled to
   completion — often the very driver process the display was requested
   from, since it was just scheduled to answer the request. The kernel's
   own post-map probe write (now removed debug code) succeeded regardless,
   because it ran under the same wrong CR3; the bug only surfaced as a
   `NOT PRESENT` page fault back in the actual calling process, the first
   time real userspace code tried to write to the "mapped" address.
   Fixed by adding `memory::with_page_table` (`panda-kernel/src/memory/paging.rs`),
   a safe wrapper that explicitly switches to the target process's page
   table before CR3-sensitive work and restores the previous one after —
   mirroring the manual save/switch/restore `Process::from_elf_data`
   already does for the same class of reason (loading a *different*
   process's ELF image into its own page table). Any future syscall that
   does page-table work after an inter-process `.await` needs the same
   treatment; there is no general guard against this class of bug yet.
2. **`OP_DISPLAY_FLUSH`'s partial-rect path sent a spec-inconsistent
   `TRANSFER_TO_HOST_2D` command.** An earlier version of
   `Gpu::flush` forwarded the client's damaged rectangle as both the
   command's destination `rect` *and* implicitly assumed a zero `offset`
   into the linear backing buffer — but virtio-gpu's `offset` field must
   independently equal the byte offset matching that rect's origin
   (`y * stride + x * bpp`); leaving it hardcoded to `0` while `rect` had a
   nonzero origin made the host sample the wrong source pixels, rendering
   composited windows at a shifted, garbled offset (caught by
   `window_test`'s screenshot comparison, not by any log-based assertion).
   Fixed by always transferring/flushing the whole surface regardless of
   `region` — matching both the deleted kernel driver's actual contract
   (`resource::display::DisplayResource::flush`'s doc comment already said
   "the underlying driver exposes no partial-rect flush... the rectangle
   currently serves as validation and future-proofing... rather than as a
   bandwidth optimisation") and `vendor/virtio-drivers/src/device/gpu/mod.rs`'s
   own `flush()`, which is likewise always whole-surface. Real partial-rect
   support (computing `offset` correctly) is possible future work but isn't
   needed to match the existing, tested contract.

**Test infrastructure changes.** `claim_test` and `display_test` both act as
their own `init` (no service manager runs in a per-test QEMU boot to spawn
drivers from `initrd:/drivers`), so both now spawn
`file:/initrd/virtio-gpu` directly and wait for its ready signal before
opening `display:/pci/display/0`, the same pattern `keyboard_test` already
established for `virtio-keyboard`; `claim_test_EXTRAS` gained `virtio-gpu`
in the Makefile accordingly. `scheme_registry_test` does *not* spawn the
driver — `display` left its "must be present in `scheme:/` at boot" pinned
list the same way `keyboard` already had for Phase 7, since it's now
registered dynamically by the driver rather than at kernel boot.
Screenshot-verified compositor tests (`window_test`, `multi_window_test`,
`alpha_test`, `partial_refresh_test`, `window_move_test`,
`compositor_protocol_test`) already spawned `virtio-gpu` via
`compositor_test_child` before this phase (that binary's own doc comment
predates this note); no change needed there beyond the driver itself
working.

**Verification status: build clean; individually correct; NOT reliably
green under this host's default (unlimited) `make test` parallelism.**
`nix develop -c make build` is clean. Every test passes individually and
the full suite passes cleanly when run with a generously extended
per-test timeout (`TEST_TIMEOUT=180`), confirming the driver itself is
functionally correct and nothing is genuinely hung. But at the harness's
*default* (unlimited) parallelism on the 8-core machine this was verified
on, `alpha_test`, `compositor_protocol_test`, `multi_window_test`,
`partial_refresh_test`, `window_move_test`, and sometimes `window_test`/
`compositor_start_test` intermittently exceed the default 60 s per-test
timeout — confirmed via full-log inspection to be real-time slowness under
CPU contention, not a hang (no missing completion, no fault, no stuck
future). `compositor_test_child`'s own doc comment already documents this
general class of flakiness (fixed real-time tick budget vs. host
parallelism) as a pre-existing, previously-tuned concern from Phase 7; this
phase makes it materially worse because every screenshot-verified
compositor test now also pays real per-flush latency from this driver's
non-IRQ busy-poll controlq wait (`ControlQueue::wait_for_response`) that
the old in-kernel driver never had (direct hardware access, no syscall
round trips at all). Mitigated but not eliminated here: `wait_for_response`
now spins on `core::hint::spin_loop()` locally and only calls
`process::yield_now()` every 256th iteration (was every iteration),
cutting scheduler-round-trip overhead in the common case where QEMU's
software virtio-gpu backend posts a completion almost immediately — this
measurably reduced (but did not eliminate) the timeout rate across repeated
runs. Follow-up options, none attempted here for time: raise the default
per-test timeout for the compositor test class, make the controlq wait
genuinely IRQ-driven instead of polling (removing the busy-wait's CPU
contention entirely), or document `MAX_PARALLEL=4` (or lower) as the
supported way to run `make test` reliably on constrained hardware.

**Phase 8 — ✅ landed.** `docs/DEVICE_DRIVERS.md` documents the real,
implemented API (including `OP_DEVICE_POLL_EVENT` and the `MmioMapOut`
out-parameter above, and Phase 7's virtqueue gap). `docs/DEVICE_PATHS.md`
and `CLAUDE.md` updated per the Phase 8 file list.

### Phase 8: Documentation

Write `docs/DEVICE_DRIVERS.md` after Phase 7 so it documents the real implemented API.

**File: `docs/DEVICE_DRIVERS.md`** — sections:

1. **Overview** — driver-centric subscription model; hot-plug as default; boot enumeration as a special case of the same mechanism
2. **ELF device table format** — section naming convention; struct layout requirement; `panda_elf::read_section` API; why fixed-size structs
3. **Declaring device tables** — `pci_device_table![]` and siblings; wildcard values; multi-bus drivers; compiled example
4. **Driver lifecycle** — step-by-step from process start through service registration and device removal; the subscription replay guarantee; cleanup on `EVENT_DEVICE_REMOVED`; crash safety via handle close
5. **Syscall reference** — `OP_DEVICE_SUBSCRIBE`, `OP_DEVICE_CLAIM`, `OP_DEVICE_MAP_MMIO`, `OP_DMA_ALLOC`, `OP_DMA_FREE`, `OP_DEVICE_SUBSCRIBE_IRQ`; args, return values, error conditions; note that closing the device handle is sufficient for full cleanup
6. **Mailbox events** — `EVENT_DEVICE_ADDED`, `EVENT_DEVICE_REMOVED`, `EVENT_DEVICE_IRQ`; payload format; token lifetime and single-use semantics
7. **Security model** — device tokens as non-forgeable handles; what the future capability gates will look like; why raw device addresses are not accepted by `DEVICE_CLAIM`
8. **Bus type reference** — table of bus types, section names, match structs, identity fields
9. **Writing a driver** — annotated example of a minimal PCI driver from `main()` through serving clients
10. **Service manager integration** — how `file:/drivers/` is scanned; when drivers are spawned; relationship to TOML service configs

**Update `docs/DEVICE_PATHS.md`** — note that `keyboard:/pci/input/0` etc. are superseded by `service:/keyboard` as drivers move to userspace; the PCI path scheme will be retired once all kernel drivers are ported.

**Update `CLAUDE.md`** — add `docs/DEVICE_DRIVERS.md` to the documentation table of contents.

## Testing

- **ELF section reading** — host unit tests in `crates/panda-elf`: correct section present returns correct bytes; absent section returns `None`; truncated ELF returns `None`
- **Match struct size stability** — compile-time assertions in `panda-abi` for each struct size; catches accidental layout changes that would silently misparse ELF sections
- **Subscription replay** — kernel test: register a PCI device, subscribe after the fact, assert `EVENT_DEVICE_ADDED` fires immediately with a valid token
- **Token single-use** — kernel test: claim with a token succeeds; a second claim attempt with the same token returns an error; a claim attempt by a process that never held the token returns an error
- **Wildcard matching** — kernel test: `vendor_id = 0xFFFF` matches any vendor; `class_mask = 0` ignores class; both combined matches all PCI devices
- **Exclusive claim** — kernel test: two processes both subscribe to the same device type; both receive `EVENT_DEVICE_ADDED` each with their own token; the first to call `DEVICE_CLAIM` succeeds; the second receives `AlreadyClaimed` — its token is valid but the claim registry already has an owner. Both the token check (is this handle in your table?) and the claim check (does the registry show an owner?) must pass independently.
- **MMIO non-cacheable mapping** — kernel test: after `DEVICE_MAP_MMIO`, walk the calling process's page table for the returned virtual address; assert the PAT index selects UC (uncacheable) memory type. This validates the PCD/PWT/PAT bits are set correctly in the page table entry — a silent failure here would cause reads to return stale cached values.
- **MmioRegion bounds** — unit test in `libpanda`: read/write within bounds succeeds; read/write at exactly `size - sizeof(T)` succeeds; read/write one byte past the end panics.
- **MMIO round-trip** — userspace test: map BAR 0 of a known virtio device via `MmioRegion`; read first 4 bytes; assert they equal the virtio magic number `0x74726976`
- **DMA round-trip** — userspace test: `dma_alloc`, write pattern via virt addr, read back via virt addr; assert iova is non-zero (IOMMU-mapped)
- **`EVENT_DEVICE_REMOVED` cleanup — driver side** — kernel/userspace test: after a device is removed (simulated by kernel test infrastructure), assert the driver process: (a) calls `OP_SERVICE_UNREGISTER` or the service becomes unreachable, (b) closes the device handle, (c) is still alive and returns to waiting. Re-add the device and assert the driver claims it again.
- **`EVENT_DEVICE_REMOVED` cleanup — kernel side** — after the driver closes its device handle following removal: assert MMIO mapping is gone from the process's page tables; assert IOMMU domain is released (no mappings remain); assert IRQ no longer posts `EVENT_DEVICE_IRQ` to the driver's mailbox; assert a new subscription and claim for the same device succeeds (resources fully freed)
- **Process exit cleanup** — kernel test: a process claims a device and then exits without explicitly closing the handle; assert the kernel releases the claim and posts `EVENT_DEVICE_REMOVED` to subscribers; assert all MMIO/DMA/IRQ resources freed
- **IRQ delivery** — `keyboard_test` and `mailbox_keyboard_test` run against the userspace virtio keyboard driver after atomic cutover in Phase 7

## Risks

1. **Wildcard matching must be implemented per bus type.** The service manager subscribes once per known bus type with all-wildcard match data (`OP_DEVICE_SUBSCRIBE(Pci, all-0xFF)`, `OP_DEVICE_SUBSCRIBE(Usb, all-0xFF)`, etc.). The kernel's subscription matching must treat `0xFFFF` vendor/device IDs as wildcards — `vendor_id == 0xFFFF || vendor_id == device.vendor_id` — and this logic must be correct for every bus type's match struct before Phase 5 lands. Adding a new bus type to panda requires one new `OP_DEVICE_SUBSCRIBE` call in the service manager's startup sequence; the absence of a call for a new bus type means its devices will never spawn a driver, which will be obvious during testing.

2. **`initrd:` scheme needed before root fs is available.** Phase 5 uses an `initrd:` scheme backed by the in-memory tar contents — the kernel keeps the initrd tar in memory after boot and exposes it as a read-only scheme without extracting to disk. This avoids needing any filesystem to be mounted before reading initrd drivers. The kernel's scheme handler parses the ustar format on demand (sequential reads only; no random access needed for driver binary loading). This scheme must be implemented as part of Phase 5; it is a prerequisite for the two-phase driver scanning to work.
