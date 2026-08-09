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
//! # Virtio transport: the shared `virtio-pci` crate
//!
//! The PCI capability walk, BAR mapping, feature negotiation, split-virtqueue
//! layout and doorbell computation all live in `userspace/virtio-pci` — see
//! that crate's doc comment for the full explanation and for the three
//! transport landmines it encapsulates. The one structural difference from
//! virtio-keyboard's use of it: virtio-blk's **requestq** is
//! a request/response queue (the driver fills in a request header + data
//! buffer, the device fills in a status byte), not the eventq's
//! device-fills-everything pattern, so each request's descriptor chain is
//! built (and its buffers filled/read) by this driver rather than being
//! fixed at setup time.
//!
//! # Single-in-flight-request model
//!
//! Every client of `block:` (the filesystem service's `SchemeBlockDevice`,
//! which ext2 reads through — see `userspace/services/fs` — and a raw
//! process opening `block:/...`
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
use virtio_pci::{
    QueueConfig, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioTransport,
};

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
// virtio-blk requestq setup and request submission. Everything generic to
// virtio-pci modern transport (capability walk, feature negotiation,
// virtqueue layout, doorbell computation) lives in the shared `virtio-pci`
// crate.
// =============================================================================

/// virtio-blk has exactly one queue (its requestq) unless `VIRTIO_BLK_F_MQ`
/// is negotiated (it isn't, here).
const REQUESTQ_INDEX: u16 = 0;

/// Requestq size: only 3 descriptors are ever used at once (header, data,
/// status — see the module doc comment), but virtio queue sizes must be a
/// power of two, and `VirtioTransport::setup_queue` clamps this to the
/// device's own advertised maximum (writing the chosen size back to the
/// device — see that method's queue-size-writeback landmine note, which
/// documents a real out-of-bounds avail-ring bug this driver hit).
const REQUESTQ_SIZE_WANTED: u16 = 8;
/// Fewest descriptors a single request needs: header, data, status.
const REQUESTQ_MIN_SIZE: u16 = 3;

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

/// Sub-layout of the queue's driver-owned scratch area: the single in-flight
/// request's header, data and status buffers (see the module doc comment's
/// "single-in-flight-request model"). Offsets are relative to the scratch
/// area's base (`VirtQueue::scratch`/`VirtQueue::scratch_iova`).
const HEADER_OFF: usize = 0;
const DATA_OFF: usize = HEADER_OFF + BLK_HEADER_LEN;
const STATUS_OFF: usize = DATA_OFF + MAX_DATA_LEN;
const SCRATCH_LEN: usize = STATUS_OFF + BLK_STATUS_LEN;

fn init_device(token: Handle) -> Result<DeviceState> {
    // No virtio-blk feature bits are needed beyond the mandatory
    // `VIRTIO_F_VERSION_1` the transport always negotiates.
    let transport = VirtioTransport::claim_and_init(token, 0)?;

    let mailbox = Mailbox::default();
    transport.subscribe_irq(&mailbox)?;

    let queue = transport.setup_queue(QueueConfig {
        index: REQUESTQ_INDEX,
        wanted_size: REQUESTQ_SIZE_WANTED,
        min_size: REQUESTQ_MIN_SIZE,
        scratch_len: SCRATCH_LEN,
        scratch_len_per_descriptor: 0,
    })?;

    // `VirtioTransport::set_driver_ok`'s doc comment explains why a driver
    // whose queue already has buffers published must kick *before* this —
    // there is nothing to kick here (the requestq starts empty; the initial
    // kick requirement is specific to virtio-keyboard's eventq
    // pre-publishing every descriptor at setup time), so `DRIVER_OK` is set
    // immediately after the queue is enabled.
    transport.set_driver_ok();

    // Read `capacity` (virtio-v1.1 5.2.4's `struct virtio_blk_config`, first
    // field, in sectors) from the device-specific config region.
    let (device_cfg, device_cfg_off) = transport.device_config().ok_or(ErrorCode::NotSupported)?;
    let capacity_sectors = device_cfg.read::<u64>(device_cfg_off);

    Ok(DeviceState {
        transport,
        capacity_bytes: capacity_sectors * SECTOR_SIZE,
        queue,
        pending: None,
    })
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
    /// Owns the claimed device handle and its MMIO mappings — dropping it
    /// releases every MMIO mapping/DMA allocation/IRQ subscription (see
    /// `DeviceResource::teardown`).
    transport: VirtioTransport,
    capacity_bytes: u64,
    queue: VirtQueue,
    pending: Option<PendingIo>,
}

impl DeviceState {
    fn kick(&self) {
        self.queue.kick(self.transport.notify_region());
    }

    /// Build and publish a 3-descriptor request (header/data/status) from
    /// this queue's fixed scratch buffers. `data_len` is the data
    /// descriptor's length; `is_read` selects whether the data descriptor is
    /// device-writable (IN) or device-readable (OUT).
    fn submit(&mut self, request_type: u32, sector: u64, data_len: usize, is_read: bool) {
        let (ring, scratch) = self.queue.scratch();
        ring.write::<u32>(scratch + HEADER_OFF, request_type);
        ring.write::<u32>(scratch + HEADER_OFF + 4, 0); // reserved
        ring.write::<u64>(scratch + HEADER_OFF + 8, sector);

        // Header descriptor (index 0): device-readable, driver-filled.
        self.queue.set_descriptor(
            0,
            self.queue.scratch_iova(HEADER_OFF),
            BLK_HEADER_LEN as u32,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        self.queue.set_descriptor(
            1,
            self.queue.scratch_iova(DATA_OFF),
            data_len as u32,
            VIRTQ_DESC_F_NEXT | if is_read { VIRTQ_DESC_F_WRITE } else { 0 },
            2,
        );
        // Status descriptor (index 2): device-writable.
        self.queue.set_descriptor(
            2,
            self.queue.scratch_iova(STATUS_OFF),
            BLK_STATUS_LEN as u32,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        self.queue.publish(0);
    }

    /// The scratch data buffer's region and absolute offset, for filling
    /// (writes) or reading back (reads) — always the same fixed offset since
    /// only one request is ever in flight.
    fn data_region(&self) -> (&MmioRegion, usize) {
        let (ring, scratch) = self.queue.scratch();
        (ring, scratch + DATA_OFF)
    }

    fn status_byte(&self) -> u8 {
        let (ring, scratch) = self.queue.scratch();
        ring.read::<u8>(scratch + STATUS_OFF)
    }

    /// Returns `true` if a new used-ring entry appeared (the in-flight
    /// request completed).
    fn poll_completed(&mut self) -> bool {
        self.queue.pop_used().is_some()
    }

    /// Begin a `Read` request: fetch every sector spanning `[offset, offset
    /// + len)`. Caller has already checked `self.pending.is_none()`.
    fn start_read(&mut self, request_id: u64, offset: u64, len: usize) {
        let start_sector = offset / SECTOR_SIZE;
        let offset_in_first = (offset % SECTOR_SIZE) as usize;
        let end = offset + len as u64;
        let end_sector = end.div_ceil(SECTOR_SIZE);
        let num_sectors = (end_sector - start_sector).max(1);

        self.submit(VIRTIO_BLK_T_IN, start_sector, (num_sectors * SECTOR_SIZE) as usize, true);
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
            let (ring, data_off) = self.data_region();
            for (i, chunk) in data.chunks(8).enumerate() {
                let mut buf8 = [0u8; 8];
                buf8[..chunk.len()].copy_from_slice(chunk);
                ring.write::<u64>(data_off + i * 8, u64::from_le_bytes(buf8));
            }
            self.submit(VIRTIO_BLK_T_OUT, offset / SECTOR_SIZE, data.len(), false);
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

        self.submit(VIRTIO_BLK_T_IN, start_sector, (num_sectors * SECTOR_SIZE) as usize, true);
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
        let status = self.status_byte();
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
                let (ring, data_off) = self.data_region();
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
                let (ring, data_off) = self.data_region();
                for (i, &byte) in data.iter().enumerate() {
                    ring.write::<u8>(data_off + offset_in_first + i, byte);
                }
                self.submit(
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
            while dev.poll_completed() {
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
/// — this driver has no separate kernel-side claim table to lean on (the
/// kernel's `devices::claims` was deleted in roadmap M5 once its last user,
/// the in-kernel ext2 mount, moved to userspace).
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
            // The M5 filesystem request kinds only ever arrive on the
            // filesystem service's `file:` provider
            // (`userspace/services/fs`), never here.
            Request::Stat { .. }
            | Request::Create { .. }
            | Request::Unlink { .. }
            | Request::Mkdir { .. }
            | Request::Rmdir { .. } => {
                let _ = provider.reply_unsupported(&request);
            }
        }
    }
}

fn log_error(prefix: &str, _err: ErrorCode) {
    environment::log(prefix);
}

#[allow(dead_code)]
fn _assert_types(_: DeviceEvent, _: Response) {}
