//! Userspace virtio-blk driver (M4 Phase 2 of `plans/device-driver-model.md`).
//!
//! Replaces the deleted kernel-resident driver
//! (`panda-kernel/src/devices/virtio_block/`). Follows the same lifecycle as
//! `userspace/drivers/virtio-keyboard` (Phase 7): subscribe to PCI device
//! events, claim the device on `EVENT_DEVICE_ADDED`, walk its virtio PCI
//! capability list, map the common-config/notify/device-config BARs,
//! negotiate virtio features, set up the requestq split virtqueue, subscribe
//! to its IRQ, register the `block:` scheme, and serve clients until
//! `EVENT_DEVICE_REMOVED`.
//!
//! # Virtio transport: hand-rolled, not `virtio_drivers`
//!
//! Same rationale and mechanics as `virtio-keyboard`'s module doc comment
//! (PCI capability walk via `OP_DEVICE_PCI_CONFIG_READ`, split virtqueue
//! laid out by hand in a DMA buffer) — see that file for the full
//! explanation. The one structural difference: virtio-blk's **requestq** is
//! a request/response queue (the driver fills in a request header + data
//! buffer, the device fills in a status byte), not the eventq's
//! device-fills-everything pattern, so each request's descriptor chain is
//! built (and its buffers filled/read) by this driver rather than being
//! fixed at setup time.
//!
//! # Single-in-flight-request model
//!
//! Every client of `block:` (the kernel's `resource::block_client::SchemeBlockDevice`
//! adapter used by `vfs::mount_ext2`, and a raw process opening `block:/...`
//! directly) issues one `Read`/`Write` round trip at a time and awaits its
//! reply before issuing the next — there is no pipelining anywhere in this
//! codebase's block I/O path today. This driver takes advantage of that: it
//! only ever has one virtio request outstanding, reusing a single scratch
//! DMA buffer rather than tracking a pool of in-flight requests (unlike the
//! deleted kernel driver's `futures.rs`, which supported real concurrent
//! I/O). If a second `Read`/`Write` somehow arrived while one was still
//! pending, it would be rejected with `IoError` rather than queued — noted
//! as follow-up debt in `plans/device-driver-model.md` alongside the other
//! M4 landed-deviations.
//!
//! # Sector alignment and read-modify-write
//!
//! `Request::Read`/`Write`'s `offset`/`len` are arbitrary byte ranges (ext2
//! and `block_test` both issue sub-sector-aligned writes), but virtio-blk
//! only transfers whole 512-byte sectors. Reads always fetch every sector
//! spanning `[offset, offset+len)` and slice out the requested bytes. Writes
//! that are already sector-aligned (`offset % 512 == 0 && len % 512 == 0`)
//! go straight to the device; anything else does a read-modify-write (read
//! the spanning sectors, patch in the caller's bytes, write the whole range
//! back) — two sequential virtio requests, mirroring the deleted kernel
//! driver's `VirtioBlockDevice::write_at` slow path.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use libpanda::device::{
    self, BusType, DeviceEvent, EVENT_DEVICE_ADDED, EVENT_DEVICE_IRQ, EVENT_DEVICE_REMOVED,
    MmioRegion, PciDeviceId,
};
use libpanda::error::Result;
use libpanda::mailbox::Mailbox;
use libpanda::scheme::SchemeProvider;
use libpanda::{Handle, environment, process};

type DeviceHandle = Handle;
use panda_abi::ErrorCode;
use panda_abi::scheme_protocol::{Request, Response};

// virtio-blk's PCI vendor/device IDs (matches the deleted kernel driver's
// `panda-kernel/src/devices.rs` match arm: `(0x1AF4, 0x1001, _) | (0x1AF4,
// 0x1042, _)`). 0x1001 is the legacy/transitional virtio-blk ID QEMU's
// default `virtio-blk-pci` device presents as; 0x1042 is the modern-only ID
// (used when `disable-legacy=on`). Unlike virtio-input, there is no other
// virtio device sharing either ID, so no PCI class filter is needed to
// disambiguate (contrast `virtio-keyboard`'s keyboard-vs-mouse class
// filter).
const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_BLK_DEVICE_ID_LEGACY: u16 = 0x1001;
const VIRTIO_BLK_DEVICE_ID_MODERN: u16 = 0x1042;

libpanda::pci_device_table![
    { vendor: VIRTIO_VENDOR_ID, device: VIRTIO_BLK_DEVICE_ID_LEGACY },
    { vendor: VIRTIO_VENDOR_ID, device: VIRTIO_BLK_DEVICE_ID_MODERN },
];

const SECTOR_SIZE: u64 = 512;

// =============================================================================
// PCI config space / virtio-pci capabilities — identical mechanics to
// `virtio-keyboard`'s module of the same name; see its doc comments for the
// virtio-v1.1 spec references. Duplicated rather than factored into a shared
// crate — see the module doc comment's note on follow-up debt.
// =============================================================================

const PCI_STATUS_OFFSET: u32 = 0x06;
const PCI_STATUS_CAP_LIST: u32 = 1 << 4;
const PCI_CAPABILITIES_POINTER_OFFSET: u32 = 0x34;
const PCI_CAP_ID_VNDR: u8 = 0x09;

const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

#[derive(Clone, Copy)]
struct VirtioCap {
    bar: u8,
    offset: u32,
    #[allow(dead_code)]
    length: u32,
    notify_off_multiplier: u32,
}

/// Walk `device_handle`'s PCI capability list for the common-config,
/// notify, and device-specific-config capabilities. The device-config
/// capability (cfg_type 4) is new relative to `virtio-keyboard`'s version of
/// this function: virtio-blk's `capacity` field lives there (virtio-v1.1
/// 5.2.4's `struct virtio_blk_config`), not in the common config.
fn find_virtio_caps(device_handle: DeviceHandle) -> Result<(VirtioCap, VirtioCap, VirtioCap)> {
    let status = device::device_pci_config_read(device_handle, PCI_STATUS_OFFSET, 2)?;
    if status & PCI_STATUS_CAP_LIST == 0 {
        return Err(ErrorCode::NotSupported);
    }

    let mut common: Option<VirtioCap> = None;
    let mut notify: Option<VirtioCap> = None;
    let mut device_cfg: Option<VirtioCap> = None;

    let mut cap_ptr =
        (device::device_pci_config_read(device_handle, PCI_CAPABILITIES_POINTER_OFFSET, 1)? as u8) & 0xFC;
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
                VIRTIO_PCI_CAP_DEVICE_CFG => {
                    device_cfg = Some(VirtioCap { bar, offset, length, notify_off_multiplier: 0 });
                }
                _ => {}
            }
        }

        cap_ptr = next & 0xFC;
    }

    match (common, notify, device_cfg) {
        (Some(common), Some(notify), Some(device_cfg)) => Ok((common, notify, device_cfg)),
        _ => Err(ErrorCode::NotSupported),
    }
}

// =============================================================================
// Virtio device status/feature negotiation — identical to virtio-keyboard's.
// =============================================================================

const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
const VIRTIO_F_VERSION_1_HIGH_BIT: u32 = 1 << 0;

mod common_cfg {
    pub const DEVICE_FEATURE_SELECT: usize = 0;
    pub const DEVICE_FEATURE: usize = 4;
    pub const DRIVER_FEATURE_SELECT: usize = 8;
    pub const DRIVER_FEATURE: usize = 12;
    pub const DEVICE_STATUS: usize = 20;
    pub const QUEUE_SELECT: usize = 22;
    pub const QUEUE_SIZE: usize = 24;
    pub const QUEUE_MSIX_VECTOR: usize = 26;
    pub const QUEUE_ENABLE: usize = 28;
    pub const QUEUE_NOTIFY_OFF: usize = 30;
    pub const QUEUE_DESC: usize = 32;
    pub const QUEUE_DRIVER: usize = 40;
    pub const QUEUE_DEVICE: usize = 48;
}

/// virtio-blk has exactly one queue (its requestq) unless `VIRTIO_BLK_F_MQ`
/// is negotiated (it isn't, here).
const REQUESTQ_INDEX: u16 = 0;
const MSIX_VECTOR_0: u16 = 0;

fn negotiate_features(common: &MmioRegion, common_off: usize) -> Result<()> {
    common.write::<u8>(common_off + common_cfg::DEVICE_STATUS, 0);
    common.write::<u8>(common_off + common_cfg::DEVICE_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
    common.write::<u8>(
        common_off + common_cfg::DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    common.write::<u32>(common_off + common_cfg::DEVICE_FEATURE_SELECT, 1);
    let device_feature_high = common.read::<u32>(common_off + common_cfg::DEVICE_FEATURE);
    if device_feature_high & VIRTIO_F_VERSION_1_HIGH_BIT == 0 {
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
// Split virtqueue: descriptor table, avail ring, used ring, plus a scratch
// buffer for the single in-flight request's header/data/status (see the
// module doc comment's "single-in-flight-request model").
// =============================================================================

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const DESC_ENTRY_LEN: usize = 16;
const USED_ENTRY_LEN: usize = 8;

/// Requestq size: only 3 descriptors are ever used at once (header, data,
/// status — see the module doc comment), but virtio queue sizes must be a
/// power of two. Clamped to the device's own advertised maximum in
/// `setup_requestq`, which also writes this chosen size back to the
/// device's `queue_size` register — see that write's doc comment for a real
/// bug (out-of-bounds avail-ring reads once `next_avail_idx` wrapped) this
/// driver had until that write was added, root-caused via
/// `ext2_create_test`/`ext2_mkdir_test` reproducibly failing at the exact
/// wraparound point.
const REQUESTQ_SIZE_WANTED: u16 = 8;

/// Largest single virtio request's data payload this driver will build, in
/// sectors: covers `panda_abi::scheme_protocol::MAX_TRANSFER_SIZE` (the
/// largest `Read`/`Write` the wire protocol allows in one frame) plus up to
/// one extra sector of read-modify-write slop on each end. `4000 + 511 +
/// 511 = 5022` bytes -> 10 sectors; rounded up generously to 16 (8 KiB).
const MAX_DATA_SECTORS: u64 = 16;
const MAX_DATA_LEN: usize = (MAX_DATA_SECTORS * SECTOR_SIZE) as usize;

/// `struct virtio_blk_req` header length (virtio-v1.1 5.2.6.2): type:u32 +
/// reserved:u32 + sector:u64.
const BLK_HEADER_LEN: usize = 16;
const BLK_STATUS_LEN: usize = 1;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_S_OK: u8 = 0;

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// Byte layout of the requestq's split virtqueue plus its scratch
/// header/data/status buffers, all packed into one DMA allocation.
struct QueueLayout {
    qsize: u16,
    desc_off: usize,
    avail_off: usize,
    used_off: usize,
    header_off: usize,
    data_off: usize,
    status_off: usize,
    total_len: usize,
}

impl QueueLayout {
    fn compute(qsize: u16) -> Self {
        let qsize_usize = qsize as usize;
        let desc_off = 0;
        let avail_off = desc_off + DESC_ENTRY_LEN * qsize_usize;
        let avail_len = 4 + 2 * qsize_usize;
        let used_off = align_up(avail_off + avail_len, 4);
        let used_len = 4 + USED_ENTRY_LEN * qsize_usize;
        let header_off = align_up(used_off + used_len, 8);
        let data_off = align_up(header_off + BLK_HEADER_LEN, 8);
        let status_off = data_off + MAX_DATA_LEN;
        let total_len = status_off + BLK_STATUS_LEN;
        Self {
            qsize,
            desc_off,
            avail_off,
            used_off,
            header_off,
            data_off,
            status_off,
            total_len,
        }
    }

    fn desc_addr_off(&self, i: u16) -> usize {
        self.desc_off + DESC_ENTRY_LEN * i as usize
    }

    fn avail_ring_off(&self, i: u16) -> usize {
        self.avail_off + 4 + 2 * i as usize
    }

    /// Kept for symmetry with `virtio-keyboard`'s identical helper and for
    /// documentation of the used-ring layout, even though this driver's
    /// `poll_completed` only needs the used ring's *count*
    /// (`used_off + 2`) — with a single request always at descriptor head 0
    /// (see `RequestQueue::submit`), there is no per-entry descriptor index
    /// to look up.
    #[allow(dead_code)]
    fn used_ring_off(&self, i: u16) -> usize {
        self.used_off + 4 + USED_ENTRY_LEN * i as usize
    }
}

fn init_device(token: Handle) -> Result<DeviceState> {
    let device_handle: DeviceHandle = device::device_claim(token)?;

    let (common_cap, notify_cap, device_cfg_cap) = find_virtio_caps(device_handle)?;

    let common_mmio = device::device_map_mmio(device_handle, common_cap.bar as u32)?;
    let notify_mmio = if notify_cap.bar == common_cap.bar {
        None
    } else {
        Some(device::device_map_mmio(device_handle, notify_cap.bar as u32)?)
    };
    let device_cfg_mmio = if device_cfg_cap.bar == common_cap.bar {
        None
    } else if notify_cap.bar == device_cfg_cap.bar {
        None
    } else {
        Some(device::device_map_mmio(device_handle, device_cfg_cap.bar as u32)?)
    };

    negotiate_features(&common_mmio, common_cap.offset as usize)?;

    let mailbox = Mailbox::default();
    device::device_subscribe_irq(device_handle, mailbox.handle())?;

    let queue = setup_requestq(device_handle, &common_mmio, common_cap.offset as usize)?;

    // Kick before DRIVER_OK for the same reason documented at length in
    // `virtio-keyboard::init_device` — but there is nothing to kick yet
    // (the requestq starts empty; the eventq's initial-kick requirement was
    // specific to it pre-populating every descriptor as available at setup
    // time, which a request/response queue doesn't do). DRIVER_OK is set
    // immediately after the queue is enabled instead.
    common_mmio.write::<u8>(
        common_cap.offset as usize + common_cfg::DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Read `capacity` (virtio-v1.1 5.2.4's `struct virtio_blk_config`,
    // first field, sectors) from whichever region the device-config
    // capability maps to.
    let device_cfg_region = device_cfg_mmio.as_ref().unwrap_or(&common_mmio);
    let device_cfg_off = if device_cfg_mmio.is_some() { 0 } else { device_cfg_cap.offset as usize };
    let capacity_sectors = device_cfg_region.read::<u64>(device_cfg_off);

    Ok(DeviceState {
        device_handle,
        common_mmio,
        notify_mmio,
        device_cfg_mmio,
        notify_off: notify_cap.offset as usize,
        notify_off_multiplier: notify_cap.notify_off_multiplier,
        capacity_bytes: capacity_sectors * SECTOR_SIZE,
        queue,
        pending: None,
    })
}

fn setup_requestq(device_handle: DeviceHandle, common: &MmioRegion, common_off: usize) -> Result<RequestQueue> {
    common.write::<u16>(common_off + common_cfg::QUEUE_SELECT, REQUESTQ_INDEX);
    let max_size = common.read::<u16>(common_off + common_cfg::QUEUE_SIZE);
    if max_size == 0 {
        return Err(ErrorCode::NotSupported);
    }
    let mut qsize = REQUESTQ_SIZE_WANTED.min(max_size);
    qsize = 1u16 << (15 - qsize.leading_zeros() as u16).min(15);
    if qsize < 3 {
        // Need at least 3 descriptors (header/data/status) per request.
        return Err(ErrorCode::NotSupported);
    }
    // Write the (possibly smaller than the device's max) chosen size back —
    // virtio-v1.1 4.1.4.3.2 requires this whenever the driver picks a size
    // other than the device's advertised maximum: the device uses this
    // value (not its own max) to compute `avail.idx`/`used.idx` modulo the
    // ring size internally. Omitting this write was a real bug found via
    // `ext2_create_test`/`ext2_mkdir_test`: with a clamped `qsize` (e.g. 64)
    // smaller than the device's actual max (QEMU's virtio-blk-pci default
    // is 256), the device kept computing ring positions modulo its own
    // larger max, so once `next_avail_idx` exceeded this driver's `qsize`
    // (but not the device's), the device read out-of-bounds past the end of
    // this driver's smaller avail-ring DMA allocation — surfacing as QEMU
    // logging "virtio-blk missing headers" and the request hanging forever,
    // reproducibly at request #65 with `qsize=64` (and at #9 with the
    // originally-tried `qsize=8` — see `REQUESTQ_SIZE_WANTED`'s doc comment,
    // now superseded by this fix; the real bug was always this missing
    // write, not queue size itself).
    common.write::<u16>(common_off + common_cfg::QUEUE_SIZE, qsize);

    let layout = QueueLayout::compute(qsize);
    let (virt_addr, iova) = device::dma_alloc(device_handle, layout.total_len)?;
    // Safety: `dma_alloc` just mapped `layout.total_len` bytes of
    // DMA-visible memory at `virt_addr`, exclusively for this queue's use.
    let ring = unsafe { MmioRegion::new(virt_addr as *mut u8, layout.total_len) };

    ring.write::<u16>(layout.avail_off, 0);
    ring.write::<u16>(layout.avail_off + 2, 0);
    ring.write::<u16>(layout.used_off, 0);
    ring.write::<u16>(layout.used_off + 2, 0);

    let desc_iova = iova + layout.desc_off as u64;
    let avail_iova = iova + layout.avail_off as u64;
    let used_iova = iova + layout.used_off as u64;
    common.write::<u64>(common_off + common_cfg::QUEUE_DESC, desc_iova);
    common.write::<u64>(common_off + common_cfg::QUEUE_DRIVER, avail_iova);
    common.write::<u64>(common_off + common_cfg::QUEUE_DEVICE, used_iova);
    common.write::<u16>(common_off + common_cfg::QUEUE_MSIX_VECTOR, MSIX_VECTOR_0);
    let queue_notify_off = common.read::<u16>(common_off + common_cfg::QUEUE_NOTIFY_OFF);
    common.write::<u16>(common_off + common_cfg::QUEUE_ENABLE, 1);

    Ok(RequestQueue {
        ring,
        iova,
        _virt_addr: virt_addr,
        _dma_size: layout.total_len,
        layout,
        queue_notify_off,
        last_used_idx: 0,
        next_avail_idx: 0,
    })
}

struct RequestQueue {
    ring: MmioRegion,
    /// IOVA base of the whole DMA allocation — descriptor `addr` fields are
    /// offsets from this.
    iova: u64,
    #[allow(dead_code)]
    _virt_addr: usize,
    #[allow(dead_code)]
    _dma_size: usize,
    layout: QueueLayout,
    queue_notify_off: u16,
    last_used_idx: u16,
    next_avail_idx: u16,
}

impl RequestQueue {
    fn kick(&self, notify_mmio: Option<&MmioRegion>, common_mmio: &MmioRegion, notify_off: usize, notify_off_multiplier: u32) {
        let notify = notify_mmio.unwrap_or(common_mmio);
        let off = notify_off + self.queue_notify_off as usize * notify_off_multiplier as usize;
        notify.write::<u16>(off, REQUESTQ_INDEX);
    }

    /// Submit a 3-descriptor request (header/data/status) built from this
    /// queue's fixed scratch buffers (see `QueueLayout`) and publish it on
    /// the avail ring. `data_len` is the data descriptor's length; `is_read`
    /// selects whether the data descriptor is device-writable (IN) or
    /// device-readable (OUT).
    fn submit(&mut self, request_type: u32, sector: u64, data_len: usize, is_read: bool) {
        let l = &self.layout;

        // Header descriptor (index 0): device-readable, driver-filled.
        self.ring.write::<u32>(l.header_off, request_type);
        self.ring.write::<u32>(l.header_off + 4, 0); // reserved
        self.ring.write::<u64>(l.header_off + 8, sector);

        let desc0 = l.desc_addr_off(0);
        self.ring.write::<u64>(desc0, self.iova + l.header_off as u64);
        self.ring.write::<u32>(desc0 + 8, BLK_HEADER_LEN as u32);
        self.ring.write::<u16>(desc0 + 12, VIRTQ_DESC_F_NEXT);
        self.ring.write::<u16>(desc0 + 14, 1);

        let desc1 = l.desc_addr_off(1);
        self.ring.write::<u64>(desc1, self.iova + l.data_off as u64);
        self.ring.write::<u32>(desc1 + 8, data_len as u32);
        let data_flags = VIRTQ_DESC_F_NEXT | if is_read { VIRTQ_DESC_F_WRITE } else { 0 };
        self.ring.write::<u16>(desc1 + 12, data_flags);
        self.ring.write::<u16>(desc1 + 14, 2);

        // Status descriptor (index 2): device-writable.
        let desc2 = l.desc_addr_off(2);
        self.ring.write::<u64>(desc2, self.iova + l.status_off as u64);
        self.ring.write::<u32>(desc2 + 8, BLK_STATUS_LEN as u32);
        self.ring.write::<u16>(desc2 + 12, VIRTQ_DESC_F_WRITE);
        self.ring.write::<u16>(desc2 + 14, 0);

        let avail_slot = self.next_avail_idx % self.layout.qsize;
        self.ring.write::<u16>(self.layout.avail_ring_off(avail_slot), 0); // head descriptor index
        self.next_avail_idx = self.next_avail_idx.wrapping_add(1);
        self.ring.write::<u16>(self.layout.avail_off + 2, self.next_avail_idx);
    }

    /// Pointer-and-length into the scratch data buffer, for filling
    /// (writes) or reading back (reads) — always at the same fixed offset
    /// since only one request is ever in flight.
    fn data_region(&self) -> (&MmioRegion, usize) {
        (&self.ring, self.layout.data_off)
    }

    fn status_byte(&self) -> u8 {
        self.ring.read::<u8>(self.layout.status_off)
    }

    /// Returns `true` if a new used-ring entry appeared (the in-flight
    /// request completed).
    fn poll_completed(&mut self) -> bool {
        let used_idx = self.ring.read::<u16>(self.layout.used_off + 2);
        if self.last_used_idx == used_idx {
            return false;
        }
        self.last_used_idx = self.last_used_idx.wrapping_add(1);
        true
    }
}

/// What the single in-flight virtio request is for, and enough state to
/// finish handling it (and, for the write-after-read phase of an unaligned
/// write, to submit the follow-up request) once its completion IRQ arrives.
enum PendingKind {
    /// A `Read` request: slice `[offset_in_first, offset_in_first +
    /// want_len)` out of the completed data buffer and reply.
    Read { offset_in_first: usize, want_len: usize },
    /// A sector-aligned `Write`: the whole data buffer was the caller's
    /// data, so the reply is just `written = len`.
    WriteAligned { len: usize },
    /// Phase 1 (read) of an unaligned write's read-modify-write: once this
    /// read completes, patch `data` into the buffer at `offset_in_first`
    /// and submit the write phase.
    WriteReadPhase {
        start_sector: u64,
        num_sectors: u64,
        offset_in_first: usize,
        data: Vec<u8>,
    },
    /// Phase 2 (write) of a read-modify-write: once this completes, reply
    /// with `written = len`.
    WriteWritePhase { len: usize },
}

struct PendingIo {
    request_id: u64,
    kind: PendingKind,
}

struct DeviceState {
    /// Held (never read again) so the claim stays alive for this driver's
    /// lifetime — dropping it would release every MMIO mapping/DMA
    /// allocation/IRQ subscription (see `DeviceResource::teardown`, and
    /// `virtio-keyboard::DeviceState`'s identical field).
    #[allow(dead_code)]
    device_handle: DeviceHandle,
    #[allow(dead_code)]
    common_mmio: MmioRegion,
    notify_mmio: Option<MmioRegion>,
    #[allow(dead_code)]
    device_cfg_mmio: Option<MmioRegion>,
    notify_off: usize,
    notify_off_multiplier: u32,
    capacity_bytes: u64,
    queue: RequestQueue,
    pending: Option<PendingIo>,
}

impl DeviceState {
    fn kick(&self) {
        self.queue.kick(self.notify_mmio.as_ref(), &self.common_mmio, self.notify_off, self.notify_off_multiplier);
    }

    /// Begin a `Read` request: fetch every sector spanning `[offset, offset
    /// + len)`. Caller has already checked `self.pending.is_none()`.
    fn start_read(&mut self, request_id: u64, offset: u64, len: usize) {
        let start_sector = offset / SECTOR_SIZE;
        let offset_in_first = (offset % SECTOR_SIZE) as usize;
        let end = offset + len as u64;
        let end_sector = end.div_ceil(SECTOR_SIZE);
        let num_sectors = (end_sector - start_sector).max(1);

        self.queue.submit(
            VIRTIO_BLK_T_IN,
            start_sector,
            (num_sectors * SECTOR_SIZE) as usize,
            true,
        );
        self.pending = Some(PendingIo {
            request_id,
            kind: PendingKind::Read { offset_in_first, want_len: len },
        });
        self.kick();
    }

    /// Begin a `Write` request for `data` at `offset`. Caller has already
    /// checked `self.pending.is_none()` and that `offset < capacity_bytes`.
    fn start_write(&mut self, request_id: u64, offset: u64, data: &[u8]) {
        let aligned = offset % SECTOR_SIZE == 0 && data.len() as u64 % SECTOR_SIZE == 0;
        if aligned {
            let (ring, data_off) = self.queue.data_region();
            for (i, chunk) in data.chunks(8).enumerate() {
                let mut buf8 = [0u8; 8];
                buf8[..chunk.len()].copy_from_slice(chunk);
                ring.write::<u64>(data_off + i * 8, u64::from_le_bytes(buf8));
            }
            self.queue.submit(VIRTIO_BLK_T_OUT, offset / SECTOR_SIZE, data.len(), false);
            self.pending = Some(PendingIo {
                request_id,
                kind: PendingKind::WriteAligned { len: data.len() },
            });
            self.kick();
            return;
        }

        // Unaligned: read-modify-write. Phase 1 — read the spanning
        // sectors.
        let start_sector = offset / SECTOR_SIZE;
        let offset_in_first = (offset % SECTOR_SIZE) as usize;
        let end = offset + data.len() as u64;
        let end_sector = end.div_ceil(SECTOR_SIZE);
        let num_sectors = (end_sector - start_sector).max(1);

        self.queue.submit(
            VIRTIO_BLK_T_IN,
            start_sector,
            (num_sectors * SECTOR_SIZE) as usize,
            true,
        );
        self.pending = Some(PendingIo {
            request_id,
            kind: PendingKind::WriteReadPhase {
                start_sector,
                num_sectors,
                offset_in_first,
                data: data.to_vec(),
            },
        });
        self.kick();
    }

    /// Called when `poll_completed()` reports a completion. Finishes (or,
    /// for a read-modify-write's first phase, advances) the pending I/O,
    /// replying to the client via `provider` when it's fully done. Returns
    /// `true` if the pending I/O was fully completed (so the driver's main
    /// loop knows a new request can now be started).
    fn finish_pending(&mut self, provider: &SchemeProvider) -> bool {
        let Some(pending) = self.pending.take() else { return true };
        let status = self.queue.status_byte();
        if status != VIRTIO_BLK_S_OK {
            match &pending.kind {
                PendingKind::Read { .. } => {
                    let _ = provider.reply_read_err(pending.request_id, ErrorCode::IoError);
                }
                PendingKind::WriteAligned { .. } | PendingKind::WriteWritePhase { .. } => {
                    let _ = provider.reply_write_err(pending.request_id, ErrorCode::IoError);
                }
                PendingKind::WriteReadPhase { .. } => {
                    let _ = provider.reply_write_err(pending.request_id, ErrorCode::IoError);
                }
            }
            return true;
        }

        match pending.kind {
            PendingKind::Read { offset_in_first, want_len } => {
                let (ring, data_off) = self.queue.data_region();
                let mut out = alloc::vec![0u8; want_len];
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte = ring.read::<u8>(data_off + offset_in_first + i);
                }
                let _ = provider.reply_read_ok(pending.request_id, &out);
                true
            }
            PendingKind::WriteAligned { len } => {
                let _ = provider.reply_write_ok(pending.request_id, len as u32);
                true
            }
            PendingKind::WriteReadPhase { start_sector, num_sectors, offset_in_first, data } => {
                // Patch the caller's bytes into the just-read buffer, then
                // submit phase 2 (write) of the read-modify-write.
                let (ring, data_off) = self.queue.data_region();
                for (i, &byte) in data.iter().enumerate() {
                    ring.write::<u8>(data_off + offset_in_first + i, byte);
                }
                self.queue.submit(
                    VIRTIO_BLK_T_OUT,
                    start_sector,
                    (num_sectors * SECTOR_SIZE) as usize,
                    false,
                );
                self.pending = Some(PendingIo {
                    request_id: pending.request_id,
                    kind: PendingKind::WriteWritePhase { len: data.len() },
                });
                self.kick();
                false
            }
            PendingKind::WriteWritePhase { len } => {
                let _ = provider.reply_write_ok(pending.request_id, len as u32);
                true
            }
        }
    }
}

/// The single resource id this driver hands out from `Open` — see the
/// module doc comment's "single-in-flight-request model" and
/// `virtio-keyboard`'s identical `KEYBOARD_RESOURCE_ID` for why a fixed
/// constant (rather than a minted counter) is correct here: this driver
/// serves exactly one device via one scheme.
const BLOCK_RESOURCE_ID: u64 = 1;

/// Message a parent process sends on the spawn channel to ask this driver
/// to exit — see `virtio-keyboard`'s identical constant.
const SHUTDOWN_REQUEST: &[u8] = b"shutdown";

libpanda::main! {
    environment::log("virtio-blk: starting");

    let mailbox = Mailbox::default();

    // Subscribe once per concrete virtio-blk device ID (`device_subscribe`
    // takes an exact match table entry, not a list, per its doc comment) —
    // a single wildcard-device_id subscription would also match every other
    // virtio device (keyboard, gpu, ...) since only `vendor_id` would be
    // fixed. Both subscriptions share the same mailbox.
    let match_legacy = PciDeviceId {
        vendor_id: VIRTIO_VENDOR_ID,
        device_id: VIRTIO_BLK_DEVICE_ID_LEGACY,
        class: 0,
        class_mask: 0,
    };
    let match_modern = PciDeviceId {
        vendor_id: VIRTIO_VENDOR_ID,
        device_id: VIRTIO_BLK_DEVICE_ID_MODERN,
        class: 0,
        class_mask: 0,
    };
    fn as_bytes(id: &PciDeviceId) -> &[u8] {
        unsafe { core::slice::from_raw_parts(id as *const PciDeviceId as *const u8, core::mem::size_of::<PciDeviceId>()) }
    }

    let Ok(_sub_legacy) = device::device_subscribe(BusType::Pci, as_bytes(&match_legacy), mailbox.handle()) else {
        environment::log("virtio-blk: device_subscribe (legacy id) failed");
        return 1;
    };
    let Ok(_sub_modern) = device::device_subscribe(BusType::Pci, as_bytes(&match_modern), mailbox.handle()) else {
        environment::log("virtio-blk: device_subscribe (modern id) failed");
        return 1;
    };
    environment::log("virtio-blk: subscribed, waiting for device");

    let mut device: Option<DeviceState> = None;
    let mut provider: Option<SchemeProvider> = None;
    let mut open: bool = false;

    let parent = libpanda::ipc::Channel::parent();

    loop {
        if let Some(p) = provider.as_ref() {
            serve_provider_requests(p, device.as_mut(), &mut open);
        }

        if let Some(parent) = parent.as_ref() {
            let mut shutdown_buf = [0u8; 16];
            if let Ok(Some(n)) = parent.try_recv(&mut shutdown_buf)
                && &shutdown_buf[..n] == SHUTDOWN_REQUEST
            {
                environment::log("virtio-blk: shutdown requested, exiting");
                return 0;
            }
        }

        let Some((handle, events)) = mailbox.try_recv() else {
            process::yield_now();
            continue;
        };

        let raw_events = events.raw();
        if raw_events & EVENT_DEVICE_ADDED != 0 {
            handle_device_added(handle, &mut device);
            if device.is_some() && provider.is_none() {
                provider = register_scheme();
                if provider.is_some()
                    && let Some(parent) = parent.as_ref()
                {
                    let _ = parent.send(b"ready");
                }
            }
        }
        if raw_events & EVENT_DEVICE_REMOVED != 0 {
            handle_device_removed(&mut device, &mut provider, &mut open);
        }
        if raw_events & EVENT_DEVICE_IRQ != 0
            && let (Some(dev), Some(p)) = (device.as_mut(), provider.as_ref())
            && dev.pending.is_some()
        {
            while dev.queue.poll_completed() {
                if dev.finish_pending(p) {
                    break;
                }
                // A read-modify-write's phase 1 just submitted phase 2 —
                // keep draining in case it (implausibly, given a fresh kick)
                // already completed synchronously.
            }
        }
    }
}

fn handle_device_added(subscription_handle: Handle, device: &mut Option<DeviceState>) {
    if device.is_some() {
        return;
    }
    loop {
        let event = match device::device_poll_event(subscription_handle) {
            Ok(Some(event)) => event,
            Ok(None) => return,
            Err(err) => {
                log_error("virtio-blk: poll_event failed", err);
                return;
            }
        };
        if event.token == 0 {
            continue;
        }
        match init_device(event.token.into()) {
            Ok(state) => {
                environment::log("virtio-blk: device claimed and initialised");
                *device = Some(state);
                return;
            }
            Err(err) => {
                log_error("virtio-blk: device init failed", err);
                return;
            }
        }
    }
}

fn handle_device_removed(device: &mut Option<DeviceState>, provider: &mut Option<SchemeProvider>, open: &mut bool) {
    if device.is_none() {
        return;
    }
    environment::log("virtio-blk: device removed");
    if let Some(pending) = device.as_mut().and_then(|d| d.pending.take())
        && let Some(p) = provider.as_ref()
    {
        match pending.kind {
            PendingKind::Read { .. } => {
                let _ = p.reply_read_err(pending.request_id, ErrorCode::IoError);
            }
            _ => {
                let _ = p.reply_write_err(pending.request_id, ErrorCode::IoError);
            }
        }
    }
    *provider = None;
    *open = false;
    *device = None;
}

fn register_scheme() -> Option<SchemeProvider> {
    match SchemeProvider::register("block") {
        Ok(provider) => {
            environment::log("virtio-blk: registered block: scheme");
            Some(provider)
        }
        Err(err) => {
            log_error("virtio-blk: scheme registration failed", err);
            None
        }
    }
}

/// Drain and answer every request currently queued on the provider channel.
///
/// Exclusivity (the M1 claim rules `block:` must honour — see `claim_test`)
/// is enforced entirely here: `open` tracks whether the single resource is
/// currently held, `Open` fails `Busy` while it is, and `Close` releases it
/// — this driver has no separate kernel-side claim table to lean on (see
/// `resource::block_client::SchemeBlockDevice`'s module doc comment).
fn serve_provider_requests(provider: &SchemeProvider, device: Option<&mut DeviceState>, open: &mut bool) {
    let Some(device) = device else { return };
    let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];
    loop {
        // Never start a second I/O while one is still in flight — see the
        // module doc comment's "single-in-flight-request model". Stop
        // draining the provider channel until the pending one completes
        // (drained by the main loop's `EVENT_DEVICE_IRQ` handling); any
        // `Open`/`Close`/`Size` requests queued behind it will be served on
        // the next call.
        if device.pending.is_some() {
            return;
        }

        let request = match provider.try_recv(&mut buf) {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(_) => return,
        };

        match request {
            Request::Open { request_id, .. } => {
                if *open {
                    let _ = provider.reply_open_err(request_id, ErrorCode::Busy);
                } else {
                    *open = true;
                    let _ = provider.reply_open_ok(request_id, BLOCK_RESOURCE_ID);
                }
            }
            Request::Readdir { request_id, .. } => {
                let _ = provider.reply_readdir_err(request_id, ErrorCode::NotSupported);
            }
            Request::Size { request_id, resource_id } => {
                if resource_id == BLOCK_RESOURCE_ID {
                    let _ = provider.reply_size_ok(request_id, device.capacity_bytes);
                } else {
                    let _ = provider.reply_size_err(request_id, ErrorCode::InvalidHandle);
                }
            }
            Request::Read { request_id, resource_id, offset, len, .. } => {
                if resource_id != BLOCK_RESOURCE_ID {
                    let _ = provider.reply_read_err(request_id, ErrorCode::InvalidHandle);
                    continue;
                }
                let len = (len as usize).min(panda_abi::scheme_protocol::MAX_TRANSFER_SIZE);
                if offset >= device.capacity_bytes || len == 0 {
                    let _ = provider.reply_read_ok(request_id, &[]);
                    continue;
                }
                let available = device.capacity_bytes - offset;
                let len = (len as u64).min(available) as usize;
                device.start_read(request_id, offset, len);
            }
            Request::Write { request_id, resource_id, offset, data } => {
                if resource_id != BLOCK_RESOURCE_ID {
                    let _ = provider.reply_write_err(request_id, ErrorCode::InvalidHandle);
                    continue;
                }
                if offset >= device.capacity_bytes {
                    let _ = provider.reply_write_err(request_id, ErrorCode::InvalidOffset);
                    continue;
                }
                let available = (device.capacity_bytes - offset) as usize;
                let data = if data.len() > available { &data[..available] } else { data };
                let data = if data.len() > MAX_DATA_LEN { &data[..MAX_DATA_LEN] } else { data };
                if data.is_empty() {
                    let _ = provider.reply_write_ok(request_id, 0);
                    continue;
                }
                device.start_write(request_id, offset, data);
            }
            Request::Close { request_id, resource_id } => {
                if resource_id == BLOCK_RESOURCE_ID {
                    *open = false;
                }
                let _ = provider.reply_close_ok(request_id);
            }
            Request::Connect { request_id, .. } => {
                let _ = provider.reply_connect_err(request_id, ErrorCode::NotSupported);
            }
            // `block:` never serves `OP_DISPLAY_*` — those request kinds
            // (roadmap M4) only arrive on a `display:` provider
            // (`userspace/drivers/virtio-gpu`).
            Request::DisplayInfo { request_id, .. } => {
                let _ = provider.reply_display_info_err(request_id, ErrorCode::NotSupported);
            }
            Request::DisplayMap { request_id, .. } => {
                let _ = provider.reply_display_map_err(request_id, ErrorCode::NotSupported);
            }
            Request::DisplayFlush { request_id, .. } => {
                let _ = provider.reply_display_flush_err(request_id, ErrorCode::NotSupported);
            }
        }
    }
}

fn log_error(prefix: &str, _err: ErrorCode) {
    environment::log(prefix);
}

#[allow(dead_code)]
fn _assert_types(_: DeviceEvent, _: Response) {}
