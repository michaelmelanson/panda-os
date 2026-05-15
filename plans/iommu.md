# IOMMU support

## Problem

All virtio device drivers run in kernel space and perform DMA by handing physical addresses directly to devices. There is no IOMMU — a misbehaving or compromised driver can instruct a device to DMA to any physical address, including kernel data structures and other processes' memory.

This is a blocker for userspace device drivers. A userspace driver process must not be able to use a device to read or write memory it does not own. IOMMU hardware enforces this: each device is assigned a **domain** with its own set of page table mappings, and the hardware rejects any DMA access outside those mappings.

## Goals

1. Implement the IOMMU abstraction as a standalone crate (`crates/iommu/`) that has no dependency on panda-kernel internals and could be published separately.
2. Implement a hardware-agnostic `Iommu` trait so that Intel VT-d and AMD-Vi (and future hardware) share the same kernel integration points.
3. Implement VT-d as the first backend (QEMU supports VT-d emulation on q35 machines, which panda already uses).
4. Stub AMD-Vi so the module structure is established from the start.
5. Integrate IOMMU domain assignment into the DMA allocation path so that all virtio devices are covered transparently.
6. Keep existing behaviour unchanged during the initial integration (identity-mapped passthrough domain for all devices) — no driver changes required.

## Constraints

- **q35 machine type** is already used in both `QEMU_COMMON` (Makefile) and the test QEMU command (`scripts/run-qemu-test.sh`). VT-d emulation requires `-device intel-iommu` added to both.
- **ACPI parsing already exists** (`panda-kernel/src/acpi/`). The `acpi` crate finds tables by signature; DMAR/IVRS sub-table structures need hand-written parsing. ACPI access stays in the kernel; the crate accepts raw bytes.
- **No IOMMU crate** exists in the Rust ecosystem for kernel use. Both VT-d and AMD-Vi page table formats must be implemented from scratch.
- **`no_std` + `alloc` throughout.** The crate must compile without `std`; it may use `alloc` for `BTreeMap` and `Vec`. The kernel already satisfies both.
- **Publishability constraint.** The crate must have no dependency on kernel-private types (`DmaBuffer`, `PhysicalMapping`, kernel spinlocks, etc.). Kernel-specific operations are injected via HAL traits.

## Design

### Crate boundary

The `crates/iommu/` crate contains everything that is hardware-specification knowledge and hardware-agnostic logic:

- The `Iommu` trait and associated types (`DomainId`, `IommuFlags`, `IommuError`, `PciAddress`)
- The `IommuDomain` RAII wrapper
- The `IovaAllocator` (I/O virtual address free-list)
- HAL traits that the kernel implements (`FrameAllocator`, `MmioAccess`)
- The VT-d backend, generic over those HAL traits
- The AMD-Vi stub

The kernel (`panda-kernel/src/iommu/`) contains everything that requires kernel-private infrastructure:

- `KernelFrameAllocator` — implements `FrameAllocator` using `DmaBuffer`
- `KernelMmioAccess` — implements `MmioAccess` using `PhysicalMapping`
- DMAR table *access* via the kernel's ACPI handler (maps the table, extracts bytes, passes them to the crate's parser)
- The global `IOMMU` static, `init()`, `get()`, and the `manager` helpers for the DMA path

### HAL traits

These two traits are defined in the crate and implemented by the kernel. They are the only seam between the crate and kernel-private memory management.

```rust
/// Provides 4 KB physically-contiguous frames for IOMMU page table nodes.
///
/// # Safety
/// Implementors must guarantee that the returned frame is:
/// - 4 KB aligned in both physical and virtual address
/// - exclusively owned by the caller until `dealloc_frame` is called
/// - valid for reads and writes of exactly 4096 bytes through the virtual pointer
pub unsafe trait FrameAllocator: Send + Sync {
    /// Allocate one page frame.
    /// Returns `(physical_address, virtual_pointer)` or `None` on OOM.
    fn alloc_frame(&self) -> Option<(u64, *mut u8)>;

    /// Release a frame previously returned by `alloc_frame`.
    /// # Safety
    /// `phys` must be a value previously returned by this allocator
    /// and not yet freed.
    unsafe fn dealloc_frame(&self, phys: u64);
}

/// Provides volatile read/write access to a contiguous MMIO register region.
///
/// # Safety
/// Implementors must guarantee that reads and writes go directly to device
/// memory without caching, and that the region covers at least the offsets
/// the caller will access.
pub unsafe trait MmioAccess: Send + Sync {
    unsafe fn read_u32(&self, offset: usize) -> u32;
    unsafe fn write_u32(&self, offset: usize, val: u32);
    unsafe fn read_u64(&self, offset: usize) -> u64;
    unsafe fn write_u64(&self, offset: usize, val: u64);
}
```

### Iommu trait

```rust
pub type DomainId = u32;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct IommuFlags(u8);

impl IommuFlags {
    pub const READ: Self = Self(0x01);
    pub const WRITE: Self = Self(0x02);
    pub const READ_WRITE: Self = Self(0x03);
}

impl core::ops::BitOr for IommuFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
}

/// A PCI device address (segment, bus, device, function).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PciAddress {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

pub trait Iommu: Send + Sync {
    /// Allocate a new isolated translation domain.
    fn create_domain(&self) -> Result<DomainId, IommuError>;

    /// Release a domain and all its page table mappings.
    fn destroy_domain(&self, domain: DomainId);

    /// Map a physical address range into a domain at the given I/O virtual address.
    fn map(&self, domain: DomainId, iova: u64, phys: u64,
           size: usize, flags: IommuFlags) -> Result<(), IommuError>;

    /// Remove a mapping from a domain.
    fn unmap(&self, domain: DomainId, iova: u64, size: usize);

    /// Assign a PCI device to a domain. Device DMA is now constrained to
    /// mappings within that domain.
    fn assign_device(&self, domain: DomainId, device: PciAddress) -> Result<(), IommuError>;

    /// Flush the IOTLB for a domain after mapping changes.
    fn flush(&self, domain: DomainId);
}
```

Everything below the trait is hardware-specific. The kernel stores the active implementation as `&'static dyn Iommu` initialised once during boot. Code that allocates DMA buffers or assigns devices never needs to know which hardware is present.

### What lives in the crate but above the trait

- **`IovaAllocator`** — a free-list over the I/O virtual address space, one instance per domain. Both VT-d and AMD-Vi deal in IOVAs; this allocator has no hardware dependency. Implemented with a sorted `Vec<(u64, usize)>` of free ranges and coalescing on free.
- **`IommuDomain`** — RAII wrapper holding a `DomainId` and a `&dyn Iommu` reference; calls `destroy_domain` on drop.

### Page table ownership

Each backend owns its page table allocations internally, opaque to the trait. The alternative — a generic associated type for page tables — would leak hardware details into the trait and make `dyn Iommu` impossible.

### Intel VT-d backend

The VT-d backend is generic over `F: FrameAllocator` and `M: MmioAccess`, which the kernel provides as concrete types. `VtdIommu<F, M>` implements `Iommu` and is stored as `Box<dyn Iommu>` (erasing the type parameters at the kernel boundary).

**Discovery:** The DMAR ACPI table contains DRHD (DMA Remapping Hardware Definition) sub-tables — one per IOMMU unit — each giving a register base address and device scope. RMRR (Reserved Memory Region Reporting) sub-tables describe physical memory regions that firmware requires devices to access; these must be identity-mapped in every domain to avoid firmware breakage. The crate's `dmar::parse(&[u8])` takes raw table bytes and returns a structured `DmarTable`. The kernel maps the ACPI table and passes the bytes.

**Hardware registers (`VtdRegisters<M: MmioAccess>`):** Wraps an `M` instance. Key registers: Root Table Address, Global Command/Status, IOTLB Invalidation, Context Command. Phase 1 uses register-based invalidation (simpler; queued invalidation required only for interrupt remapping).

**Context entry table (`RootTable<F: FrameAllocator>`):** A two-level table (root → context) indexed by `(bus, device, function)`. Each context entry points to a domain's SLPT root and records the domain ID. Frames are allocated via `F`.

**Second-level page tables (`SlptRoot<F: FrameAllocator>`):** Four-level structure (same geometry as x86-64 paging — 512 entries, 9 bits per level — but different flag bits: bit 0 = read, bit 1 = write, no NX). Intermediate nodes allocated via `F` on demand; dropped when empty on unmap.

**Passthrough domain:** A special domain that identity-maps a configurable physical range (IOVA == phys). All virtio devices are assigned here initially. This preserves existing behaviour exactly — no driver changes, no DMA regressions.

### AMD-Vi backend

A stub module that implements `Iommu` by returning `IommuError::Unsupported` for all operations. The `ivrs.rs` parser and page table format are left unimplemented. This establishes the module structure so AMD-Vi can be filled in without changing the trait or shared code.

AMD-Vi uses an IVRS ACPI table (analogous to DMAR) and a flat device table indexed by device ID (analogous to the root/context table hierarchy). The page table format is four-level with different flag semantics.

### Module layout

```
crates/iommu/
  Cargo.toml          -- name = "iommu", no_std, features = ["alloc"]
  src/
    lib.rs            -- re-exports, #![no_std], extern crate alloc
    types.rs          -- DomainId, PciAddress, IommuFlags, IommuError
    iommu.rs          -- Iommu trait
    domain.rs         -- IommuDomain RAII wrapper
    iova.rs           -- IovaAllocator (free-list, coalescing)
    hal.rs            -- FrameAllocator trait, MmioAccess trait
    vtd/
      mod.rs          -- VtdIommu<F,M> implementing Iommu; VtdUnit<F,M>
      dmar.rs         -- parse(&[u8]) -> DmarTable; DrhEntry, RmrrEntry
      context.rs      -- RootTable<F>: root/context entry tables
      page_table.rs   -- SlptRoot<F>: SLPT allocation and mapping
      registers.rs    -- VtdRegisters<M>: register layout and operations
    amd_vi/
      mod.rs          -- AmdViIommu stub implementing Iommu
      ivrs.rs         -- parse stub (returns NotImplemented)
      page_table.rs   -- empty module, notes on AMD-Vi differences

panda-kernel/src/iommu/
  mod.rs              -- init(), get(), IOMMU global (RwSpinlock<Option<&'static dyn Iommu>>)
  hal.rs              -- KernelFrameAllocator (DmaBuffer), KernelMmioAccess (PhysicalMapping)
  dmar_acpi.rs        -- map DMAR via acpi::with_table, extract bytes, call iommu::vtd::dmar::parse
  manager.rs          -- DeviceDomainRegistry, dma_map(), dma_unmap() helpers for virtio_hal
```

## Implementation plan

### Phase 1: QEMU and build configuration

Enable VT-d emulation in QEMU. No kernel changes yet — just confirming QEMU presents a DMAR table for later parsing.

**Files:**
- `Makefile` — add `-device intel-iommu` to `QEMU_COMMON`
- `scripts/run-qemu-test.sh` — add `-device intel-iommu` to the `QEMU_CMD` array

**Verification:** Boot the existing kernel and confirm it still runs. The kernel ignores the IOMMU hardware for now.

### Phase 2: `crates/iommu` scaffolding

Create the crate and define all types, traits, and shared logic that have no hardware dependency.

**Files:**
- `crates/iommu/Cargo.toml` — `name = "iommu"`, `edition = "2024"`, `[features] default = ["alloc"]`, `alloc = []`; no external dependencies
- `Cargo.toml` (workspace root) — add `"crates/iommu"` to `[workspace.members]`
- `panda-kernel/Cargo.toml` — add `iommu = { path = "../../crates/iommu" }`
- `crates/iommu/src/lib.rs` — `#![no_std]`, `#[cfg(feature = "alloc")] extern crate alloc;`, module declarations, top-level re-exports
- `crates/iommu/src/types.rs` — `DomainId`, `PciAddress`, `IommuFlags` (with `BitOr`), `IommuError`
- `crates/iommu/src/iommu.rs` — `Iommu` trait
- `crates/iommu/src/domain.rs` — `IommuDomain`: holds `DomainId` + `*const dyn Iommu`, calls `destroy_domain` on drop; `Send`/`Sync` bounds ensure safety
- `crates/iommu/src/iova.rs` — `IovaAllocator`: sorted `Vec<(u64, usize)>` of free ranges; `alloc(size, align) -> Option<u64>`, `free(base, size)` with coalescing; constructor takes `(range_start, range_end)`
- `crates/iommu/src/hal.rs` — `FrameAllocator` and `MmioAccess` unsafe traits with doc comments describing the contracts the kernel must uphold

### Phase 3: DMAR table parser (in crate)

Parse the raw bytes of the DMAR ACPI table. The crate works with `&[u8]` — no knowledge of how the kernel obtained the bytes.

**DMAR structure (Intel VT-d specification, chapter 8):**
- Header: signature `"DMAR"`, length, revision, OEM fields, host address width, flags
- Sub-tables immediately follow: each has 2-byte type and 2-byte length
  - Type 0 = DRHD: register base address (u64), segment (u16), device scope list
  - Type 1 = RMRR: base address (u64), limit address (u64), segment, device scope list
  - Device scope entry: type, start bus, path of (device, function) pairs

**Files:**
- `crates/iommu/src/vtd/dmar.rs` — `DmarTable` (owned, parsed); `DrhEntry` (reg base, segment, `Vec<DeviceScope>`); `RmrrEntry` (phys range, `Vec<DeviceScope>`); `DeviceScope`; `parse(bytes: &[u8]) -> Result<DmarTable, DmarError>`; iterators over DRHD and RMRR entries; pure byte-slice parsing, no kernel calls

### Phase 4: VT-d register interface (in crate)

Define the register layout and hardware operations for one VT-d unit. Generic over `M: MmioAccess`.

**Key registers (offsets from DRHD base):**
- `0x000` — Version
- `0x008` — Capability (CAP): page table levels, IOTLB register offset (`IRO` field)
- `0x010` — Extended Capability (ECAP): queued invalidation support
- `0x018` — Global Command (GCMD): enable translation, set root table pointer
- `0x01C` — Global Status (GSTS): poll to confirm commands complete
- `0x020` — Root Table Address (RTADDR): written before enabling
- `0x028` — Context Command (CCMD): context-cache invalidation
- `CAP.IRO * 16` — IOTLB Invalidation registers (offset from base, not fixed)

**Files:**
- `crates/iommu/src/vtd/registers.rs` — `VtdRegisters<M: MmioAccess>`: holds `M`; typed methods for each register; `read_cap()`, `read_ecap()`, `enable_translation()`, `set_root_table(phys)`, `invalidate_context_cache()`, `invalidate_iotlb_global()`; GSTS polling loops; IOTLB offset computed from CAP.IRO on construction

### Phase 5: Context entry table (in crate)

The root table is a 4 KB page of 256 root entries (one per PCI bus). Each root entry points to a 4 KB context table of 256 entries (one per device/function, indexed as `device << 3 | function`). Each context entry records the domain ID and the physical address of that domain's SLPT root. Generic over `F: FrameAllocator`.

**Files:**
- `crates/iommu/src/vtd/context.rs` — `RootTable<F>`: allocates root and context table frames via `F`; `set_context_entry(bus, device, function, domain_id, slpt_root_phys)`; `physical_address() -> u64` for the `RTADDR` register; context entry bitfield: present (bit 0), translation type (bits 1-2 = `0b00` for translated), address width (bits 4-6 = `0b010` for 4-level), domain ID (bits 8-23), SLPT root pointer (bits 12-63 of the second 8-byte word)

### Phase 6: Second-level page tables (in crate)

VT-d uses a four-level page table with the same geometry as x86-64 (512 entries, 9-bit index per level, 12-bit page offset) but different flag bits. Generic over `F: FrameAllocator`.

**VT-d SLPT entry flags:** bit 0 = read permission, bit 1 = write permission. Bits 12–51 hold the physical address of the next-level table or page frame. No NX bit.

**Files:**
- `crates/iommu/src/vtd/page_table.rs` — `SlptNode`: holds the physical address of an allocated 4 KB frame and a virtual `*mut [u64; 512]` for CPU access; `SlptRoot<F>`: level-4 table, owns intermediate nodes in `BTreeMap<u64, SlptNode>` keyed by IOVA; `map(iova, phys, size, flags)`: walks four levels, allocates intermediate nodes via `F` on demand, writes leaf entries; `unmap(iova, size)`: clears entries, drops empty intermediate nodes, frees frames via `F`; `root_phys() -> u64` for context entries

### Phase 7: VtdIommu assembly and kernel HAL implementations

Assemble the crate's components into `VtdIommu<F, M>` implementing `Iommu`. Then, in the kernel, provide the concrete HAL types and wire everything into the boot sequence.

**Crate files:**
- `crates/iommu/src/vtd/mod.rs` — `VtdUnit<F, M>`: holds `VtdRegisters<M>` + `RootTable<F>` + the DRHD's device scopes for `assign_device` routing; `VtdIommu<F, M>`: holds `Vec<VtdUnit<F, M>>`, `Mutex<BTreeMap<DomainId, (SlptRoot<F>, IovaAllocator)>>`, `Mutex<IdAllocator>`; implements `Iommu`:
  - `create_domain()`: allocate domain ID, create empty `SlptRoot` and fresh `IovaAllocator`
  - `destroy_domain()`: drop `SlptRoot` (frames freed via `F` on drop), release ID
  - `map()`: look up domain, call `SlptRoot::map()`, call `flush()`
  - `unmap()`: `SlptRoot::unmap()`, call `flush()`
  - `assign_device()`: find the `VtdUnit` whose device scopes cover the PCI address, write context entry with the domain's SLPT root, invalidate context cache
  - `flush()`: `invalidate_iotlb_global()` on all units (domain-scoped invalidation is a future optimisation)
  - `init_passthrough_domain()`: create domain 1, map `0..passthrough_limit` as identity (IOVA == phys, `READ_WRITE`), assign all devices to it; the caller passes `passthrough_limit` (kernel provides this from its memory map)

**Kernel files:**
- `panda-kernel/src/iommu/hal.rs` — `KernelFrameAllocator`: implements `FrameAllocator` by calling `allocate_physical(Layout::from_size_align(4096, 4096))` and storing the `Frame` in a `BTreeMap<u64, Frame>` keyed by physical address; `dealloc_frame` drops the `Frame` from the map. `KernelMmioAccess`: wraps a `PhysicalMapping`; implements `MmioAccess` with volatile pointer reads/writes at the given offsets.
- `panda-kernel/src/iommu/dmar_acpi.rs` — `read_dmar() -> Result<DmarTable, ...>`: calls `acpi::with_table::<RawAcpiTable>("DMAR", |table| { let bytes = ...; iommu::vtd::dmar::parse(bytes) })`; converts the DMAR's DRHD entries into `(physical_base, KernelMmioAccess)` pairs by calling `PhysicalMapping::new` for each register base
- `panda-kernel/src/iommu/mod.rs` — `static IOMMU: RwSpinlock<Option<Box<dyn Iommu>>>`; `init()`: calls `dmar_acpi::read_dmar()`, constructs `KernelFrameAllocator`, builds one `VtdUnit` per DRHD entry, assembles `VtdIommu`, calls `init_passthrough_domain(passthrough_limit)`, stores as `Box<dyn Iommu>`; falls back to logging "AMD-Vi detected but not yet supported" if only IVRS found; `get() -> Option<&'static dyn Iommu>`; `init()` called from `panda-kernel/src/main.rs` after ACPI and PCI init

### Phase 8: DMA integration

Hook IOMMU mappings into the existing DMA allocation path.

**Files:**
- `panda-kernel/src/iommu/manager.rs` — `DeviceDomainRegistry`: `BTreeMap<PciAddress, DomainId>`, `assign(device, domain)`, `domain_for(device) -> Option<DomainId>`; `dma_map(device, phys, size, flags) -> u64`: look up domain, call `Iommu::map(domain, phys, phys, size, flags)` (IOVA == phys in passthrough), return IOVA; `dma_unmap(device, iova, size)`: call `Iommu::unmap()`
- `panda-kernel/src/devices/virtio_hal.rs` — in `dma_alloc()`: after allocating frames, call `iommu::manager::dma_map(device, phys, size, READ_WRITE)`, store IOVA alongside physical address, return IOVA to the virtio driver; in `dma_dealloc()`: call `iommu::manager::dma_unmap(device, iova, size)` before dropping frames

  During passthrough (IOVA == phys), the device receives the same address as before. The structural change — returning an IOVA instead of a raw physical address — is transparent because virtio drivers already treat the DMA address opaquely.

### Phase 9: AMD-Vi stub (in crate)

Establish the AMD-Vi module structure so future implementation requires no structural changes.

**Files:**
- `crates/iommu/src/amd_vi/mod.rs` — `AmdViIommu` (empty struct); implements `Iommu` with all methods returning `Err(IommuError::Unsupported)` or no-opping
- `crates/iommu/src/amd_vi/ivrs.rs` — `parse(_: &[u8]) -> Result<IvrsTable, IvrsError>` returning `Err(IvrsError::NotImplemented)`
- `crates/iommu/src/amd_vi/page_table.rs` — empty module; comment noting key differences from VT-d: flat device table (not root/context hierarchy), different flag bits, IOMMU command queue for invalidation
- `panda-kernel/src/iommu/mod.rs` — in `init()`, after failing to find DMAR, probe for IVRS and log "AMD-Vi detected but not yet supported" rather than panicking

## Testing

- **QEMU boots with `-device intel-iommu`** — all existing kernel and userspace tests continue to pass after Phase 1.
- **`crates/iommu` unit tests** — the crate can be tested on the host (no kernel required):
  - `IovaAllocator`: alloc/free round-trips, coalescing of adjacent free regions, alignment constraints
  - `dmar::parse`: a hand-crafted byte slice matching the DMAR structure produces the expected `DrhEntry` and `RmrrEntry` values
  - `SlptRoot`: map a range, inspect entries via a test `FrameAllocator` that returns known physical addresses, assert leaf entries have correct flags; unmap and assert entries cleared
- **DMAR table present** — kernel test that boots, reads the DMAR table via `dmar_acpi::read_dmar()`, and asserts at least one DRHD entry with a non-zero register base
- **Passthrough domain regression gate** — after Phase 7, all existing kernel tests and userspace tests continue to pass; IOMMU is active but behaviour is unchanged
- **Context entry round-trip** — write a context entry for a known PCI address, read the raw entry back from the root table frame, assert domain ID and SLPT root pointer fields match
- **IOTLB flush** — after mapping changes, poll GSTS to confirm VT-d invalidation completed
- **DMA integration** — run `block_test` and `ext2_validation` kernel tests after Phase 8; block I/O exercises the full DMA path through the IOMMU

## Risks

1. **RMRR identity mappings.** BIOS/UEFI firmware reserves memory regions that devices must be able to DMA to (RMRR entries). If these are not identity-mapped in every domain, firmware-initiated DMA (e.g., the UEFI GOP framebuffer driver) will be rejected by the IOMMU and the system will hang or fault. `VtdIommu::create_domain()` must automatically include all RMRR ranges as identity mappings in every new domain.

2. **Queued invalidation requirement.** Some VT-d implementations (including newer QEMU versions) require the invalidation queue to be initialised before enabling translation, even when using register-based invalidation. If `enable_translation()` hangs waiting on GSTS, check ECAP.QI and set up the invalidation queue if required.

3. **VT-d capability register offsets.** The IOTLB invalidation registers are not at a fixed offset — their location is encoded in the CAP register's IRO field (`(CAP >> 8) & 0x3ff`, multiplied by 16). The `VtdRegisters` constructor must read CAP first and compute the IOTLB offset before any invalidation operation.

4. **Physical memory coverage for passthrough.** The passthrough identity mapping must cover all physical addresses that `DmaBuffer` can return. The `DmaBuffer` allocator draws from the kernel heap, which is backed by the largest UEFI CONVENTIONAL memory region. Rather than enumerating all UEFI memory regions, `init_passthrough_domain()` can conservatively map 0–4 GB physical — over-mapping in the passthrough domain is harmless (devices could already DMA anywhere without an IOMMU) and avoids the need to store the UEFI memory map past boot.

5. **Workspace configuration.** Adding `crates/iommu` to the workspace `[members]` list and as a kernel dependency must be done consistently. If the crate is added to `Cargo.toml` but the kernel's `Cargo.toml` does not reference it, the kernel will silently use no IOMMU. The `iommu::init()` call in `main.rs` and the kernel's dependency declaration must be added together.
