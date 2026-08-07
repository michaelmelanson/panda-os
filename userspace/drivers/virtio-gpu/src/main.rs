//! Userspace virtio-gpu driver (roadmap M4, "display driver service").
//!
//! Replaces the deleted kernel-resident driver
//! (`panda-kernel/src/devices/virtio_gpu/` — deleted in the same commit as
//! this file, an atomic cutover per `plans/ROADMAP.md`'s M4 section). Claims
//! the virtio-gpu PCI device, sets up a 2D scanout, and registers the
//! `display:` scheme with the same `OP_DISPLAY_INFO`/`MAP`/`FLUSH` contract
//! clients already use (`userspace/tests/display_test`,
//! `userspace/compositor`) — that contract is permanent; only the provider
//! moved to userspace.
//!
//! # Virtio transport: the shared `virtio-pci` crate
//!
//! The virtio-pci modern-transport plumbing (PCI capability walk, feature
//! negotiation, split virtqueue, doorbell computation) lives in
//! `userspace/virtio-pci`, shared with `userspace/drivers/virtio-keyboard`
//! and `userspace/drivers/virtio-blk` — see that crate's doc comment for why
//! it is hand-rolled against the `OP_DEVICE_*`/`OP_DMA_*` syscalls instead of
//! using the kernel-only `virtio_drivers` crate, and for the three transport
//! landmines it encapsulates.
//!
//! # Only the control queue; no cursor queue
//!
//! virtio-gpu exposes two virtqueues: controlq (index 0, 2D/3D resource and
//! scanout commands) and cursorq (index 1, hardware cursor updates). Only
//! controlq is set up — this driver never draws a hardware cursor, matching
//! the deleted kernel driver's scope (it only ever called
//! `change_resolution`/`flush`, both controlq-only operations).
//!
//! Every controlq request in this driver is synchronous request/response
//! (one descriptor chain in flight at a time, like
//! `virtio_drivers::device::gpu::VirtIOGpu::request`). Completion is
//! delivered via a dedicated MSI-X vector (entry 0 of
//! `DeviceResource::subscribe_irq`'s single-vector-per-device pool — no
//! other queue on this device needs a vector, so the pool's one-slot limit
//! is a non-issue here): `wait_for_response` blocks on a mailbox
//! (`Mailbox::recv`) for the interrupt rather than polling the used ring in
//! a spin loop.
//!
//! An earlier version of this driver polled instead — reading the DMA'd
//! used-ring memory directly is valid regardless of whether an interrupt
//! fires, and a synchronous poll loop avoided the ceremony of wiring up an
//! IRQ mailbox. That worked functionally but was the wrong trade for a
//! *userspace* driver specifically: unlike a kernel driver's in-place
//! hardware access, every poll iteration here is a `MmioRegion` read that
//! still costs nothing extra, but yielding periodically to avoid starving
//! the host (`process::yield_now()`) turned into a real wall-clock cost
//! under `make test`'s default parallelism (several QEMU instances
//! contending for the host's CPUs) — enough added latency per flush,
//! multiplied across every screenshot-verified compositor test's several
//! flushes, to blow `compositor_test_child`'s fixed tick budget and
//! TIMEOUT even though nothing was actually stuck (see that binary's
//! module doc comment). Blocking on the interrupt properly fixes this at
//! the root: this process consumes zero CPU while a request is in flight,
//! so it adds no contention for other processes to lose scheduler time to,
//! and wakes promptly (a real hardware/QEMU-delivered interrupt, not a
//! polling interval) once the device actually completes the request.
//!
//! # Framebuffer memory
//!
//! The framebuffer is a `dma_alloc`'d (physically contiguous,
//! IOMMU-mapped) buffer, attached to a virtio-gpu 2D resource via
//! `RESOURCE_ATTACH_BACKING` with a single backing entry. `OP_DISPLAY_MAP`
//! is served by handing the kernel that buffer's *guest*-physical address
//! (the `iova` `dma_alloc` returns) directly — see
//! `panda_abi::scheme_protocol::MSG_DISPLAY_MAP`'s doc comment for why the
//! wire protocol carries a raw physical range for this one request instead
//! of proxied bytes, and the trust assumption that follows from it.

#![no_std]
#![no_main]

extern crate alloc;

use libpanda::device::{self, BusType, EVENT_DEVICE_ADDED, EVENT_DEVICE_REMOVED, PciDeviceId};
use libpanda::error::Result;
use libpanda::mailbox::Mailbox;
use libpanda::scheme::SchemeProvider;
use libpanda::{Handle, environment, process};
use panda_abi::ErrorCode;
use panda_abi::scheme_protocol::Request;

use virtio_pci::{QueueConfig, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioTransport};

// 0x1AF4 is the Red Hat/virtio PCI vendor ID; 0x1050 is the "virtio 1.0 GPU
// device" ID (see the deleted `panda-kernel/src/devices/virtio_gpu/mod.rs`
// and `panda-kernel/src/devices.rs`'s old dispatch arm). Unlike
// virtio-input (keyboard vs. mouse), no other virtio device shares this
// vendor:device pair, so no PCI class filter is needed here the way
// virtio-keyboard needs one — see that driver's module doc comment for the
// collision it had to guard against.
const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_GPU_DEVICE_ID: u16 = 0x1050;

libpanda::pci_device_table![{ vendor: VIRTIO_VENDOR_ID, device: VIRTIO_GPU_DEVICE_ID }];

// =============================================================================
// virtio-gpu controlq: a small, fixed-size queue used for one synchronous
// request/response at a time (mirrors
// `virtio_drivers::device::gpu::VirtIOGpu::request`, which also serialises on
// one in-flight descriptor chain). Everything generic to virtio-pci modern
// transport (capability walk, feature negotiation, virtqueue layout, doorbell
// computation) lives in the shared `virtio-pci` crate.
// =============================================================================

const CONTROLQ_INDEX: u16 = 0;

/// Fixed queue size: only one request is ever in flight, so two descriptors
/// (send + recv) is enough; virtqueue sizes must be a power of two.
const QSIZE: u16 = 2;
/// Send/recv buffer size: comfortably larger than the largest command this
/// driver issues (`RESOURCE_ATTACH_BACKING` at 56 bytes) or response it
/// reads (`RESP_DISPLAY_INFO` at 32 bytes).
const CMD_BUF_LEN: usize = 256;

/// Sub-layout of the queue's driver-owned scratch area: the send buffer (the
/// command this driver writes) followed by the recv buffer (the response the
/// device writes). Offsets are relative to the scratch area's base.
const SEND_OFF: usize = 0;
const RECV_OFF: usize = SEND_OFF + CMD_BUF_LEN;
const SCRATCH_LEN: usize = RECV_OFF + CMD_BUF_LEN;

/// One decoded GPU display-info response: whether scanout 0 is enabled, and
/// its rectangle.
struct DisplayInfo {
    width: u32,
    height: u32,
}

/// virtio-gpu control-queue command opcodes (virtio-v1.1 5.7.6.7 /
/// `vendor/virtio-drivers/src/device/gpu/mod.rs`'s `Command`).
mod cmd {
    pub const GET_DISPLAY_INFO: u32 = 0x100;
    pub const RESOURCE_CREATE_2D: u32 = 0x101;
    pub const SET_SCANOUT: u32 = 0x103;
    pub const RESOURCE_FLUSH: u32 = 0x104;
    pub const TRANSFER_TO_HOST_2D: u32 = 0x105;
    pub const RESOURCE_ATTACH_BACKING: u32 = 0x106;

    pub const OK_NODATA: u32 = 0x1100;
    pub const OK_DISPLAY_INFO: u32 = 0x1101;
}

const FORMAT_B8G8R8A8_UNORM: u32 = 1;
const SCANOUT_ID: u32 = 0;
const RESOURCE_ID_FB: u32 = 1;

/// Write a `CtrlHeader` (24 bytes: hdr_type, flags, fence_id, ctx_id,
/// padding) at the start of `buf`.
fn write_ctrl_header(buf: &mut alloc::vec::Vec<u8>, hdr_type: u32) {
    buf.extend_from_slice(&hdr_type.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // flags
    buf.extend_from_slice(&0u64.to_le_bytes()); // fence_id
    buf.extend_from_slice(&0u32.to_le_bytes()); // ctx_id
    buf.extend_from_slice(&0u32.to_le_bytes()); // padding
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

struct Gpu {
    /// Owns the claimed device handle and its MMIO mappings — dropping it
    /// releases every MMIO mapping/DMA allocation/IRQ subscription (see
    /// `DeviceResource::teardown`).
    transport: VirtioTransport,
    queue: VirtQueue,
    /// Dedicated mailbox `EVENT_DEVICE_IRQ` is posted to (via
    /// `VirtioTransport::subscribe_irq`, called once in `init_device`) —
    /// `wait_for_response` blocks on this rather than polling the used ring.
    /// Kept separate from the driver's main event-subscription mailbox so
    /// blocking here can never swallow an
    /// `EVENT_DEVICE_ADDED`/`EVENT_DEVICE_REMOVED` the main loop needs to
    /// see.
    irq_mailbox: Mailbox,
    width: u32,
    height: u32,
    framebuffer_iova: u64,
}

impl Gpu {
    /// One synchronous controlq round trip: write `cmd_bytes` into the send
    /// buffer, publish the send/recv descriptor chain, ring the doorbell, and
    /// block until the device answers.
    fn request(&mut self, cmd_bytes: &[u8]) -> alloc::vec::Vec<u8> {
        let (ring, scratch) = self.queue.scratch();
        for (i, b) in cmd_bytes.iter().enumerate() {
            ring.write::<u8>(scratch + SEND_OFF + i, *b);
        }

        // Descriptor 0: the command (device-readable), chained to
        // descriptor 1: the response buffer (device-writable).
        self.queue.set_descriptor(
            0,
            self.queue.scratch_iova(SEND_OFF),
            cmd_bytes.len() as u32,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        self.queue.set_descriptor(
            1,
            self.queue.scratch_iova(RECV_OFF),
            CMD_BUF_LEN as u32,
            VIRTQ_DESC_F_WRITE,
            0,
        );
        self.queue.publish(0);
        self.queue.kick(self.transport.notify_region());

        let len = self.wait_for_response();
        let (ring, scratch) = self.queue.scratch();
        (0..len).map(|i| ring.read::<u8>(scratch + RECV_OFF + i)).collect()
    }

    /// Block until the in-flight request completes, then return the number of
    /// bytes the device wrote into the recv buffer.
    ///
    /// Checks the used ring first (a cheap in-memory read) in case the
    /// completion already posted before this call — e.g. a very fast QEMU
    /// response beating the driver back here — then blocks on
    /// `irq_mailbox.recv()` for the MSI-X interrupt otherwise, re-checking
    /// after every wake. This costs no CPU while waiting (unlike a spin/
    /// yield poll loop — see the module doc comment for why that mattered
    /// under `make test`'s default parallelism) and wakes promptly on the
    /// real interrupt. Bounded to `MAX_WAKES` blocking waits so a completely
    /// wedged device (e.g. misconfigured MSI-X routing) fails loudly by
    /// returning an empty response instead of hanging this process forever —
    /// in practice this never gets close to the bound, since the mailbox is
    /// dedicated to this one IRQ source (see `Gpu::irq_mailbox`'s doc
    /// comment) and every wake is a real completion.
    fn wait_for_response(&mut self) -> usize {
        const MAX_WAKES: u32 = 1000;
        let mut wakes = 0u32;
        loop {
            if let Some(used) = self.queue.pop_used() {
                return used.len as usize;
            }
            if wakes >= MAX_WAKES {
                return 0;
            }
            self.irq_mailbox.recv();
            wakes += 1;
        }
    }

    fn send_buf(&self, len: usize) -> alloc::vec::Vec<u8> {
        alloc::vec::Vec::with_capacity(len)
    }

    fn get_display_info(&mut self) -> Result<DisplayInfo> {
        let mut buf = self.send_buf(24);
        write_ctrl_header(&mut buf, cmd::GET_DISPLAY_INFO);
        let resp = self.request(&buf);
        if resp.len() < 24 + 24 || read_u32(&resp, 0) != cmd::OK_DISPLAY_INFO {
            return Err(ErrorCode::IoError);
        }
        // RespDisplayInfo: header(24) + rect{x,y,width,height}(16) + enabled(4) + flags(4)
        let width = read_u32(&resp, 24 + 8);
        let height = read_u32(&resp, 24 + 12);
        Ok(DisplayInfo { width, height })
    }

    fn resource_create_2d(&mut self, resource_id: u32, width: u32, height: u32) -> Result<()> {
        let mut buf = self.send_buf(40);
        write_ctrl_header(&mut buf, cmd::RESOURCE_CREATE_2D);
        buf.extend_from_slice(&resource_id.to_le_bytes());
        buf.extend_from_slice(&FORMAT_B8G8R8A8_UNORM.to_le_bytes());
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        let resp = self.request(&buf);
        check_ok_nodata(&resp)
    }

    fn resource_attach_backing(&mut self, resource_id: u32, iova: u64, length: u32) -> Result<()> {
        let mut buf = self.send_buf(56);
        write_ctrl_header(&mut buf, cmd::RESOURCE_ATTACH_BACKING);
        buf.extend_from_slice(&resource_id.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes()); // nr_entries
        buf.extend_from_slice(&iova.to_le_bytes());
        buf.extend_from_slice(&length.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // padding
        let resp = self.request(&buf);
        check_ok_nodata(&resp)
    }

    fn set_scanout(&mut self, scanout_id: u32, resource_id: u32, width: u32, height: u32) -> Result<()> {
        let mut buf = self.send_buf(40);
        write_ctrl_header(&mut buf, cmd::SET_SCANOUT);
        buf.extend_from_slice(&0u32.to_le_bytes()); // rect.x
        buf.extend_from_slice(&0u32.to_le_bytes()); // rect.y
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        buf.extend_from_slice(&scanout_id.to_le_bytes());
        buf.extend_from_slice(&resource_id.to_le_bytes());
        let resp = self.request(&buf);
        check_ok_nodata(&resp)
    }

    fn transfer_to_host_2d(&mut self, resource_id: u32, x: u32, y: u32, width: u32, height: u32) -> Result<()> {
        let mut buf = self.send_buf(56);
        write_ctrl_header(&mut buf, cmd::TRANSFER_TO_HOST_2D);
        buf.extend_from_slice(&x.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes()); // offset
        buf.extend_from_slice(&resource_id.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // padding
        let resp = self.request(&buf);
        check_ok_nodata(&resp)
    }

    fn resource_flush(&mut self, resource_id: u32, x: u32, y: u32, width: u32, height: u32) -> Result<()> {
        let mut buf = self.send_buf(40);
        write_ctrl_header(&mut buf, cmd::RESOURCE_FLUSH);
        buf.extend_from_slice(&x.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        buf.extend_from_slice(&resource_id.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // padding
        let resp = self.request(&buf);
        check_ok_nodata(&resp)
    }

    /// Full round trip for `OP_DISPLAY_FLUSH`: copy the framebuffer to the
    /// host, then flush it to the screen.
    ///
    /// Always transfers/flushes the *whole* surface, ignoring `region`
    /// beyond the validation `resource::display::DisplayResource::flush`
    /// (the caller, pre-M4) already did against it — same as the deleted
    /// kernel-resident driver's contract (see that struct's `flush` doc
    /// comment, carried forward here: "the underlying driver exposes no
    /// partial-rect flush... the rectangle currently serves as validation
    /// and future-proofing... rather than as a bandwidth optimisation").
    /// `TRANSFER_TO_HOST_2D`'s `rect` gives the destination sub-region on
    /// the resource, but its `offset` field must independently give the
    /// matching byte offset into the linear backing buffer
    /// (`y * stride + x * bpp`) for that sub-region — get that wrong (e.g.
    /// leave it `0` while `rect` has a nonzero origin, as an earlier
    /// version of this function did) and the host samples the wrong source
    /// pixels, rendering composited windows at a shifted, garbled offset
    /// (caught by `window_test`'s screenshot comparison). Always passing
    /// the full-surface rect together with `offset=0` sidesteps needing to
    /// get that computation right for a partial region we don't need to
    /// support anyway — `vendor/virtio-drivers/src/device/gpu/mod.rs`'s own
    /// `flush()` does the same (always whole-surface, `offset` hardcoded to
    /// `0`), which is the reference this driver otherwise mirrors.
    fn flush(&mut self, _region: Option<(u32, u32, u32, u32)>) -> Result<()> {
        let (x, y, width, height) = (0, 0, self.width, self.height);
        self.transfer_to_host_2d(RESOURCE_ID_FB, x, y, width, height)?;
        self.resource_flush(RESOURCE_ID_FB, x, y, width, height)
    }
}

fn check_ok_nodata(resp: &[u8]) -> Result<()> {
    if resp.len() < 4 || read_u32(resp, 0) != cmd::OK_NODATA {
        return Err(ErrorCode::IoError);
    }
    Ok(())
}

/// Claim the device, bring up the virtio transport, set up the controlq,
/// fetch the display's preferred resolution, allocate and attach a
/// framebuffer, and set the scanout.
fn init_device(token: Handle) -> Result<Gpu> {
    // No virtio-gpu feature bits are needed beyond the mandatory
    // `VIRTIO_F_VERSION_1` the transport always negotiates.
    let transport = VirtioTransport::claim_and_init(token, 0)?;

    // Subscribe to (and thereby enable) the device's MSI-X vector *before*
    // `setup_queue` programs `queue_msix_vector` to point at it and *before*
    // `DRIVER_OK` is set — see `VirtioTransport::subscribe_irq`'s doc comment.
    let irq_mailbox = Mailbox::create()?;
    transport.subscribe_irq(&irq_mailbox)?;

    let queue = transport.setup_queue(QueueConfig {
        index: CONTROLQ_INDEX,
        wanted_size: QSIZE,
        min_size: QSIZE,
        scratch_len: SCRATCH_LEN,
        scratch_len_per_descriptor: 0,
    })?;

    // Nothing is published on the controlq at init time (every request
    // publishes its own chain and kicks), so there is no pre-`DRIVER_OK`
    // kick to make here — see `VirtioTransport::set_driver_ok`'s doc comment
    // for when there is.
    transport.set_driver_ok();

    let device_handle = transport.device_handle();
    let mut gpu = Gpu {
        transport,
        queue,
        irq_mailbox,
        width: 0,
        height: 0,
        framebuffer_iova: 0,
    };

    let info = gpu.get_display_info().unwrap_or(DisplayInfo { width: 0, height: 0 });
    let (width, height) = if info.width == 0 || info.height == 0 {
        (1024, 768)
    } else {
        (info.width, info.height)
    };

    let fb_size = (width as usize) * (height as usize) * 4;
    let (fb_virt, fb_iova) = device::dma_alloc(device_handle, fb_size)?;
    let _ = fb_virt;

    gpu.resource_create_2d(RESOURCE_ID_FB, width, height)?;
    gpu.resource_attach_backing(RESOURCE_ID_FB, fb_iova, fb_size as u32)?;
    gpu.set_scanout(SCANOUT_ID, RESOURCE_ID_FB, width, height)?;

    gpu.width = width;
    gpu.height = height;
    gpu.framebuffer_iova = fb_iova;

    Ok(gpu)
}


// =============================================================================
// display: scheme serving
// =============================================================================

const PIXEL_FORMAT_ARGB8888: u32 = 0;
/// Resource id this driver hands out from `Open` — there is exactly one
/// display/resource per running instance of this driver.
const DISPLAY_RESOURCE_ID: u64 = 1;

/// State for the one virtio-gpu device this driver currently owns.
struct DeviceState {
    gpu: Gpu,
    /// Whether a client currently holds the exclusive `display:` claim (a
    /// second `Open` before `Close`/disconnect is refused `Busy`) — the
    /// scheme-level equivalent of the deleted kernel driver's
    /// `crate::devices::claims::claim` exclusivity, now enforced by this
    /// driver instead of the kernel (see
    /// `panda-kernel/src/resource/scheme.rs`'s "Display Scheme" module doc
    /// comment).
    opened: bool,
}

libpanda::main! {
    environment::log("virtio-gpu: starting");

    let mailbox = Mailbox::default();

    const PCI_CLASS_ANY: u32 = 0; // no other device shares this vendor:device pair
    let match_entry = PciDeviceId {
        vendor_id: VIRTIO_VENDOR_ID,
        device_id: VIRTIO_GPU_DEVICE_ID,
        class: PCI_CLASS_ANY,
        class_mask: 0,
    };
    let match_bytes = unsafe {
        core::slice::from_raw_parts(
            &match_entry as *const PciDeviceId as *const u8,
            core::mem::size_of::<PciDeviceId>(),
        )
    };
    let Ok(_subscription) = device::device_subscribe(BusType::Pci, match_bytes, mailbox.handle())
    else {
        environment::log("virtio-gpu: device_subscribe failed");
        return 1;
    };
    environment::log("virtio-gpu: subscribed, waiting for device");

    let mut device: Option<DeviceState> = None;
    let mut provider: Option<SchemeProvider> = None;

    let parent = libpanda::ipc::Channel::parent();

    loop {
        if let Some(p) = provider.as_ref() {
            serve_provider_requests(p, device.as_mut());
        }

        // Cooperative shutdown — see
        // `userspace/drivers/virtio-keyboard`'s identical convention and
        // `plans/device-driver-model.md`'s Phase 7 landed-deviations note
        // for why this exists.
        if let Some(parent) = parent.as_ref() {
            let mut shutdown_buf = [0u8; 16];
            if let Ok(Some(n)) = parent.try_recv(&mut shutdown_buf)
                && &shutdown_buf[..n] == SHUTDOWN_REQUEST
            {
                environment::log("virtio-gpu: shutdown requested, exiting");
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
                if provider.is_some() {
                    if let Some(parent) = parent.as_ref() {
                        let _ = parent.send(b"ready");
                    }
                }
            }
        }
        if raw_events & EVENT_DEVICE_REMOVED != 0 {
            provider = None;
            device = None;
        }
        // `EVENT_DEVICE_IRQ` is posted to `Gpu::irq_mailbox`, a
        // dedicated mailbox `Gpu::request`/`wait_for_response` block on
        // directly — never to this one (the driver's main event-
        // subscription mailbox), so there's nothing to handle for it here.
    }
}

const SHUTDOWN_REQUEST: &[u8] = b"shutdown";

fn handle_device_added(subscription_handle: Handle, device: &mut Option<DeviceState>) {
    if device.is_some() {
        return;
    }

    loop {
        let event = match device::device_poll_event(subscription_handle) {
            Ok(Some(event)) => event,
            Ok(None) => return,
            Err(_) => {
                environment::log("virtio-gpu: poll_event failed");
                return;
            }
        };

        if event.token == 0 {
            continue;
        }

        match init_device(event.token.into()) {
            Ok(gpu) => {
                environment::log("virtio-gpu: device claimed and initialised");
                *device = Some(DeviceState { gpu, opened: false });
                return;
            }
            Err(_) => {
                environment::log("virtio-gpu: device init failed");
                return;
            }
        }
    }
}

fn register_scheme() -> Option<SchemeProvider> {
    match SchemeProvider::register("display") {
        Ok(provider) => {
            environment::log("virtio-gpu: registered display: scheme");
            Some(provider)
        }
        Err(_) => {
            environment::log("virtio-gpu: scheme registration failed");
            None
        }
    }
}

/// Drain and answer every request currently queued on the provider channel.
fn serve_provider_requests(provider: &SchemeProvider, mut device: Option<&mut DeviceState>) {
    let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];
    loop {
        let request = match provider.try_recv(&mut buf) {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(_) => return,
        };

        match request {
            Request::Open { request_id, .. } => {
                let Some(dev) = device.as_deref_mut() else {
                    let _ = provider.reply_open_err(request_id, ErrorCode::NotFound);
                    continue;
                };
                if dev.opened {
                    let _ = provider.reply_open_err(request_id, ErrorCode::Busy);
                } else {
                    dev.opened = true;
                    let _ = provider.reply_open_ok(request_id, DISPLAY_RESOURCE_ID);
                }
            }
            Request::Readdir { request_id, .. } => {
                let _ = provider.reply_readdir_err(request_id, ErrorCode::NotSupported);
            }
            Request::Read { request_id, .. } => {
                let _ = provider.reply_read_err(request_id, ErrorCode::NotSupported);
            }
            Request::Write { request_id, .. } => {
                let _ = provider.reply_write_err(request_id, ErrorCode::NotWritable);
            }
            Request::Close { request_id, .. } => {
                if let Some(dev) = device.as_deref_mut() {
                    dev.opened = false;
                }
                let _ = provider.reply_close_ok(request_id);
            }
            Request::Connect { request_id, .. } => {
                let _ = provider.reply_connect_err(request_id, ErrorCode::NotSupported);
            }
            // `display:` never serves `OP_SIZE` — that request kind (M4's
            // block driver service) only arrives on a `block:` provider
            // (`userspace/drivers/virtio-blk`).
            Request::Size { request_id, .. } => {
                let _ = provider.reply_size_err(request_id, ErrorCode::NotSupported);
            }
            Request::DisplayInfo { request_id, .. } => {
                let Some(dev) = device.as_deref_mut() else {
                    let _ = provider.reply_display_info_err(request_id, ErrorCode::InvalidHandle);
                    continue;
                };
                let _ = provider.reply_display_info_ok(
                    request_id,
                    dev.gpu.width,
                    dev.gpu.height,
                    PIXEL_FORMAT_ARGB8888,
                    dev.gpu.width * 4,
                );
            }
            Request::DisplayMap { request_id, .. } => {
                let Some(dev) = device.as_deref_mut() else {
                    let _ = provider.reply_display_map_err(request_id, ErrorCode::InvalidHandle);
                    continue;
                };
                let size = (dev.gpu.width as u64) * (dev.gpu.height as u64) * 4;
                environment::log(&alloc::format!(
                    "virtio-gpu: DEBUG DisplayMap iova={:#x} size={:#x} w={} h={}",
                    dev.gpu.framebuffer_iova, size, dev.gpu.width, dev.gpu.height
                ));
                let _ = provider.reply_display_map_ok(request_id, dev.gpu.framebuffer_iova, size);
            }
            Request::DisplayFlush { request_id, rect, .. } => {
                let Some(dev) = device.as_deref_mut() else {
                    let _ = provider.reply_display_flush_err(request_id, ErrorCode::InvalidHandle);
                    continue;
                };
                match dev.gpu.flush(rect) {
                    Ok(()) => {
                        let _ = provider.reply_display_flush_ok(request_id);
                    }
                    Err(err) => {
                        let _ = provider.reply_display_flush_err(request_id, err);
                    }
                }
            }
        }
    }
}
