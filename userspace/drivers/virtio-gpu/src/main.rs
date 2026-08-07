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
//! # Virtio transport: hand-rolled, not `virtio_drivers`
//!
//! Same rationale as `userspace/drivers/virtio-keyboard` (see that module's
//! doc comment in full): the kernel-only `virtio_drivers` crate assumes
//! direct physical memory access, which a userspace process doesn't have.
//! This driver re-derives the virtio-pci modern-transport plumbing (PCI
//! capability walk, feature negotiation, split virtqueue) by hand, against
//! the `OP_DEVICE_*`/`OP_DMA_*` syscalls, duplicating
//! `userspace/drivers/virtio-keyboard`'s equivalent code rather than
//! factoring a shared crate (acceptable given time constraints — see
//! `plans/device-driver-model.md`'s M4 landed-deviations note for this as
//! tracked follow-up debt).
//!
//! # Only the control queue; no cursor queue, no IRQ
//!
//! virtio-gpu exposes two virtqueues: controlq (index 0, 2D/3D resource and
//! scanout commands) and cursorq (index 1, hardware cursor updates). Only
//! controlq is set up — this driver never draws a hardware cursor, matching
//! the deleted kernel driver's scope (it only ever called
//! `change_resolution`/`flush`, both controlq-only operations).
//!
//! Every controlq request in this driver is synchronous request/response
//! (one descriptor chain in flight at a time, like
//! `virtio_drivers::device::gpu::VirtIOGpu::request`), and completion is
//! detected by polling the used ring directly rather than waiting for an
//! MSI-X interrupt: reading the DMA'd used-ring memory is valid regardless
//! of whether an interrupt was delivered, and a synchronous poll loop is
//! simpler than wiring an IRQ mailbox for a queue that is never used
//! asynchronously. This sidesteps `DeviceResource::subscribe_irq`'s
//! single-MSI-X-vector limit entirely rather than needing to reason about
//! it, since basic 2D scanout needs no interrupt delivery at all.
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

use libpanda::device::{self, BusType, EVENT_DEVICE_ADDED, EVENT_DEVICE_REMOVED, MmioRegion, PciDeviceId};
use libpanda::error::Result;
use libpanda::mailbox::Mailbox;
use libpanda::scheme::SchemeProvider;
use libpanda::{Handle, environment, process};
use panda_abi::ErrorCode;
use panda_abi::scheme_protocol::Request;

type DeviceHandle = Handle;

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
// PCI config space / virtio-pci capability walk — identical in structure to
// `userspace/drivers/virtio-keyboard`'s copy; see that module for the
// virtio-v1.1 spec citations.
// =============================================================================

const PCI_STATUS_OFFSET: u32 = 0x06;
const PCI_STATUS_CAP_LIST: u32 = 1 << 4;
const PCI_CAPABILITIES_POINTER_OFFSET: u32 = 0x34;
const PCI_CAP_ID_VNDR: u8 = 0x09;

const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;

#[derive(Clone, Copy)]
struct VirtioCap {
    bar: u8,
    offset: u32,
    #[allow(dead_code)]
    length: u32,
    notify_off_multiplier: u32,
}

fn find_virtio_caps(device_handle: DeviceHandle) -> Result<(VirtioCap, VirtioCap)> {
    let status = device::device_pci_config_read(device_handle, PCI_STATUS_OFFSET, 2)?;
    if status & PCI_STATUS_CAP_LIST == 0 {
        return Err(ErrorCode::NotSupported);
    }

    let mut common: Option<VirtioCap> = None;
    let mut notify: Option<VirtioCap> = None;

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
// Virtio device status/feature negotiation.
// =============================================================================

const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;

const VIRTIO_F_VERSION_1_HIGH_BIT: u32 = 1 << 0; // bit 32 overall = bit 0 of the high dword

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

const CONTROLQ_INDEX: u16 = 0;
/// No MSI-X vector is ever subscribed (see the module doc comment), so the
/// queue is left pointing at the "no vector" sentinel (virtio-v1.1
/// 4.1.4.3.2).
const MSIX_VECTOR_NONE: u16 = 0xFFFF;

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
// Split virtqueue: a small, fixed-size controlq used for one synchronous
// request/response at a time (mirrors
// `virtio_drivers::device::gpu::VirtIOGpu::request`, which also serialises
// on one in-flight descriptor chain).
// =============================================================================

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const DESC_ENTRY_LEN: usize = 16;
const USED_ENTRY_LEN: usize = 8;

/// Fixed queue size: only one request is ever in flight, so two descriptors
/// (send + recv) is enough; virtqueue sizes must be a power of two.
const QSIZE: u16 = 2;
/// Send/recv buffer size: comfortably larger than the largest command this
/// driver issues (`RESOURCE_ATTACH_BACKING` at 56 bytes) or response it
/// reads (`RESP_DISPLAY_INFO` at 32 bytes).
const CMD_BUF_LEN: usize = 256;

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

struct QueueLayout {
    desc_off: usize,
    avail_off: usize,
    used_off: usize,
    send_off: usize,
    recv_off: usize,
    total_len: usize,
}

impl QueueLayout {
    fn compute() -> Self {
        let qsize = QSIZE as usize;
        let desc_off = 0;
        let avail_off = desc_off + DESC_ENTRY_LEN * qsize;
        let avail_len = 4 + 2 * qsize;
        let used_off = align_up(avail_off + avail_len, 4);
        let used_len = 4 + USED_ENTRY_LEN * qsize;
        let send_off = align_up(used_off + used_len, 8);
        let recv_off = send_off + CMD_BUF_LEN;
        let total_len = recv_off + CMD_BUF_LEN;
        Self { desc_off, avail_off, used_off, send_off, recv_off, total_len }
    }

    fn desc_addr_off(&self, i: u16) -> usize {
        self.desc_off + DESC_ENTRY_LEN * i as usize
    }

    fn avail_ring_off(&self, i: u16) -> usize {
        self.avail_off + 4 + 2 * (i % QSIZE) as usize
    }

    fn used_ring_off(&self, i: u16) -> usize {
        self.used_off + 4 + USED_ENTRY_LEN * (i % QSIZE) as usize
    }
}

struct ControlQueue {
    ring: MmioRegion,
    #[allow(dead_code)]
    virt_addr: usize,
    #[allow(dead_code)]
    dma_size: usize,
    layout: QueueLayout,
    send_iova: u64,
    recv_iova: u64,
    queue_notify_off: u16,
    /// Next avail-ring slot / used-ring slot to consume — both advance by
    /// one per request, since exactly one descriptor chain (head index 0)
    /// is reused for every request.
    next_idx: u16,
}

impl ControlQueue {
    /// Publish a descriptor chain for `cmd` (already encoded into the send
    /// buffer via [`Self::write_send`]) onto the avail ring. Returns the
    /// avail/used slot this request occupies, to pass to
    /// [`Self::wait_for_response`] — split from that method (rather than one
    /// combined `request`) so the caller can ring the notify doorbell
    /// (a separate `MmioRegion`, possibly a distinct struct field) in
    /// between without holding two conflicting borrows of `self` at once.
    fn prepare(&mut self, cmd_len: usize) -> u16 {
        let head = 0u16;
        let desc0 = self.layout.desc_addr_off(head);
        self.ring.write::<u64>(desc0, self.send_iova);
        self.ring.write::<u32>(desc0 + 8, cmd_len as u32);
        self.ring.write::<u16>(desc0 + 12, VIRTQ_DESC_F_NEXT);
        self.ring.write::<u16>(desc0 + 14, 1);

        let desc1 = self.layout.desc_addr_off(1);
        self.ring.write::<u64>(desc1, self.recv_iova);
        self.ring.write::<u32>(desc1 + 8, CMD_BUF_LEN as u32);
        self.ring.write::<u16>(desc1 + 12, VIRTQ_DESC_F_WRITE);
        self.ring.write::<u16>(desc1 + 14, 0);

        let avail_slot = self.next_idx;
        self.ring.write::<u16>(self.layout.avail_ring_off(avail_slot), head);
        let next_avail = avail_slot.wrapping_add(1);
        self.ring.write::<u16>(self.layout.avail_off, 0); // flags
        self.ring.write::<u16>(self.layout.avail_off + 2, next_avail);
        avail_slot
    }

    /// Ring the controlq's doorbell: a 16-bit write of the queue index to
    /// the notify capability's BAR at `notify_off` (already includes
    /// `queue_notify_off * notify_off_multiplier` — see `Gpu::request`).
    fn kick(&self, notify: &MmioRegion, notify_off: usize) {
        notify.write::<u16>(notify_off, CONTROLQ_INDEX);
    }

    /// Busy-poll the used ring until `avail_slot`'s request completes (see
    /// the module doc comment for why this driver never waits on an IRQ
    /// mailbox for controlq requests), then return the number of bytes the
    /// device wrote into the recv buffer. Bounded so a wedged device fails
    /// loudly instead of hanging this process forever.
    ///
    /// QEMU's virtio-gpu backend processes a controlq request essentially
    /// immediately (no real DMA latency to wait out), so the common case is
    /// a handful of pure in-memory ring reads — `process::yield_now()` on
    /// *every* spin (as an earlier version of this loop did) turned each of
    /// those into a syscall/context-switch round trip regardless, which is
    /// cheap in isolation but adds up: under host CPU contention (several
    /// QEMU instances running at once, e.g. `make test`'s default
    /// parallelism) a scheduler round trip can take much longer than the
    /// completion actually takes to post, and every screenshot-verified
    /// compositor test does several flushes per test — enough added
    /// wall-clock latency to blow `compositor_test_child`'s fixed tick
    /// budget (see that binary's module doc comment) and TIMEOUT even
    /// though nothing was actually stuck. Only yield every
    /// `YIELD_INTERVAL` iterations; spin on a cheap CPU hint the rest of
    /// the time, so the fast (normal) case never leaves this process's own
    /// time slice at all.
    fn wait_for_response(&mut self, avail_slot: u16) -> usize {
        const YIELD_INTERVAL: u32 = 256;
        let target_used = avail_slot.wrapping_add(1);
        let mut spins: u32 = 0;
        loop {
            let used_idx = self.ring.read::<u16>(self.layout.used_off + 2);
            if used_idx == target_used {
                break;
            }
            spins += 1;
            if spins > 20_000_000 {
                break;
            }
            if spins % YIELD_INTERVAL == 0 {
                process::yield_now();
            } else {
                core::hint::spin_loop();
            }
        }
        self.next_idx = target_used;

        let used_entry_off = self.layout.used_ring_off(avail_slot);
        self.ring.read::<u32>(used_entry_off + 4) as usize
    }

    fn send_buf(&self, len: usize) -> alloc::vec::Vec<u8> {
        alloc::vec::Vec::with_capacity(len)
    }

    fn write_send(&self, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            self.ring.write::<u8>(self.layout.send_off + i, *b);
        }
    }

    fn read_recv(&self, len: usize) -> alloc::vec::Vec<u8> {
        (0..len).map(|i| self.ring.read::<u8>(self.layout.recv_off + i)).collect()
    }
}

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
    queue: ControlQueue,
    /// The notify capability's offset within whichever `MmioRegion` this
    /// driver ends up writing the doorbell through (already includes
    /// `queue_notify_off * notify_off_multiplier` — see `init_device`).
    notify_off: usize,
    /// Kept alive so the common-config mapping isn't unmapped early; its
    /// registers aren't touched again after `init_device`.
    #[allow(dead_code)]
    common_mmio: MmioRegion,
    /// `Some` only when the notify capability lives in a BAR distinct from
    /// common config — see [`Self::notify_region`].
    notify_mmio: Option<MmioRegion>,
    #[allow(dead_code)]
    device_handle: DeviceHandle,
    width: u32,
    height: u32,
    framebuffer_iova: u64,
}

impl Gpu {
    fn request(&mut self, cmd_bytes: &[u8]) -> alloc::vec::Vec<u8> {
        self.queue.write_send(cmd_bytes);
        let avail_slot = self.queue.prepare(cmd_bytes.len());
        self.queue.kick(self.notify_mmio.as_ref().unwrap_or(&self.common_mmio), self.notify_off);
        let len = self.queue.wait_for_response(avail_slot);
        self.queue.read_recv(len)
    }

    fn get_display_info(&mut self) -> Result<DisplayInfo> {
        let mut buf = self.queue.send_buf(24);
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
        let mut buf = self.queue.send_buf(40);
        write_ctrl_header(&mut buf, cmd::RESOURCE_CREATE_2D);
        buf.extend_from_slice(&resource_id.to_le_bytes());
        buf.extend_from_slice(&FORMAT_B8G8R8A8_UNORM.to_le_bytes());
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        let resp = self.request(&buf);
        check_ok_nodata(&resp)
    }

    fn resource_attach_backing(&mut self, resource_id: u32, iova: u64, length: u32) -> Result<()> {
        let mut buf = self.queue.send_buf(56);
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
        let mut buf = self.queue.send_buf(40);
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
        let mut buf = self.queue.send_buf(56);
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
        let mut buf = self.queue.send_buf(40);
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

/// Claim the device, walk its virtio-pci capabilities, map the common
/// config / notify BARs, negotiate features, set up the controlq, fetch the
/// display's preferred resolution, allocate and attach a framebuffer, and
/// set the scanout. Mirrors `userspace/drivers/virtio-keyboard`'s
/// `init_device` structure.
fn init_device(token: Handle) -> Result<Gpu> {
    let device_handle: DeviceHandle = device::device_claim(token)?;

    let (common_cap, notify_cap) = find_virtio_caps(device_handle)?;

    let common_mmio = device::device_map_mmio(device_handle, common_cap.bar as u32)?;
    let notify_mmio = if notify_cap.bar == common_cap.bar {
        None
    } else {
        Some(device::device_map_mmio(device_handle, notify_cap.bar as u32)?)
    };

    negotiate_features(&common_mmio, common_cap.offset as usize)?;

    common_mmio.write::<u16>(common_cap.offset as usize + common_cfg::QUEUE_SELECT, CONTROLQ_INDEX);
    let max_size = common_mmio.read::<u16>(common_cap.offset as usize + common_cfg::QUEUE_SIZE);
    if max_size == 0 || max_size < QSIZE {
        return Err(ErrorCode::NotSupported);
    }

    let layout = QueueLayout::compute();
    let (virt_addr, iova) = device::dma_alloc(device_handle, layout.total_len)?;
    let ring = unsafe { MmioRegion::new(virt_addr as *mut u8, layout.total_len) };
    let send_iova = iova + layout.send_off as u64;
    let recv_iova = iova + layout.recv_off as u64;

    let desc_iova = iova + layout.desc_off as u64;
    let avail_iova = iova + layout.avail_off as u64;
    let used_iova = iova + layout.used_off as u64;
    let common_off = common_cap.offset as usize;
    // Tell the device the queue size this driver actually laid out
    // (`QSIZE`, not the device's own `max_size`) — without this write the
    // device keeps using its own (larger) default and reads/writes the
    // avail/used rings as if they were sized for that many entries,
    // overrunning this driver's much smaller DMA allocation and corrupting
    // adjacent memory (observed as QEMU's "Guest says index NNN is
    // available" once the first out-of-bounds avail.idx read returned
    // garbage). Per virtio-v1.1 4.1.4.3.2, queue_size may only be *shrunk*
    // from queue_size's reset value (`max_size`), which `QSIZE` always is.
    common_mmio.write::<u16>(common_off + common_cfg::QUEUE_SIZE, QSIZE);
    common_mmio.write::<u64>(common_off + common_cfg::QUEUE_DESC, desc_iova);
    common_mmio.write::<u64>(common_off + common_cfg::QUEUE_DRIVER, avail_iova);
    common_mmio.write::<u64>(common_off + common_cfg::QUEUE_DEVICE, used_iova);
    common_mmio.write::<u16>(common_off + common_cfg::QUEUE_MSIX_VECTOR, MSIX_VECTOR_NONE);
    let queue_notify_off = common_mmio.read::<u16>(common_off + common_cfg::QUEUE_NOTIFY_OFF);
    common_mmio.write::<u16>(common_off + common_cfg::QUEUE_ENABLE, 1);

    common_mmio.write::<u8>(
        common_off + common_cfg::DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    let queue = ControlQueue {
        ring,
        virt_addr,
        dma_size: layout.total_len,
        layout,
        send_iova,
        recv_iova,
        queue_notify_off,
        next_idx: 0,
    };

    let notify_off = notify_cap.offset as usize
        + queue.queue_notify_off as usize * notify_cap.notify_off_multiplier as usize;

    let mut gpu = Gpu {
        queue,
        notify_off,
        common_mmio,
        notify_mmio,
        device_handle,
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
        // `EVENT_DEVICE_IRQ` is never subscribed (see the module doc
        // comment), so it's never posted to this mailbox.
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
