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
//! # Virtio transport: the shared `virtio-pci` crate
//!
//! The PCI capability walk, BAR mapping, feature negotiation, split-virtqueue
//! layout and doorbell computation all live in `userspace/virtio-pci` (see
//! that crate's doc comment for why they are hand-rolled against the
//! `OP_DEVICE_*`/`OP_DMA_*` syscalls instead of using the kernel-only
//! `virtio_drivers` crate, and for the three transport landmines it
//! encapsulates). This driver only supplies what is specific to virtio-input:
//! one fixed-size event buffer per descriptor, all published up front, and
//! the recycle-and-renotify drain loop in `DeviceState::poll_ring`.
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
    PciDeviceId,
};
use virtio_pci::{QueueConfig, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioTransport};
use libpanda::error::Result;
use libpanda::keyboard::RawInputEvent;
use libpanda::mailbox::Mailbox;
use libpanda::scheme::SchemeProvider;
use libpanda::{Handle, environment, process};

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
// virtio-input eventq setup. Everything generic to virtio-pci modern
// transport (capability walk, feature negotiation, virtqueue layout, doorbell
// computation) lives in the shared `virtio-pci` crate.
// =============================================================================

/// The eventq (virtio-input queue index 0) — the only queue this driver
/// uses (see the module doc comment).
const EVENTQ_INDEX: u16 = 0;

/// Maximum key events buffered before the oldest is dropped, matching the
/// deleted kernel driver's `RingBuffer<64>`. Also the eventq's requested
/// queue size, clamped to the device's actual maximum by
/// `VirtioTransport::setup_queue`.
const EVENT_BUFFER_CAPACITY: usize = 64;

/// Bytes per eventq buffer: one `struct virtio_input_event` (virtio-v1.1
/// 5.8.6), which matches `libpanda::keyboard::RawInputEvent`'s layout.
const INPUT_EVENT_LEN: usize = 8;

/// Claim the device, bring up the virtio transport, allocate and populate the
/// eventq, subscribe to IRQs, and set `DRIVER_OK`. Mirrors
/// `panda-kernel/src/devices/virtio_keyboard.rs::init_from_pci_device`'s
/// sequence (claim -> transport setup -> feature negotiation -> queue setup
/// -> `DRIVER_OK` -> IRQ registration), but through the userspace
/// `OP_DEVICE_*` syscalls and the shared `virtio-pci` transport instead of the
/// kernel-only `virtio_drivers` crate.
fn init_device(token: Handle) -> Result<DeviceState> {
    // No virtio-input feature bits are needed beyond the mandatory
    // `VIRTIO_F_VERSION_1` the transport always negotiates.
    let transport = VirtioTransport::claim_and_init(token, 0)?;

    // Subscribe to (and thereby enable) the device's MSI-X vector *before*
    // `setup_queue` programs `queue_msix_vector` to point at it and *before*
    // `DRIVER_OK` is set — see `VirtioTransport::subscribe_irq`'s doc comment.
    let mailbox = Mailbox::default();
    transport.subscribe_irq(&mailbox)?;

    let mut queue = transport.setup_queue(QueueConfig {
        index: EVENTQ_INDEX,
        wanted_size: EVENT_BUFFER_CAPACITY as u16,
        min_size: 1,
        scratch_len: 0,
        scratch_len_per_descriptor: INPUT_EVENT_LEN,
    })?;

    // Pre-populate every descriptor as a device-writable buffer and make all
    // of them available: this queue is purely device-to-driver — the driver
    // never fills in event *contents*, only supplies empty buffers for the
    // device to write into (see `DeviceState::poll_ring`).
    for i in 0..queue.size() {
        queue.set_descriptor(
            i,
            queue.scratch_iova(INPUT_EVENT_LEN * i as usize),
            INPUT_EVENT_LEN as u32,
            VIRTQ_DESC_F_WRITE,
            0, // next (unused: single-descriptor chains)
        );
        queue.publish(i);
    }

    // Kick the queue once, immediately, *before* `DRIVER_OK`: every
    // descriptor was just published as available, and the device does not
    // proactively poll the avail ring. Without this initial kick the eventq
    // sits idle forever — `poll_ring`'s own kick only fires *after* draining
    // a used-ring entry, so the very first event would never arrive to
    // trigger it. See `VirtioTransport::set_driver_ok`'s doc comment for the
    // full incident history behind this ordering.
    queue.kick(transport.notify_region());
    transport.set_driver_ok();

    Ok(DeviceState { transport, queue, events: Vec::with_capacity(EVENT_BUFFER_CAPACITY) })
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
    /// Owns the claimed device handle and its MMIO mappings. Held (rather
    /// than dropped immediately) because dropping it releases every MMIO
    /// mapping/DMA allocation/IRQ subscription this driver is supposed to be
    /// holding (see `DeviceResource::teardown`). This driver never resets the
    /// device on removal — dropping the claim tears it down, registers
    /// included.
    transport: VirtioTransport,
    queue: VirtQueue,
    events: Vec<RawInputEvent>,
}

impl DeviceState {
    /// Drain every newly-completed used-ring entry, decode its event, recycle
    /// the descriptor back onto the avail ring so the device can reuse it,
    /// and notify the device once if anything was recycled. Decoded `EV_KEY`
    /// events are buffered oldest-first, dropping the oldest past
    /// `EVENT_BUFFER_CAPACITY` (matching the deleted kernel driver's ring
    /// buffer behaviour). Returns whether any new events were buffered.
    fn poll_ring(&mut self) -> bool {
        let mut buffered_new = false;
        let mut recycled = false;

        while let Some(used) = self.queue.pop_used() {
            let desc_index = used.id as u16;

            let (ring, scratch_off) = self.queue.scratch();
            let event_off = scratch_off + INPUT_EVENT_LEN * desc_index as usize;
            let event_type = ring.read::<u16>(event_off);
            let code = ring.read::<u16>(event_off + 2);
            let value = ring.read::<u32>(event_off + 4);
            if event_type == EV_KEY {
                if self.events.len() >= EVENT_BUFFER_CAPACITY {
                    self.events.remove(0);
                }
                self.events.push(RawInputEvent { event_type, code, value });
                buffered_new = true;
            }

            // Recycle: the buffer at `desc_index` is free again (the
            // descriptor table entry itself doesn't need rewriting — it still
            // points at the same buffer with the same `VIRTQ_DESC_F_WRITE`
            // flags from `init_device`), so just republish it on the avail
            // ring.
            self.queue.publish(desc_index);
            recycled = true;
        }

        if recycled {
            self.queue.kick(self.transport.notify_region());
        }
        buffered_new
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
            Request::Size { request_id, .. } => {
                // The eventq has no addressable size (see `Request::Size`'s
                // doc comment) — only `resource::block_client::SchemeBlockDevice`
                // (M4's block driver service) uses this request.
                let _ = provider.reply_size_err(request_id, ErrorCode::NotSupported);
            }
            // `keyboard:` never serves `OP_DISPLAY_*` — those request kinds
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
