//! Shared virtio-pci **modern** (virtio 1.0+) transport for userspace drivers.
//!
//! Every userspace virtio driver in this tree (`userspace/drivers/virtio-blk`,
//! `.../virtio-gpu`, `.../virtio-keyboard`) needs the same plumbing before it
//! can say anything device-specific: find the device's virtio-pci capability
//! structures in PCI config space, map the BARs they point at, negotiate
//! features, lay out a split virtqueue in DMA memory, and ring the right
//! doorbell. That plumbing lives here, once.
//!
//! # Why hand-rolled, and not `virtio_drivers`
//!
//! The deleted kernel-resident drivers used the `virtio_drivers` crate's
//! `PciTransport`, which is kernel-only: it assumes direct physical-memory
//! access and a `Hal` trait implemented against kernel internals (see
//! `panda-kernel/src/devices/virtio_hal.rs`). A userspace driver has none of
//! that — it only has the `OP_DEVICE_*`/`OP_DMA_*` syscalls exposed by
//! `libpanda::device`. Two consequences drove what is implemented here:
//!
//! 1. **PCI capability walk**: a modern virtio-pci device exposes its
//!    common-config/notify/ISR/device-config register blocks through PCI
//!    capabilities (vendor-specific capability id `0x09`), each naming a BAR
//!    plus a byte offset — it is *not* safe to assume BAR 0 offset 0 is the
//!    common config, unlike a naive legacy-virtio-pci reading.
//!    `OP_DEVICE_MAP_MMIO` only maps a BAR by index; it knows nothing about
//!    virtio. So [`find_virtio_caps`] reads raw PCI config space via
//!    `OP_DEVICE_PCI_CONFIG_READ` and walks the capability list by hand,
//!    matching what `PciTransport::new` does internally in the kernel
//!    drivers' old dependency.
//! 2. **Split virtqueue**: with no in-kernel virtio transport to build the
//!    descriptor table/avail ring/used ring, [`VirtioTransport::setup_queue`]
//!    lays them out by hand in a DMA buffer obtained from `dma_alloc`, and
//!    [`VirtQueue`] provides the driver-side ring bookkeeping.
//!
//! # Division of labour
//!
//! This crate owns the *generic* mechanics: capability walk, BAR mapping,
//! feature negotiation, descriptor-table/avail-ring/used-ring layout and
//! bookkeeping, notify-address computation, and MSI-X subscription. It
//! deliberately does **not** own request *payloads* — what goes in each
//! descriptor's buffer differs completely between the three drivers
//! (virtio-input: one fixed-size event buffer per descriptor that the device
//! fills; virtio-blk: header/data/status three-descriptor chains;
//! virtio-gpu: its own control-command request/response format). Each queue
//! gets a driver-owned "scratch" area inside the same DMA allocation
//! (see [`QueueConfig`]) whose contents are entirely the driver's business.
//!
//! # Landmines
//!
//! Three details of this transport each caused a real, hard-to-diagnose bug
//! before they were understood. They are implemented here so no future driver
//! has to rediscover them; see the doc comments at each site:
//!
//! 1. Notify-address computation — [`VirtQueue::notify_addr`] and
//!    [`find_virtio_caps`]'s handling of `notify_off_multiplier`.
//! 2. Kick ordering relative to `DRIVER_OK` —
//!    [`VirtioTransport::set_driver_ok`].
//! 3. Queue-size clamp *and writeback* — [`VirtioTransport::setup_queue`].

#![no_std]

extern crate alloc;

use libpanda::Handle;
use libpanda::device::{self, MmioRegion};
use libpanda::error::Result;
use libpanda::mailbox::Mailbox;
use panda_abi::ErrorCode;

// =============================================================================
// PCI config space: status register, capabilities list, virtio-pci caps
// (virtio-v1.1 spec section 4.1.4).
// =============================================================================

/// PCI config space offset of the 16-bit status register. Bit 4
/// (`PCI_STATUS_CAP_LIST`) indicates a capabilities list is present.
pub const PCI_STATUS_OFFSET: u32 = 0x06;
pub const PCI_STATUS_CAP_LIST: u32 = 1 << 4;
/// PCI config space offset of the 8-bit "capabilities pointer" (offset of the
/// first capability, only valid if `PCI_STATUS_CAP_LIST` is set).
pub const PCI_CAPABILITIES_POINTER_OFFSET: u32 = 0x34;
/// Standard PCI capability ID for a vendor-specific capability — every
/// virtio-pci capability structure uses this ID (virtio-v1.1 4.1.4).
pub const PCI_CAP_ID_VNDR: u8 = 0x09;

/// `cfg_type` values from `struct virtio_pci_cap` (virtio-v1.1 4.1.4.3).
pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

/// One located virtio-pci capability: which BAR it lives in, and the byte
/// offset/length within that BAR.
#[derive(Clone, Copy)]
pub struct VirtioCap {
    pub bar: u8,
    pub offset: u32,
    /// Read for documentation/bounds-checking purposes; not otherwise used.
    pub length: u32,
    /// Only meaningful for the notify capability (`cfg_type == 2`); zero for
    /// every other capability type. See [`find_virtio_caps`].
    pub notify_off_multiplier: u32,
}

/// The virtio-pci capabilities this transport cares about.
pub struct VirtioCaps {
    pub common: VirtioCap,
    pub notify: VirtioCap,
    /// The device-specific config capability (`cfg_type == 4`), present only
    /// on devices that have one and only interesting to drivers that read it
    /// — virtio-blk's `capacity` lives there (virtio-v1.1 5.2.4's
    /// `struct virtio_blk_config`), while virtio-input and virtio-gpu never
    /// touch theirs.
    pub device_cfg: Option<VirtioCap>,
}

/// Walk `device_handle`'s PCI capability list (via
/// `OP_DEVICE_PCI_CONFIG_READ`) to find the virtio-pci common-config, notify
/// and (optionally) device-config capabilities. See the crate doc comment for
/// why the common config's location can't simply be assumed to be BAR 0
/// offset 0.
///
/// # Landmine: `notify_off_multiplier`
///
/// `struct virtio_pci_notify_cap` (virtio-v1.1 4.1.4.4) is *not* a plain
/// `virtio_pci_cap`: it appends a 4-byte `notify_off_multiplier` field
/// immediately after the standard 16-byte `virtio_pci_cap` header, i.e. at
/// `cap_ptr + 16`. That field exists only for `cfg_type ==
/// VIRTIO_PCI_CAP_NOTIFY_CFG`, which is why it is read only in that match
/// arm and left zero everywhere else. Reusing a generic capability struct
/// that never reads it — or reading it from the wrong offset — silently
/// yields a multiplier of 0, which collapses every queue's doorbell onto the
/// same address: a single-queue device keeps working by accident, and a
/// multi-queue one mysteriously never gets notified on any queue but the
/// first. The multiplier is consumed in [`VirtQueue::notify_addr`].
pub fn find_virtio_caps(device_handle: Handle) -> Result<VirtioCaps> {
    let status = device::device_pci_config_read(device_handle, PCI_STATUS_OFFSET, 2)?;
    if status & PCI_STATUS_CAP_LIST == 0 {
        return Err(ErrorCode::NotSupported);
    }

    let mut common: Option<VirtioCap> = None;
    let mut notify: Option<VirtioCap> = None;
    let mut device_cfg: Option<VirtioCap> = None;

    let mut cap_ptr = (device::device_pci_config_read(
        device_handle,
        PCI_CAPABILITIES_POINTER_OFFSET,
        1,
    )? as u8)
        & 0xFC;
    // Bound the walk: a well-formed capability list is a few entries long and
    // null-terminated, but a malformed/adversarial device could form a cycle
    // — cap this at a generous limit rather than looping forever.
    for _ in 0..64 {
        if cap_ptr == 0 {
            break;
        }
        let cap_id = device::device_pci_config_read(device_handle, cap_ptr as u32, 1)? as u8;
        let next = device::device_pci_config_read(device_handle, cap_ptr as u32 + 1, 1)? as u8;

        if cap_id == PCI_CAP_ID_VNDR {
            let cfg_type = device::device_pci_config_read(device_handle, cap_ptr as u32 + 3, 1)? as u8;
            let bar = device::device_pci_config_read(device_handle, cap_ptr as u32 + 4, 1)? as u8;
            let offset = device::device_pci_config_read(device_handle, cap_ptr as u32 + 8, 4)?;
            let length = device::device_pci_config_read(device_handle, cap_ptr as u32 + 12, 4)?;
            match cfg_type {
                VIRTIO_PCI_CAP_COMMON_CFG => {
                    common = Some(VirtioCap { bar, offset, length, notify_off_multiplier: 0 });
                }
                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                    // The extra field only `virtio_pci_notify_cap` has — see
                    // this function's "Landmine" doc section.
                    let notify_off_multiplier =
                        device::device_pci_config_read(device_handle, cap_ptr as u32 + 16, 4)?;
                    notify = Some(VirtioCap { bar, offset, length, notify_off_multiplier });
                }
                VIRTIO_PCI_CAP_DEVICE_CFG => {
                    device_cfg = Some(VirtioCap { bar, offset, length, notify_off_multiplier: 0 });
                }
                _ => {}
            }
        }

        cap_ptr = next & 0xFC;
    }

    match (common, notify) {
        (Some(common), Some(notify)) => Ok(VirtioCaps { common, notify, device_cfg }),
        _ => Err(ErrorCode::NotSupported),
    }
}

// =============================================================================
// Virtio device status/feature negotiation (virtio-v1.1 spec section 2.1,
// and 4.1.4.3's `device_status`/`device_feature*`/`driver_feature*` fields of
// `struct virtio_pci_common_cfg`).
// =============================================================================

pub const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
pub const VIRTIO_STATUS_DRIVER: u8 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 8;

/// Feature bit 32 (`VIRTIO_F_VERSION_1`) — the one feature a modern-only
/// driver must accept; without it the device would fall back to legacy
/// semantics none of these drivers implement.
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// Byte offsets into `struct virtio_pci_common_cfg` (virtio-v1.1 4.1.4.3).
pub mod common_cfg {
    pub const DEVICE_FEATURE_SELECT: usize = 0;
    pub const DEVICE_FEATURE: usize = 4;
    pub const DRIVER_FEATURE_SELECT: usize = 8;
    pub const DRIVER_FEATURE: usize = 12;
    /// Documented for completeness (full `virtio_pci_common_cfg` layout) even
    /// though no driver here assigns a config-change interrupt vector.
    #[allow(dead_code)]
    pub const MSIX_CONFIG: usize = 16;
    pub const DEVICE_STATUS: usize = 20;
    pub const QUEUE_SELECT: usize = 22;
    pub const QUEUE_SIZE: usize = 24;
    pub const QUEUE_MSIX_VECTOR: usize = 26;
    pub const QUEUE_ENABLE: usize = 28;
    pub const QUEUE_NOTIFY_OFF: usize = 30;
    pub const QUEUE_DESC: usize = 32;
    pub const QUEUE_DRIVER: usize = 40; // avail ring
    pub const QUEUE_DEVICE: usize = 48; // used ring
}

/// MSI-X table entry every driver's single interrupt subscription configures
/// (`DeviceResource::subscribe_irq` always configures table entry 0 — see that
/// method's doc comment). Routing a queue's interrupts there means no
/// ISR-status register read is needed to acknowledge interrupts (that is only
/// required for the legacy/no-MSI-X interrupt path).
pub const MSIX_VECTOR_0: u16 = 0;

// =============================================================================
// Split virtqueue (virtio-v1.1 spec section 2.6).
// =============================================================================

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Bytes per descriptor-table entry: addr:u64 + len:u32 + flags:u16 + next:u16.
const DESC_ENTRY_LEN: usize = 16;
/// Bytes per used-ring entry: id:u32 + len:u32.
const USED_ENTRY_LEN: usize = 8;

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// What a driver wants from [`VirtioTransport::setup_queue`].
pub struct QueueConfig {
    /// Virtqueue index to select (`common_cfg.queue_select`).
    pub index: u16,
    /// Queue size the driver would like. Clamped down to the device's
    /// advertised maximum and rounded down to a power of two.
    pub wanted_size: u16,
    /// Smallest usable queue size for this driver — setup fails with
    /// `NotSupported` if the clamped size falls below it (e.g. virtio-blk
    /// needs at least three descriptors for its header/data/status chains).
    pub min_size: u16,
    /// Fixed-size driver-owned scratch bytes to carve out of the same DMA
    /// allocation, after the rings.
    pub scratch_len: usize,
    /// Additional driver-owned scratch bytes per descriptor (e.g. one
    /// fixed-size input-event buffer per descriptor). Total scratch is
    /// `scratch_len + scratch_len_per_descriptor * queue_size`.
    pub scratch_len_per_descriptor: usize,
}

/// One drained used-ring entry (virtio-v1.1 2.6.8's `struct virtq_used_elem`).
pub struct UsedElem {
    /// Index of the head descriptor of the completed chain.
    pub id: u32,
    /// Number of bytes the device wrote into the chain's device-writable
    /// buffers.
    pub len: u32,
}

/// A split virtqueue: its DMA-backed ring memory, the driver-side cursors
/// needed to publish buffers and drain completions, and the notify offset its
/// doorbell lives at.
///
/// The rings and the driver's scratch area share one DMA allocation. Offsets
/// held here are relative to that allocation's base — both its virtual
/// address (for driver-side reads/writes through `ring`) and its IOVA (for the
/// addresses programmed into the device's `queue_desc`/`queue_driver`/
/// `queue_device` registers and into each descriptor's `addr` field).
pub struct VirtQueue {
    ring: MmioRegion,
    iova: u64,
    /// Kept only so the DMA allocation's virtual address is available for
    /// `dma_free` if a driver ever needs to tear a queue down explicitly.
    /// Today none does: dropping the device handle frees every DMA allocation
    /// associated with the device (see `DeviceResource::teardown`).
    #[allow(dead_code)]
    virt_addr: usize,
    #[allow(dead_code)]
    dma_size: usize,
    index: u16,
    qsize: u16,
    desc_off: usize,
    avail_off: usize,
    used_off: usize,
    scratch_off: usize,
    /// Byte offset of this queue's doorbell within the notify capability's
    /// mapped region — see [`Self::notify_addr`].
    notify_addr: usize,
    /// Driver-side copy of how many used-ring entries have been consumed so
    /// far; compared against the device-owned `used.idx` to find new entries.
    last_used_idx: u16,
    /// Driver-side copy of how many avail-ring entries have been published so
    /// far; mirrored into the device-visible `avail.idx`.
    next_avail_idx: u16,
}

impl VirtQueue {
    /// This queue's negotiated size, in descriptors (always a power of two).
    pub fn size(&self) -> u16 {
        self.qsize
    }

    /// The driver-owned scratch area: the region to read/write through, and
    /// the byte offset its base sits at. Sub-layout within it is entirely the
    /// driver's business (see the crate doc comment's "Division of labour").
    pub fn scratch(&self) -> (&MmioRegion, usize) {
        (&self.ring, self.scratch_off)
    }

    /// Device-visible address of `offset` bytes into the scratch area — what
    /// a descriptor's `addr` field must carry.
    pub fn scratch_iova(&self, offset: usize) -> u64 {
        self.iova + (self.scratch_off + offset) as u64
    }

    /// Fill in descriptor-table entry `index` (virtio-v1.1 2.6.5).
    pub fn set_descriptor(&self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let off = self.desc_off + DESC_ENTRY_LEN * index as usize;
        self.ring.write::<u64>(off, addr);
        self.ring.write::<u32>(off + 8, len);
        self.ring.write::<u16>(off + 12, flags);
        self.ring.write::<u16>(off + 14, next);
    }

    /// Publish the descriptor chain headed at `head` on the avail ring,
    /// making it available to the device (virtio-v1.1 2.6.6). The device is
    /// not told about it until [`Self::kick`].
    pub fn publish(&mut self, head: u16) {
        let slot = self.next_avail_idx % self.qsize;
        self.ring.write::<u16>(self.avail_off + 4 + 2 * slot as usize, head);
        self.next_avail_idx = self.next_avail_idx.wrapping_add(1);
        self.ring.write::<u16>(self.avail_off + 2, self.next_avail_idx);
    }

    /// Take the next completed chain off the used ring, or `None` if the
    /// device has not completed anything new since the last call.
    pub fn pop_used(&mut self) -> Option<UsedElem> {
        let used_idx = self.ring.read::<u16>(self.used_off + 2);
        if self.last_used_idx == used_idx {
            return None;
        }
        let slot = self.last_used_idx % self.qsize;
        let entry_off = self.used_off + 4 + USED_ENTRY_LEN * slot as usize;
        let id = self.ring.read::<u32>(entry_off);
        let len = self.ring.read::<u32>(entry_off + 4);
        self.last_used_idx = self.last_used_idx.wrapping_add(1);
        Some(UsedElem { id, len })
    }

    /// Byte offset of this queue's doorbell within the notify capability's
    /// mapped region.
    ///
    /// # Landmine: notify address computation
    ///
    /// virtio-v1.1 4.1.4.4 defines it as
    /// `notify_cap.offset + queue_notify_off * notify_off_multiplier`, where
    /// `queue_notify_off` is read from `common_cfg.queue_notify_off` *while
    /// this queue is selected* (so it must be read during queue setup, not
    /// later — it is constant for the queue's lifetime, which is why it is
    /// baked into this value once) and `notify_off_multiplier` comes from the
    /// notify capability's extra trailing field (see [`find_virtio_caps`]'s
    /// landmine note). Getting either factor wrong — most easily by reading
    /// the multiplier as 0 — points every queue's doorbell at the notify
    /// region's base, which a single-queue device tolerates silently and a
    /// multi-queue one does not.
    pub fn notify_addr(&self) -> usize {
        self.notify_addr
    }

    /// Ring this queue's doorbell (virtio-v1.1 4.1.4.4): a 16-bit
    /// little-endian write of the queue index at [`Self::notify_addr`].
    /// `notify` must be the region the notify capability was mapped into —
    /// [`VirtioTransport::notify_region`] always returns the right one.
    ///
    /// The device does not proactively poll the avail ring, so a driver must
    /// kick after making buffers available (virtio-v1.1 2.6.13). See
    /// [`VirtioTransport::set_driver_ok`] for the ordering landmine around
    /// the *first* such kick.
    pub fn kick(&self, notify: &MmioRegion) {
        notify.write::<u16>(self.notify_addr, self.index);
    }
}

// =============================================================================
// Transport
// =============================================================================

/// A claimed virtio-pci device with its capability regions mapped and its
/// features negotiated: everything a driver needs before it can set up queues
/// and talk device-specific protocol.
pub struct VirtioTransport {
    device_handle: Handle,
    common_mmio: MmioRegion,
    /// `None` when the notify capability shares the common config's BAR (very
    /// common — QEMU's virtio-pci puts everything on BAR 4). Mapping the same
    /// BAR twice would otherwise carve out two separate ranges of this
    /// process's address space for the same physical pages.
    notify_mmio: Option<MmioRegion>,
    /// `None` when there is no device-config capability, or when it shares a
    /// BAR that is already mapped.
    device_cfg_mmio: Option<MmioRegion>,
    caps: VirtioCaps,
}

impl VirtioTransport {
    /// Claim the device behind `token`, walk its virtio-pci capabilities, map
    /// the BARs they live in, and run the modern feature-negotiation
    /// handshake (virtio-v1.1 3.1.1) accepting `driver_features` on top of the
    /// mandatory `VIRTIO_F_VERSION_1`.
    ///
    /// On return the device is in the `ACKNOWLEDGE | DRIVER | FEATURES_OK`
    /// state: queues may be set up, but `DRIVER_OK` is deliberately *not* set
    /// — see [`Self::set_driver_ok`].
    pub fn claim_and_init(token: Handle, driver_features: u64) -> Result<Self> {
        let device_handle = device::device_claim(token)?;

        let caps = find_virtio_caps(device_handle)?;

        let common_mmio = device::device_map_mmio(device_handle, caps.common.bar as u32)?;
        let notify_mmio = if caps.notify.bar == caps.common.bar {
            None
        } else {
            Some(device::device_map_mmio(device_handle, caps.notify.bar as u32)?)
        };
        let device_cfg_mmio = match caps.device_cfg {
            Some(cap) if cap.bar != caps.common.bar && cap.bar != caps.notify.bar => {
                Some(device::device_map_mmio(device_handle, cap.bar as u32)?)
            }
            _ => None,
        };

        let transport = Self { device_handle, common_mmio, notify_mmio, device_cfg_mmio, caps };
        transport.negotiate_features(driver_features)?;
        Ok(transport)
    }

    /// The claimed device's handle, for the driver's own `dma_alloc` needs
    /// (e.g. virtio-gpu's framebuffer) beyond what its queues use.
    pub fn device_handle(&self) -> Handle {
        self.device_handle
    }

    /// The mapped common-config region and the byte offset the
    /// `virtio_pci_common_cfg` structure starts at within it.
    pub fn common(&self) -> (&MmioRegion, usize) {
        (&self.common_mmio, self.caps.common.offset as usize)
    }

    /// The region a queue's doorbell must be written through — the notify
    /// capability's own mapping when it lives in its own BAR, otherwise the
    /// common-config mapping that already covers it.
    pub fn notify_region(&self) -> &MmioRegion {
        self.notify_mmio.as_ref().unwrap_or(&self.common_mmio)
    }

    /// The mapped device-specific config region and the byte offset the
    /// device's config structure starts at within it, or `None` if this
    /// device advertises no device-config capability.
    pub fn device_config(&self) -> Option<(&MmioRegion, usize)> {
        let cap = self.caps.device_cfg?;
        let region = if let Some(region) = self.device_cfg_mmio.as_ref() {
            region
        } else if cap.bar == self.caps.common.bar {
            &self.common_mmio
        } else {
            self.notify_region()
        };
        Some((region, cap.offset as usize))
    }

    /// Subscribe to (and thereby enable, via `enable_msix` —
    /// `DeviceResource::subscribe_irq`) this device's MSI-X vector, posting
    /// `EVENT_DEVICE_IRQ` to `mailbox`.
    ///
    /// Call this *before* [`Self::setup_queue`] programs `queue_msix_vector`
    /// to point at that vector, and before [`Self::set_driver_ok`], so the
    /// device never has a window in which it could complete a request against
    /// an interrupt source that isn't wired up yet.
    pub fn subscribe_irq(&self, mailbox: &Mailbox) -> Result<()> {
        device::device_subscribe_irq(self.device_handle, mailbox.handle())
    }

    /// Reset the device and run the modern feature-negotiation handshake
    /// (virtio-v1.1 3.1.1): `ACKNOWLEDGE`, `DRIVER`, exchange feature bits,
    /// `FEATURES_OK`, then confirm `FEATURES_OK` stuck.
    ///
    /// `VIRTIO_F_VERSION_1` is always required and always accepted: a device
    /// that doesn't offer it is a legacy device none of these drivers can
    /// speak to. `driver_features` adds any device-specific bits the caller
    /// wants (today every driver passes none).
    fn negotiate_features(&self, driver_features: u64) -> Result<()> {
        let (common, off) = self.common();
        common.write::<u8>(off + common_cfg::DEVICE_STATUS, 0);
        common.write::<u8>(off + common_cfg::DEVICE_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
        common.write::<u8>(
            off + common_cfg::DEVICE_STATUS,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
        );

        let wanted = driver_features | VIRTIO_F_VERSION_1;

        // Feature bits are exchanged one 32-bit dword at a time, selected by
        // `*_feature_select` (0 = bits 0-31, 1 = bits 32-63).
        common.write::<u32>(off + common_cfg::DEVICE_FEATURE_SELECT, 1);
        let device_feature_high = common.read::<u32>(off + common_cfg::DEVICE_FEATURE);
        if device_feature_high & (VIRTIO_F_VERSION_1 >> 32) as u32 == 0 {
            // Not a modern (virtio 1.0+) device.
            return Err(ErrorCode::NotSupported);
        }
        common.write::<u32>(off + common_cfg::DRIVER_FEATURE_SELECT, 0);
        common.write::<u32>(off + common_cfg::DRIVER_FEATURE, wanted as u32);
        common.write::<u32>(off + common_cfg::DRIVER_FEATURE_SELECT, 1);
        common.write::<u32>(off + common_cfg::DRIVER_FEATURE, (wanted >> 32) as u32);

        common.write::<u8>(
            off + common_cfg::DEVICE_STATUS,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
        );
        let status = common.read::<u8>(off + common_cfg::DEVICE_STATUS);
        if status & VIRTIO_STATUS_FEATURES_OK == 0 {
            return Err(ErrorCode::NotSupported);
        }
        Ok(())
    }

    /// Select queue `cfg.index`, size it, allocate and lay out its DMA
    /// backing (descriptor table + avail ring + used ring + the driver's
    /// scratch area), program the device's queue registers, route its
    /// completions to MSI-X vector 0, and enable it.
    ///
    /// The returned [`VirtQueue`] has no buffers published yet: the driver
    /// fills descriptors and calls [`VirtQueue::publish`] for whatever its
    /// protocol needs.
    ///
    /// # Landmine: queue-size clamp *and writeback*
    ///
    /// The driver's chosen size must be written back to the device's
    /// `queue_size` register before `queue_enable` (virtio-v1.1 4.1.4.3.2:
    /// the register's reset value is the device's maximum, and the driver may
    /// only shrink it). The device uses *that register*, not its own maximum,
    /// to compute avail/used ring positions modulo the ring size. Omitting
    /// the writeback was a real bug in virtio-blk: with a clamped size (64)
    /// smaller than QEMU's virtio-blk-pci default maximum (256), the device
    /// kept computing ring positions modulo 256, so once the driver's
    /// `avail.idx` passed 64 the device read out of bounds past the end of
    /// the driver's smaller avail-ring DMA allocation — surfacing as QEMU
    /// logging "virtio-blk missing headers" with the request hanging forever,
    /// reproducibly at request #65 (and at #9 with an initially-tried size of
    /// 8), root-caused via `ext2_create_test`/`ext2_mkdir_test`. virtio-gpu
    /// hit the same thing from the other end: without the writeback, QEMU
    /// logged "Guest says index NNN is available" once its first
    /// out-of-bounds `avail.idx` read returned garbage. The writeback below
    /// is unconditional, so no driver can forget it.
    pub fn setup_queue(&self, cfg: QueueConfig) -> Result<VirtQueue> {
        let (common, off) = self.common();

        common.write::<u16>(off + common_cfg::QUEUE_SELECT, cfg.index);
        let max_size = common.read::<u16>(off + common_cfg::QUEUE_SIZE);
        if max_size == 0 || cfg.wanted_size == 0 {
            return Err(ErrorCode::NotSupported);
        }
        let clamped = cfg.wanted_size.min(max_size);
        // Round down to a power of two (virtio queue sizes must be one; the
        // device's own max is guaranteed to be, but the clamp might not be).
        let qsize = 1u16 << (15 - clamped.leading_zeros() as u16).min(15);
        if qsize < cfg.min_size {
            return Err(ErrorCode::NotSupported);
        }
        // See this method's "Landmine" doc section.
        common.write::<u16>(off + common_cfg::QUEUE_SIZE, qsize);

        let qsize_usize = qsize as usize;
        let desc_off = 0;
        let avail_off = desc_off + DESC_ENTRY_LEN * qsize_usize;
        let avail_len = 4 + 2 * qsize_usize; // flags:u16 + idx:u16 + ring[qsize]:u16
        let used_off = align_up(avail_off + avail_len, 4);
        let used_len = 4 + USED_ENTRY_LEN * qsize_usize; // flags:u16 + idx:u16 + ring[qsize]
        let scratch_off = align_up(used_off + used_len, 8);
        let scratch_len = cfg.scratch_len + cfg.scratch_len_per_descriptor * qsize_usize;
        let total_len = scratch_off + scratch_len;

        let (virt_addr, iova) = device::dma_alloc(self.device_handle, total_len)?;
        // Safety: `dma_alloc` just mapped `total_len` bytes of DMA-visible
        // memory at `virt_addr` into this process, exclusively for this
        // queue's use — `MmioRegion` is repurposed here for its
        // bounds-checked volatile-only access, not because this is device
        // MMIO.
        let ring = unsafe { MmioRegion::new(virt_addr as *mut u8, total_len) };

        ring.write::<u16>(avail_off, 0); // avail.flags
        ring.write::<u16>(avail_off + 2, 0); // avail.idx
        ring.write::<u16>(used_off, 0); // used.flags
        ring.write::<u16>(used_off + 2, 0); // used.idx

        common.write::<u64>(off + common_cfg::QUEUE_DESC, iova + desc_off as u64);
        common.write::<u64>(off + common_cfg::QUEUE_DRIVER, iova + avail_off as u64);
        common.write::<u64>(off + common_cfg::QUEUE_DEVICE, iova + used_off as u64);
        common.write::<u16>(off + common_cfg::QUEUE_MSIX_VECTOR, MSIX_VECTOR_0);
        let queue_notify_off = common.read::<u16>(off + common_cfg::QUEUE_NOTIFY_OFF);
        common.write::<u16>(off + common_cfg::QUEUE_ENABLE, 1);

        // See `VirtQueue::notify_addr`'s landmine note.
        let notify_addr = self.caps.notify.offset as usize
            + queue_notify_off as usize * self.caps.notify.notify_off_multiplier as usize;

        Ok(VirtQueue {
            ring,
            iova,
            virt_addr,
            dma_size: total_len,
            index: cfg.index,
            qsize,
            desc_off,
            avail_off,
            used_off,
            scratch_off,
            notify_addr,
            last_used_idx: 0,
            next_avail_idx: 0,
        })
    }

    /// Set `DRIVER_OK`, completing the initialisation handshake
    /// (virtio-v1.1 3.1.1): the device is now live.
    ///
    /// # Landmine: kick before `DRIVER_OK`, not after
    ///
    /// A driver whose queue already has buffers published at init time must
    /// call [`VirtQueue::kick`] **before** this, not after. That is why this
    /// is a separate call the driver makes explicitly rather than something
    /// [`Self::setup_queue`] does for it.
    ///
    /// virtio-keyboard's eventq is the case that needs it: every descriptor
    /// is published as available during setup, and the device does not
    /// proactively poll the avail ring (virtio-v1.1 2.6.13). An earlier
    /// version of that driver kicked *after* `DRIVER_OK` — reasoning from
    /// 3.1.1's description of `DRIVER_OK` as the last step — and QEMU then
    /// never touched the virtqueue at all: the eventq sat idle forever
    /// because the driver's only other kick happens after draining a used
    /// entry, and no first event ever arrived to trigger one. Kicking before
    /// `DRIVER_OK` matches what the kernel drivers' old dependency did
    /// (`vendor/virtio-drivers/src/device/input.rs::VirtIOInput::new` notifies
    /// the queue and only then calls `finish_init()` to set `DRIVER_OK`).
    ///
    /// Drivers whose queues start empty (virtio-blk's requestq,
    /// virtio-gpu's controlq) have nothing to kick at init time and simply
    /// call this straight after `setup_queue`; their later kicks accompany
    /// each request they submit.
    pub fn set_driver_ok(&self) {
        let (common, off) = self.common();
        common.write::<u8>(
            off + common_cfg::DEVICE_STATUS,
            VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK,
        );
    }
}
