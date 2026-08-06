//! Userspace virtio-keyboard driver (Phase 7 of `plans/device-driver-model.md`).
//!
//! Replaces the deleted kernel-resident driver
//! (`panda-kernel/src/devices/virtio_keyboard.rs`). Follows the lifecycle
//! documented in the plan's "Driver lifecycle" section: subscribe to PCI
//! device events, claim the device on `EVENT_DEVICE_ADDED`, walk its virtio
//! PCI capability list, map the common-config/notify BARs, negotiate virtio
//! features, set up the eventq split virtqueue, subscribe to its IRQ,
//! register the `keyboard:` scheme, and serve clients until
//! `EVENT_DEVICE_REMOVED`.
//!
//! # Virtio transport: hand-rolled, not `virtio_drivers`
//!
//! The deleted kernel driver used the `virtio_drivers` crate's
//! `PciTransport`, which is kernel-only (it assumes direct physical-memory
//! access and a `Hal` trait implemented against kernel internals — see
//! `panda-kernel/src/devices/virtio_hal.rs`). A userspace driver has none of
//! that: it only has `OP_DEVICE_*`/`OP_DMA_*` syscalls
//! (`libpanda::device`). Two consequences drove what's implemented here:
//!
//! 1. **PCI capability walk**: a modern (virtio 1.0) virtio-pci device
//!    exposes its common-config/notify/ISR/device-config register blocks
//!    through PCI capabilities (vendor-specific capability id `0x09`), each
//!    naming a BAR + byte offset — it is *not* safe to assume BAR 0 offset 0
//!    is the common config, unlike a naive legacy-virtio-pci reading.
//!    `OP_DEVICE_MAP_MMIO` only maps a BAR by index; it does not know
//!    anything about virtio. So this driver reads raw PCI config space
//!    itself via `OP_DEVICE_PCI_CONFIG_READ` (added in this phase — see its
//!    doc comment in `panda_abi::device`) and walks the capability list by
//!    hand (`find_virtio_caps`) to find the common-config and notify
//!    capabilities' BAR/offset, matching what `PciTransport::new` does
//!    internally in the kernel driver's dependency.
//! 2. **Split virtqueue**: with no in-kernel virtio transport to build the
//!    descriptor table/avail ring/used ring, this driver lays them out by
//!    hand in the DMA buffer `init_device` allocates (`setup_eventq`), and
//!    drains the used ring itself in `poll_device` — see that function's
//!    doc comment for the recycle-and-renotify loop.
//!
//! Only the virtio-input **eventq** (queue index 0) is set up; the
//! statusq (index 1, host->guest LED state etc.) is not needed to read key
//! events and is left disabled, matching the deleted kernel driver's scope
//! (it only ever drained input events, never drove the statusq either).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use libpanda::device::{
    self, BusType, DeviceEvent, EVENT_DEVICE_ADDED, EVENT_DEVICE_IRQ, EVENT_DEVICE_REMOVED,
    MmioRegion, PciDeviceId,
};
use libpanda::error::Result;
use libpanda::keyboard::RawInputEvent;
use libpanda::mailbox::Mailbox;
use libpanda::scheme::SchemeProvider;
use libpanda::{Handle, environment, process};

/// Alias for readability at device-syscall call sites: a claimed device's
/// handle, as returned by `device::device_claim`.
type DeviceHandle = Handle;
use panda_abi::ErrorCode;
use panda_abi::scheme_protocol::{Request, Response};

// The virtio-input device ID the deleted kernel driver matched
// (`panda-kernel/src/devices.rs`'s old `(0x1AF4, 0x1052, 0x00) =>
// virtio_keyboard::init_from_pci_device(pci_device)` arm). 0x1AF4 is the
// Red Hat/virtio PCI vendor ID; 0x1052 is the "virtio 1.0 input device" ID.
const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_INPUT_DEVICE_ID: u16 = 0x1052;

libpanda::pci_device_table![{ vendor: VIRTIO_VENDOR_ID, device: VIRTIO_INPUT_DEVICE_ID }];

// Virtio input event type we care about (evdev `EV_KEY`), matching
// `panda-kernel/src/devices/virtio_keyboard.rs`'s `EV_KEY` constant.
const EV_KEY: u16 = 0x01;

// =============================================================================
// PCI config space: status register, capabilities list, virtio-pci caps
// (virtio-v1.1 spec section 4.1.4).
// =============================================================================

/// PCI config space offset of the 16-bit status register. Bit 4
/// (`PCI_STATUS_CAP_LIST`) indicates a capabilities list is present.
const PCI_STATUS_OFFSET: u32 = 0x06;
const PCI_STATUS_CAP_LIST: u32 = 1 << 4;
/// PCI config space offset of the 8-bit "capabilities pointer" (offset of
/// the first capability, only valid if `PCI_STATUS_CAP_LIST` is set).
const PCI_CAPABILITIES_POINTER_OFFSET: u32 = 0x34;
/// Standard PCI capability ID for a vendor-specific capability — every
/// virtio-pci capability structure uses this ID (virtio-v1.1 4.1.4).
const PCI_CAP_ID_VNDR: u8 = 0x09;

/// `cfg_type` values from `struct virtio_pci_cap` (virtio-v1.1 4.1.4.3).
const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;

/// One located virtio-pci capability: which BAR it lives in, and the byte
/// offset/length within that BAR. `notify_off_multiplier` is only
/// meaningful for the notify capability (see `find_virtio_caps`).
#[derive(Clone, Copy)]
struct VirtioCap {
    bar: u8,
    offset: u32,
    #[allow(dead_code)] // Read for documentation/bounds-checking purposes; not otherwise used.
    length: u32,
    notify_off_multiplier: u32,
}

/// Walk `device_handle`'s PCI capability list (via
/// `OP_DEVICE_PCI_CONFIG_READ`) to find the virtio-pci common-config and
/// notify capabilities. See the module doc comment for why this can't be
/// assumed to be BAR 0 offset 0.
fn find_virtio_caps(device_handle: DeviceHandle) -> Result<(VirtioCap, VirtioCap)> {
    let status = device::device_pci_config_read(device_handle, PCI_STATUS_OFFSET, 2)?;
    if status & PCI_STATUS_CAP_LIST == 0 {
        return Err(ErrorCode::NotSupported);
    }

    let mut common: Option<VirtioCap> = None;
    let mut notify: Option<VirtioCap> = None;

    let mut cap_ptr =
        (device::device_pci_config_read(device_handle, PCI_CAPABILITIES_POINTER_OFFSET, 1)? as u8) & 0xFC;
    // Bound the walk: a well-formed capability list is a few entries long
    // and null-terminated, but a malformed/adversarial device could form a
    // cycle — cap this at a generous limit rather than looping forever.
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
                    let notify_off_multiplier =
                        device::device_pci_config_read(device_handle, cap_ptr as u32 + 16, 4)?;
                    notify = Some(VirtioCap { bar, offset, length, notify_off_multiplier });
                }
                _ => {}
            }
        }

        cap_ptr = next & 0xFC;
    }

    match (common, notify) {
        (Some(common), Some(notify)) => Ok((common, notify)),
        _ => Err(ErrorCode::NotSupported),
    }
}

// =============================================================================
// Virtio device status/feature negotiation (virtio-v1.1 spec section 2.1,
// 4.1.4.3's `device_status`/`device_feature*`/`driver_feature*` fields of
// `struct virtio_pci_common_cfg`).
// =============================================================================

const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;

/// Feature bit 32 (`VIRTIO_F_VERSION_1`) — the one feature a modern-only
/// driver like this one must accept; without it the device would fall back
/// to legacy semantics this driver doesn't implement.
const VIRTIO_F_VERSION_1_HIGH_BIT: u32 = 1 << 0; // bit 32 overall = bit 0 of the high dword

/// Byte offsets into `struct virtio_pci_common_cfg` (virtio-v1.1 4.1.4.3).
mod common_cfg {
    pub const DEVICE_FEATURE_SELECT: usize = 0;
    pub const DEVICE_FEATURE: usize = 4;
    pub const DRIVER_FEATURE_SELECT: usize = 8;
    pub const DRIVER_FEATURE: usize = 12;
    // Documented for completeness (full `virtio_pci_common_cfg` layout) even
    // though this driver never assigns a config-change interrupt vector.
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

/// The eventq (virtio-input queue index 0) — the only queue this driver
/// uses (see the module doc comment).
const EVENTQ_INDEX: u16 = 0;

/// MSI-X table entry this driver's single interrupt subscription
/// configures (`DeviceResource::subscribe_irq` always configures table
/// entry 0 — see that method's doc comment). Routing the eventq's
/// interrupts there means no ISR-status register read is needed to
/// acknowledge interrupts (that's only required for the legacy/no-MSI-X
/// interrupt path).
const MSIX_VECTOR_0: u16 = 0;

/// Maximum key events buffered before the oldest is dropped, matching the
/// deleted kernel driver's `RingBuffer<64>`. Also the eventq's requested
/// queue size, clamped to the device's actual maximum in `setup_eventq`.
const EVENT_BUFFER_CAPACITY: usize = 64;

/// Reset the device, negotiate `VIRTIO_F_VERSION_1` only (this driver needs
/// no optional virtio-input feature bits), and confirm `FEATURES_OK` stuck
/// — the modern virtio feature negotiation handshake (virtio-v1.1 3.1.1).
fn negotiate_features(common: &MmioRegion, common_off: usize) -> Result<()> {
    common.write::<u8>(common_off + common_cfg::DEVICE_STATUS, 0);
    common.write::<u8>(common_off + common_cfg::DEVICE_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
    common.write::<u8>(
        common_off + common_cfg::DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    // Select the high feature dword (bits 32-63) and accept only
    // VIRTIO_F_VERSION_1 (bit 32). We don't need any low (0-31) feature
    // bits, so driver_feature_select=0/driver_feature=0 covers those.
    common.write::<u32>(common_off + common_cfg::DEVICE_FEATURE_SELECT, 1);
    let device_feature_high = common.read::<u32>(common_off + common_cfg::DEVICE_FEATURE);
    if device_feature_high & VIRTIO_F_VERSION_1_HIGH_BIT == 0 {
        // Not a modern (virtio 1.0+) device — this driver can't speak
        // legacy virtio.
        return Err(ErrorCode::NotSupported);
    }
    common.write::<u32>(common_off + common_cfg::DRIVER_FEATURE_SELECT, 0);
    common.write::<u32>(common_off + common_cfg::DRIVER_FEATURE, 0);
    common.write::<u32>(common_off + common_cfg::DRIVER_FEATURE_SELECT, 1);
    common.write::<u32>(
        common_off + common_cfg::DRIVER_FEATURE,
        VIRTIO_F_VERSION_1_HIGH_BIT,
    );

    common.write::<u8>(
        common_off + common_cfg::DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    let status = common.read::<u8>(common_off + common_cfg::DEVICE_STATUS);
    if status & VIRTIO_STATUS_FEATURES_OK == 0 {
        return Err(ErrorCode::NotSupported);
    }
    Ok(())
}

// =============================================================================
// Split virtqueue (virtio-v1.1 spec section 2.6): descriptor table, avail
// ring, used ring, laid out by hand in the DMA buffer `init_device`
// allocates, since there is no in-kernel virtio transport for userspace
// drivers to delegate this to (see the module doc comment).
// =============================================================================

const VIRTQ_DESC_F_WRITE: u16 = 2;
const DESC_ENTRY_LEN: usize = 16; // addr:u64 + len:u32 + flags:u16 + next:u16
const USED_ENTRY_LEN: usize = 8; // id:u32 + len:u32
const INPUT_EVENT_LEN: usize = 8; // matches libpanda::keyboard::RawInputEvent

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// Byte layout of the eventq's split virtqueue plus its event-buffer pool,
/// all packed into one DMA allocation. Offsets are relative to the DMA
/// buffer's base (both `virt_addr`, for driver-side reads/writes through
/// `ring`, and `iova`, for the addresses programmed into the device's
/// `queue_desc`/`queue_driver`/`queue_device` registers and each
/// descriptor's `addr` field).
struct QueueLayout {
    qsize: u16,
    desc_off: usize,
    avail_off: usize,
    used_off: usize,
    events_off: usize,
    total_len: usize,
}

impl QueueLayout {
    fn compute(qsize: u16) -> Self {
        let qsize_usize = qsize as usize;
        let desc_off = 0;
        let avail_off = desc_off + DESC_ENTRY_LEN * qsize_usize;
        let avail_len = 4 + 2 * qsize_usize; // flags:u16 + idx:u16 + ring[qsize]:u16
        let used_off = align_up(avail_off + avail_len, 4);
        let used_len = 4 + USED_ENTRY_LEN * qsize_usize; // flags:u16 + idx:u16 + ring[qsize]
        let events_off = align_up(used_off + used_len, 8);
        let total_len = events_off + INPUT_EVENT_LEN * qsize_usize;
        Self { qsize, desc_off, avail_off, used_off, events_off, total_len }
    }

    fn desc_addr_off(&self, i: u16) -> usize {
        self.desc_off + DESC_ENTRY_LEN * i as usize
    }

    fn avail_ring_off(&self, i: u16) -> usize {
        self.avail_off + 4 + 2 * i as usize
    }

    fn used_ring_off(&self, i: u16) -> usize {
        self.used_off + 4 + USED_ENTRY_LEN * i as usize
    }

    fn event_buf_off(&self, i: u16) -> usize {
        self.events_off + INPUT_EVENT_LEN * i as usize
    }
}

/// Claim the device, walk its virtio-pci capabilities, map the common
/// config / notify BARs, negotiate features, allocate and lay out the
/// eventq, enable it, subscribe to IRQs, and set `device_status` to
/// `DRIVER_OK`. Mirrors `panda-kernel/src/devices/virtio_keyboard.rs
/// ::init_from_pci_device`'s sequence (claim -> transport setup -> feature
/// negotiation -> queue setup -> `DRIVER_OK` -> IRQ registration), but
/// through the userspace `OP_DEVICE_*` syscalls and a hand-rolled virtio
/// transport instead of the kernel-only `virtio_drivers` crate.
fn init_device(token: Handle) -> Result<DeviceState> {
    let device_handle: DeviceHandle = device::device_claim(token)?;

    let (common_cap, notify_cap) = find_virtio_caps(device_handle)?;

    let common_mmio = device::device_map_mmio(device_handle, common_cap.bar as u32)?;
    // The notify capability is very often on the same BAR as common config
    // (QEMU's virtio-pci puts everything on BAR 4) — avoid mapping the same
    // BAR twice, which would otherwise carve out two separate ranges of
    // this process's address space for the same physical pages.
    let notify_mmio = if notify_cap.bar == common_cap.bar {
        None
    } else {
        Some(device::device_map_mmio(device_handle, notify_cap.bar as u32)?)
    };

    negotiate_features(&common_mmio, common_cap.offset as usize)?;

    // Subscribe to (and thereby enable, via `enable_msix` —
    // `DeviceResource::subscribe_irq`) the device's MSI-X vector *before*
    // `setup_eventq` programs `queue_msix_vector` to point at it and
    // *before* `DRIVER_OK` is set, so the device never has a window where
    // it could complete a request against an interrupt source that isn't
    // wired up yet (virtio-v1.1 3.1.1 describes `DRIVER_OK` as the last
    // status bit set, after the device is fully configured).
    let mailbox = Mailbox::default();
    device::device_subscribe_irq(device_handle, mailbox.handle())?;

    let queue = setup_eventq(device_handle, &common_mmio, common_cap.offset as usize)?;

    // Kick the queue once, immediately, *before* `DRIVER_OK`: the device
    // does not proactively poll the avail ring on its own (virtio-v1.1
    // 2.6.13 — a driver must notify after making buffers available), and
    // every descriptor was just published as available in `setup_eventq`
    // before the device had any reason to look. Without this initial kick,
    // the eventq sits idle forever: `poll_device`'s own `kick` call only
    // fires *after* draining a used-ring entry, so the very first event
    // would never arrive to trigger it.
    //
    // This ordering — kick *before* `DRIVER_OK`, not after — matches the
    // deleted kernel driver's dependency
    // (`vendor/virtio-drivers/src/device/input.rs::VirtIOInput::new`,
    // via `Transport::finish_init`): it calls `transport.notify(QUEUE_EVENT)`
    // right after populating the avail ring, then only afterwards calls
    // `finish_init()` to set `DRIVER_OK`. An earlier version of this driver
    // kicked *after* `DRIVER_OK` instead (reasoning from virtio-v1.1
    // 3.1.1's description of `DRIVER_OK` as "last" step), which turned out
    // to be the actual bug behind Phase 7's virtqueue never being touched
    // by QEMU — see `plans/device-driver-model.md`'s landed-deviations
    // note.
    let notify_region = notify_mmio.as_ref().unwrap_or(&common_mmio);
    queue.kick(device_handle, Some(notify_region), notify_cap.offset as usize, notify_cap.notify_off_multiplier);

    common_mmio.write::<u8>(
        common_cap.offset as usize + common_cfg::DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    Ok(DeviceState {
        device_handle,
        common_mmio,
        notify_mmio,
        notify_off: notify_cap.offset as usize,
        notify_off_multiplier: notify_cap.notify_off_multiplier,
        queue,
        events: Vec::with_capacity(EVENT_BUFFER_CAPACITY),
    })
}

/// Select the eventq, size it (clamped to the device's advertised maximum,
/// rounded down to a power of two), allocate its DMA backing (descriptor
/// table + avail ring + used ring + one fixed event buffer per descriptor),
/// pre-populate every descriptor as a device-writable buffer and make all
/// of them available (this queue is purely device-to-driver: the driver
/// never fills in event *contents*, only supplies empty buffers for the
/// device to write into — see `poll_device`), and enable the queue.
fn setup_eventq(device_handle: DeviceHandle, common: &MmioRegion, common_off: usize) -> Result<EventQueue> {
    common.write::<u16>(common_off + common_cfg::QUEUE_SELECT, EVENTQ_INDEX);
    let max_size = common.read::<u16>(common_off + common_cfg::QUEUE_SIZE);
    if max_size == 0 {
        return Err(ErrorCode::NotSupported);
    }
    let mut qsize = (EVENT_BUFFER_CAPACITY as u16).min(max_size);
    // Round down to a power of two (virtio queue sizes must be a power of
    // two; the device's own max is guaranteed to be one, but our clamp
    // might not be).
    qsize = 1u16 << (15 - qsize.leading_zeros() as u16).min(15);

    let layout = QueueLayout::compute(qsize);
    let (virt_addr, iova) = device::dma_alloc(device_handle, layout.total_len)?;
    // Safety: `dma_alloc` just mapped `layout.total_len` bytes of
    // DMA-visible memory at `virt_addr` into this process, exclusively for
    // this queue's use — `MmioRegion` here is repurposed for its
    // bounds-checked volatile-only access, not because this is device MMIO.
    let ring = unsafe { MmioRegion::new(virt_addr as *mut u8, layout.total_len) };

    for i in 0..qsize {
        let desc_off = layout.desc_addr_off(i);
        let buf_iova = iova + layout.event_buf_off(i) as u64;
        ring.write::<u64>(desc_off, buf_iova); // addr
        ring.write::<u32>(desc_off + 8, INPUT_EVENT_LEN as u32); // len
        ring.write::<u16>(desc_off + 12, VIRTQ_DESC_F_WRITE); // flags
        ring.write::<u16>(desc_off + 14, 0); // next (unused: single-descriptor chains)

        ring.write::<u16>(layout.avail_ring_off(i), i);
    }
    ring.write::<u16>(layout.avail_off, 0); // avail.flags
    ring.write::<u16>(layout.avail_off + 2, qsize); // avail.idx: every buffer is available
    ring.write::<u16>(layout.used_off, 0); // used.flags
    ring.write::<u16>(layout.used_off + 2, 0); // used.idx

    let desc_iova = iova + layout.desc_off as u64;
    let avail_iova = iova + layout.avail_off as u64;
    let used_iova = iova + layout.used_off as u64;
    common.write::<u64>(common_off + common_cfg::QUEUE_DESC, desc_iova);
    common.write::<u64>(common_off + common_cfg::QUEUE_DRIVER, avail_iova);
    common.write::<u64>(common_off + common_cfg::QUEUE_DEVICE, used_iova);
    common.write::<u16>(common_off + common_cfg::QUEUE_MSIX_VECTOR, MSIX_VECTOR_0);
    let queue_notify_off = common.read::<u16>(common_off + common_cfg::QUEUE_NOTIFY_OFF);
    common.write::<u16>(common_off + common_cfg::QUEUE_ENABLE, 1);
    Ok(EventQueue {
        ring,
        _virt_addr: virt_addr,
        _dma_size: layout.total_len,
        layout,
        queue_notify_off,
        last_used_idx: 0,
        next_avail_idx: qsize,
    })
}

/// The eventq's virtqueue state: the DMA-backed ring memory plus the
/// driver-side cursors needed to drain the used ring and recycle
/// descriptors back onto the avail ring (see `poll_device`).
struct EventQueue {
    ring: MmioRegion,
    /// Kept only so the DMA allocation's virtual address is available for
    /// `dma_free` if this driver ever needs to tear it down explicitly
    /// (today it doesn't: `DeviceState` is simply dropped on
    /// `EVENT_DEVICE_REMOVED`, and `OP_DEVICE_CLAIM`'s handle teardown frees
    /// every DMA allocation associated with the device — see
    /// `DeviceResource::teardown`).
    #[allow(dead_code)]
    _virt_addr: usize,
    #[allow(dead_code)]
    _dma_size: usize,
    layout: QueueLayout,
    /// `common_cfg.queue_notify_off` for this queue, read once at setup
    /// time (constant for the queue's lifetime) — combined with the notify
    /// capability's `notify_off_multiplier` to compute the notify register
    /// address in `kick`.
    queue_notify_off: u16,
    /// Driver-side copy of how many used-ring entries have been consumed so
    /// far; compared against the device-owned `used.idx` to find new
    /// entries.
    last_used_idx: u16,
    /// Driver-side copy of how many avail-ring entries have been published
    /// so far (starts at `qsize`, since every descriptor is made available
    /// up front in `setup_eventq`); advanced by one for every descriptor
    /// recycled back onto the avail ring in `poll_device`.
    next_avail_idx: u16,
}

impl EventQueue {
    /// Drain every newly-completed used-ring entry, decode its event,
    /// recycle the descriptor back onto the avail ring so the device can
    /// reuse it, and notify the device once if anything was recycled.
    /// Returns the decoded `EV_KEY` events, oldest first.
    fn drain(&mut self, device_handle: DeviceHandle, notify_mmio: Option<&MmioRegion>, notify_off: usize, notify_off_multiplier: u32) -> Vec<RawInputEvent> {
        let used_idx = self.ring.read::<u16>(self.layout.used_off + 2);
        let mut decoded = Vec::new();
        let mut recycled = false;

        while self.last_used_idx != used_idx {
            let ring_index = self.last_used_idx % self.layout.qsize;
            let used_entry_off = self.layout.used_ring_off(ring_index);
            let desc_index = self.ring.read::<u32>(used_entry_off) as u16;

            let event_off = self.layout.event_buf_off(desc_index);
            let event_type = self.ring.read::<u16>(event_off);
            let code = self.ring.read::<u16>(event_off + 2);
            let value = self.ring.read::<u32>(event_off + 4);
            if event_type == EV_KEY {
                decoded.push(RawInputEvent { event_type, code, value });
            }

            // Recycle: the buffer at `desc_index` is free again (the
            // descriptor table entry itself doesn't need rewriting — it
            // still points at the same buffer with the same
            // `VIRTQ_DESC_F_WRITE` flags from `setup_eventq`), so just
            // republish it on the avail ring.
            let avail_slot = self.next_avail_idx % self.layout.qsize;
            self.ring.write::<u16>(self.layout.avail_ring_off(avail_slot), desc_index);
            self.next_avail_idx = self.next_avail_idx.wrapping_add(1);
            self.ring.write::<u16>(self.layout.avail_off + 2, self.next_avail_idx);
            recycled = true;

            self.last_used_idx = self.last_used_idx.wrapping_add(1);
        }

        if recycled {
            self.kick(device_handle, notify_mmio, notify_off, notify_off_multiplier);
        }
        decoded
    }

    /// Ring the device's doorbell for this queue (virtio-v1.1 4.1.4.4): a
    /// 16-bit little-endian write of the queue index to the notify
    /// capability's BAR at `offset + queue_notify_off *
    /// notify_off_multiplier`.
    fn kick(&self, _device_handle: DeviceHandle, notify_mmio: Option<&MmioRegion>, notify_off: usize, notify_off_multiplier: u32) {
        // `notify_mmio` is `None` when the notify capability shares the
        // same BAR as common config (see `init_device`) — in that case the
        // common-config `MmioRegion` itself covers the notify offset too,
        // and `self.ring`/`notify_mmio` being distinct regions doesn't
        // apply; the caller (`poll_device`) always passes the right region
        // for this queue's notify capability.
        if let Some(notify) = notify_mmio {
            let off = notify_off + self.queue_notify_off as usize * notify_off_multiplier as usize;
            notify.write::<u16>(off, EVENTQ_INDEX);
        }
    }
}

/// A client `Read` request that couldn't be satisfied immediately because no
/// key events were buffered yet. Completed the next time `poll_device`
/// drains new events from the virtio ring — this is what gives
/// `file::read` (used by `keyboard_test`) real blocking semantics despite
/// every provider round trip being a single request/response pair: the
/// response is simply deferred until there's real data to send.
struct PendingRead {
    request_id: u64,
    /// Requested length, capped the same way the kernel caps scheme-proxy
    /// reads (`panda_abi::scheme_protocol::MAX_TRANSFER_SIZE`).
    len: usize,
}

/// State for the one virtio-keyboard device this driver currently owns.
/// Torn down (implicitly, by dropping) on `EVENT_DEVICE_REMOVED`.
struct DeviceState {
    /// Held (rather than dropped immediately) because closing it early
    /// would release every MMIO mapping/DMA allocation/IRQ subscription
    /// this driver is supposed to be holding (see
    /// `DeviceResource::teardown`).
    device_handle: DeviceHandle,
    /// Kept alive so the common-config mapping isn't unmapped early; its
    /// registers aren't touched again after `init_device` (this driver
    /// doesn't reset the device on removal — dropping `device_handle` tears
    /// the whole claim down, registers included).
    #[allow(dead_code)]
    common_mmio: MmioRegion,
    /// `None` when the notify capability shares `common_mmio`'s BAR (see
    /// `init_device`) — `notify_off` is then an offset into `common_mmio`
    /// instead.
    notify_mmio: Option<MmioRegion>,
    notify_off: usize,
    notify_off_multiplier: u32,
    queue: EventQueue,
    events: Vec<RawInputEvent>,
}

impl DeviceState {
    /// Drain the virtqueue's used ring into `self.events`, dropping the
    /// oldest buffered event past `EVENT_BUFFER_CAPACITY` (matching the
    /// deleted kernel driver's ring buffer behaviour). Returns whether any
    /// new events were buffered.
    fn poll_ring(&mut self) -> bool {
        let notify_region_is_common = self.notify_mmio.is_none();
        let (queue, notify_mmio, notify_off, notify_off_multiplier) = (
            &mut self.queue,
            if notify_region_is_common { Some(&self.common_mmio) } else { self.notify_mmio.as_ref() },
            self.notify_off,
            self.notify_off_multiplier,
        );
        let decoded = queue.drain(self.device_handle, notify_mmio, notify_off, notify_off_multiplier);
        if decoded.is_empty() {
            return false;
        }
        for event in decoded {
            if self.events.len() >= EVENT_BUFFER_CAPACITY {
                self.events.remove(0);
            }
            self.events.push(event);
        }
        true
    }

    /// Encode buffered events (oldest first) as packed `RawInputEvent`
    /// records, matching the wire format `keyboard_test`/
    /// `mailbox_keyboard_test` expect from `file::read`/`file::try_read`.
    fn drain_encoded(&mut self, max_bytes: usize) -> alloc::vec::Vec<u8> {
        let max_events = max_bytes / core::mem::size_of::<RawInputEvent>();
        let take = self.events.len().min(max_events);
        let mut out = alloc::vec::Vec::with_capacity(take * core::mem::size_of::<RawInputEvent>());
        for event in self.events.drain(..take) {
            // Safety: RawInputEvent is #[repr(C)], POD, 8 bytes.
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    &event as *const RawInputEvent as *const u8,
                    core::mem::size_of::<RawInputEvent>(),
                )
            };
            out.extend_from_slice(bytes);
        }
        out
    }
}

/// Resource id this driver hands out from `Open` — there is exactly one
/// device/resource per running instance of this driver (see
/// `serve_provider_requests`), so it's a fixed constant rather than a
/// minted counter.
const KEYBOARD_RESOURCE_ID: u64 = 1;

libpanda::main! {
    environment::log("virtio-keyboard: starting");

    let mailbox = Mailbox::default();

    // Step 2 of the driver lifecycle: subscribe to PCI add/remove events for
    // exactly this vendor/device/class triple. `vendor_id`/`device_id` alone
    // are *not* enough to identify this device: virtio-input covers both
    // keyboards and mice (and other HID types), and QEMU's virtio-mouse-pci
    // and virtio-keyboard-pci both present the identical PCI vendor:device
    // pair `0x1AF4:0x1052` — they're only distinguishable by PCI class code
    // (class 0x09 "input device controller", subclass 0x00 = keyboard vs.
    // 0x02 = mouse). Without the class filter below, this driver's
    // subscription matched *both* devices, and `handle_device_added`
    // claimed whichever one replayed first — which happened to be the
    // mouse, so every virtqueue/capability/notify computation in this file
    // was correct but wired to a device that never receives keyboard input
    // at all. Found by tracing QEMU's own `msix_write_config`/PCI address
    // debug output and noticing the claimed device's PCI address (00:02.0)
    // didn't match `info pci`'s keyboard entry (00:03.0). See
    // `plans/device-driver-model.md`'s Phase 7 landed-deviations note.
    const PCI_CLASS_INPUT_KEYBOARD: u32 = 0x0900; // class 0x09, subclass 0x00
    let match_entry = PciDeviceId {
        vendor_id: VIRTIO_VENDOR_ID,
        device_id: VIRTIO_INPUT_DEVICE_ID,
        class: PCI_CLASS_INPUT_KEYBOARD,
        class_mask: 0xFFFF,
    };
    let match_bytes = unsafe {
        core::slice::from_raw_parts(
            &match_entry as *const PciDeviceId as *const u8,
            core::mem::size_of::<PciDeviceId>(),
        )
    };
    let Ok(_subscription) = device::device_subscribe(BusType::Pci, match_bytes, mailbox.handle())
    else {
        environment::log("virtio-keyboard: device_subscribe failed");
        return 1;
    };
    environment::log("virtio-keyboard: subscribed, waiting for device");

    let mut device: Option<DeviceState> = None;
    let mut provider: Option<SchemeProvider> = None;
    let mut pending_reads: Vec<PendingRead> = Vec::new();

    // Held for the driver's whole lifetime (not just to send "ready" once):
    // also doubles as the shutdown-request channel — see `SHUTDOWN_REQUEST`
    // below. `None` when spawned with no parent channel (shouldn't happen —
    // every process is spawned by something — but handled rather than
    // assumed).
    let parent = libpanda::ipc::Channel::parent();

    loop {
        // Service any queued client requests first so a provider that just
        // registered doesn't wait a full mailbox cycle to see its first
        // request.
        if let Some(p) = provider.as_ref() {
            serve_provider_requests(p, device.as_mut(), &mut pending_reads);
        }

        // Cooperative shutdown: this driver otherwise loops forever by
        // design (matching how it runs in the real boot path, spawned once
        // by `init` and never expected to exit). A caller that spawned it
        // directly — e.g. `userspace/tests/keyboard_test`, which acts as
        // its own `init` and has no service manager to eventually reap
        // drivers — has no other way to make it exit: there is no
        // implemented `OP_PROCESS_SIGNAL`-based kill in this kernel (see
        // `panda-kernel/src/resource/spawn_handle.rs::SpawnHandle::signal`'s
        // doc comment for why forcibly killing a process holding an active
        // `DeviceResource` from *outside* its own context isn't safe), so
        // shutdown has to be a request this process observes and acts on
        // itself, exiting normally through the same `OP_PROCESS_EXIT` path
        // every other process uses. See `plans/device-driver-model.md`'s
        // Phase 7 landed-deviations note.
        if let Some(parent) = parent.as_ref() {
            let mut shutdown_buf = [0u8; 16];
            if let Ok(Some(n)) = parent.try_recv(&mut shutdown_buf)
                && &shutdown_buf[..n] == SHUTDOWN_REQUEST
            {
                environment::log("virtio-keyboard: shutdown requested, exiting");
                return 0;
            }
        }

        let Some((handle, events)) = mailbox.try_recv() else {
            // Nothing pending on the mailbox right now. Yield briefly rather
            // than spinning hot — there is no generic "attach an arbitrary
            // channel handle to a mailbox after creation" syscall in this
            // codebase (mailbox attachment happens at resource-creation time
            // via a `mailbox` parameter, e.g. `device_subscribe`'s 4th arg),
            // so the scheme provider's channel and the device subscription
            // can't share one blocking `mailbox.recv()` wait; this loop polls
            // both instead.
            process::yield_now();
            continue;
        };

        let raw_events = events.raw();
        if raw_events & EVENT_DEVICE_ADDED != 0 {
            handle_device_added(handle, &mut device);
            if device.is_some() && provider.is_none() {
                provider = register_scheme();
                if provider.is_some() {
                    // Best-effort readiness signal for a test harness (or
                    // any other caller) that spawned this driver directly
                    // and wants to know when `keyboard:` is actually
                    // servable rather than racing `environment::open`
                    // against this driver's own async startup — see
                    // `userspace/tests/keyboard_test`. Spawned by `init` in
                    // the real boot path, this is simply never read by
                    // anyone and harmlessly sits in the channel buffer.
                    if let Some(parent) = parent.as_ref() {
                        let _ = parent.send(b"ready");
                    }
                }
            }
        }
        if raw_events & EVENT_DEVICE_REMOVED != 0 {
            handle_device_removed(&mut device, &mut provider, &mut pending_reads);
        }
        if raw_events & EVENT_DEVICE_IRQ != 0 {
            poll_device(device.as_mut(), &mut pending_reads, provider.as_ref());
        }
    }
}

/// Message a parent process sends on the spawn channel to ask this driver
/// to exit — see the shutdown-request handling in the main loop above.
const SHUTDOWN_REQUEST: &[u8] = b"shutdown";

/// Step 5-10: claim the newly-arrived device, walk its virtio capabilities,
/// map its config BARs, negotiate virtio features, set up the eventq, and
/// subscribe to its IRQ.
fn handle_device_added(subscription_handle: Handle, device: &mut Option<DeviceState>) {
    if device.is_some() {
        // Already own a device; a second ADDED before the first is removed
        // shouldn't happen for an exact (non-wildcard) subscription, but
        // don't clobber the existing claim if it does.
        return;
    }

    // The mailbox event bit only wakes us up; the actual `DeviceEvent`
    // (bus type, identity, and the claim token `device_claim` needs) is
    // read back with `device_poll_event` — see `libpanda::device`'s doc
    // comment on `OP_DEVICE_POLL_EVENT`. The queue may (in principle)
    // contain more than one pending event if several devices matched
    // before we got scheduled; drain until we find one we can claim or the
    // queue is empty.
    loop {
        let event = match device::device_poll_event(subscription_handle) {
            Ok(Some(event)) => event,
            Ok(None) => return,
            Err(err) => {
                log_error("virtio-keyboard: poll_event failed", err);
                return;
            }
        };

        if event.token == 0 {
            // A removal notification interleaved with adds; nothing to
            // claim.
            continue;
        }

        match init_device(event.token.into()) {
            Ok(state) => {
                environment::log("virtio-keyboard: device claimed and initialised");
                *device = Some(state);
                return;
            }
            Err(err) => {
                log_error("virtio-keyboard: device init failed", err);
                return;
            }
        }
    }
}

/// Step 13: tear down our claim on device removal and stop serving clients.
fn handle_device_removed(
    device: &mut Option<DeviceState>,
    provider: &mut Option<SchemeProvider>,
    pending_reads: &mut Vec<PendingRead>,
) {
    if device.is_none() {
        return;
    }
    environment::log("virtio-keyboard: device removed");

    // Fail any reads a client is still waiting on rather than leaving them
    // hanging forever.
    if let Some(p) = provider.as_ref() {
        for pending in pending_reads.drain(..) {
            let _ = reply_read_err(p, pending.request_id, ErrorCode::IoError);
        }
    }

    // Unregister the scheme so `keyboard:` opens fail cleanly instead of
    // hanging on a provider that will never answer, until the device
    // reappears.
    *provider = None;

    // Closing the device handle (dropping `DeviceState`) triggers the
    // kernel's single cleanup action for MMIO/DMA/IRQ resources — see
    // `plans/device-driver-model.md`'s "Driver lifecycle" step 13.
    *device = None;
}

/// Step 12 (IRQ half): drain the virtio eventq's used ring, translate
/// `EV_KEY` events, and either buffer them or immediately satisfy a client
/// `Read` that's already waiting. Also pushes a `Response::Notify` to any
/// mailbox a client attached at `Open` time (see `serve_provider_requests`'
/// `Request::Open` handling and `SchemeProxyResource::attach_mailbox`),
/// giving `mailbox_keyboard_test`'s notify-then-read pattern real events to
/// wait on instead of hanging forever.
fn poll_device(
    device: Option<&mut DeviceState>,
    pending_reads: &mut Vec<PendingRead>,
    provider: Option<&SchemeProvider>,
) {
    let Some(device) = device else { return };

    let got_new_events = device.poll_ring();

    if let Some(provider) = provider {
        while !device.events.is_empty() {
            let Some(pending) = pending_reads.first() else {
                break;
            };
            let data = device.drain_encoded(pending.len);
            if data.is_empty() {
                break;
            }
            let pending = pending_reads.remove(0);
            let _ = reply_read_ok(provider, pending.request_id, &data);
        }

        if got_new_events {
            let _ = provider.notify(KEYBOARD_RESOURCE_ID, panda_abi::EVENT_KEYBOARD_KEY);
        }
    }
}

/// Step 11: register the `keyboard:` scheme via the real, current
/// `OP_SCHEME_REGISTER` API (`libpanda::scheme::SchemeProvider::register`,
/// found by grepping `OP_SCHEME_REGISTER` — see the module's naming
/// re-scope note in `plans/device-driver-model.md`). Called only after
/// `init_device` succeeds, so this driver never advertises a scheme it
/// can't actually serve.
fn register_scheme() -> Option<SchemeProvider> {
    match SchemeProvider::register("keyboard") {
        Ok(provider) => {
            environment::log("virtio-keyboard: registered keyboard: scheme");
            Some(provider)
        }
        Err(err) => {
            log_error("virtio-keyboard: scheme registration failed", err);
            None
        }
    }
}

/// Drain and answer every request currently queued on the provider channel.
/// `Open` grants any path (this driver serves exactly one device); `Read`
/// completes immediately if events are buffered, or is queued in
/// `pending_reads` to be completed later by `poll_device` (see
/// `PendingRead`'s doc comment).
fn serve_provider_requests(
    provider: &SchemeProvider,
    mut device: Option<&mut DeviceState>,
    pending_reads: &mut Vec<PendingRead>,
) {
    let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];
    loop {
        let request = match provider.try_recv(&mut buf) {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(_) => return,
        };

        match request {
            Request::Open { request_id, .. } => {
                // A single resource id: this driver serves one device via
                // one scheme, so every open path resolves to the same
                // resource (matching the paths `keyboard_test` /
                // `mailbox_keyboard_test` use, e.g. "/pci/00:03.0").
                let _ = provider.reply_open_ok(request_id, KEYBOARD_RESOURCE_ID);
            }
            Request::Readdir { request_id, .. } => {
                let _ = provider.reply_readdir_err(request_id, ErrorCode::NotSupported);
            }
            Request::Read { request_id, len, nonblocking, .. } => {
                let len = (len as usize).min(panda_abi::scheme_protocol::MAX_TRANSFER_SIZE);
                let data = device.as_deref_mut().map(|d| d.drain_encoded(len));
                match data {
                    Some(data) if !data.is_empty() => {
                        let _ = reply_read_ok(provider, request_id, &data);
                    }
                    _ if nonblocking => {
                        // Non-blocking (`file::try_read`): no events
                        // buffered, but the caller asked not to wait —
                        // answer immediately with zero bytes rather than
                        // deferring like the blocking case below.
                        // `OP_FILE_READ`'s scheme-proxy path always awaits
                        // one full round trip either way (see
                        // `SchemeProxyResource::read`'s doc comment); the
                        // difference non-blocking makes is here, on the
                        // provider side, not in the kernel's wait behaviour.
                        let _ = reply_read_ok(provider, request_id, &[]);
                    }
                    _ => {
                        // Blocking (`file::read`): no events buffered yet —
                        // defer the reply until `poll_device` has something
                        // for us, giving this request real blocking
                        // semantics (see `PendingRead`'s doc comment).
                        pending_reads.push(PendingRead { request_id, len });
                    }
                }
            }
            Request::Write { request_id, .. } => {
                let _ = provider.reply_write_err(request_id, ErrorCode::NotWritable);
            }
            Request::Close { request_id, .. } => {
                // No per-open state to release beyond the shared device
                // handle, which is owned by `DeviceState` and released on
                // `EVENT_DEVICE_REMOVED`.
                let _ = provider.reply_close_ok(request_id);
            }
            Request::Connect { request_id, .. } => {
                let _ = provider.reply_connect_err(request_id, ErrorCode::NotSupported);
            }
        }
    }
}

fn reply_read_ok(provider: &SchemeProvider, request_id: u64, data: &[u8]) -> Result<()> {
    provider.reply_read_ok(request_id, data)
}

fn reply_read_err(provider: &SchemeProvider, request_id: u64, error: ErrorCode) -> Result<()> {
    provider.reply_read_err(request_id, error)
}

fn log_error(prefix: &str, _err: ErrorCode) {
    // `environment::log` takes a plain `&str` (see `libpanda::environment`),
    // so the specific `ErrorCode` isn't interpolated here — logging is
    // best-effort diagnostics, not a user-facing error message.
    environment::log(prefix);
}

// Referenced only to keep `DeviceEvent`/`Response` imports intentional and
// documented rather than silently unused.
#[allow(dead_code)]
fn _assert_types(_: DeviceEvent, _: Response) {}
