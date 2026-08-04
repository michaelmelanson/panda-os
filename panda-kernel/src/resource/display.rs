//! The display resource: exclusive ownership of a display device.
//!
//! This is the kernel side of the `display:` scheme
//! (`display:/pci/display/0`), the permanent interface a compositor uses to
//! drive the screen — see plans/userspace-compositor.md, "The display
//! resource". Opening it claims the display device exclusively via
//! [`crate::devices::claims`]; a second open (from anywhere, including
//! `surface:/fb0`) fails with `Busy` until the owning handle is closed or the
//! owning process exits.
//!
//! Because holding a [`DisplayDevice`] handle *is* the proof of exclusive
//! ownership, the three operations below apply no further permission check: a
//! process that does not own the display simply has no handle to send them
//! to.
//!
//! In this milestone the provider is kernel plumbing in front of the
//! virtio-gpu driver. In roadmap M4 the provider becomes a userspace display
//! driver service registering the same interface, and this file goes away
//! unchanged from the client's point of view.

use spinning_top::Spinlock;
use x86_64::VirtAddr;

use crate::device_address::DeviceAddress;
use crate::devices::claims::ClaimGuard;
use crate::memory::{MemoryMappingOptions, map_external, virtual_address_to_physical};
use crate::pci::{self, DeviceClass};
use crate::process::Process;

use super::surface::{Rect, SurfaceError, SurfaceInfo};
use super::{MailboxRef, Resource};

/// The device address of the system's display, if one exists.
///
/// Single source of truth for "which device is *the* display": the
/// `display:` scheme, the legacy `surface:/fb0` path, and the in-kernel
/// compositor's own claim all resolve the display through this function, so
/// they can never end up claiming different addresses and silently
/// coexisting.
pub fn device_address() -> Option<DeviceAddress> {
    pci::get_device_by_class(DeviceClass::Display.code(), 0)
}

/// Errors that can occur while mapping the framebuffer into a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayError {
    /// No virtual address space was available for the mapping.
    MappingFailed,
}

/// An exclusively-owned display device.
///
/// Holds the device's [`ClaimGuard`] for the lifetime of the resource, so
/// `close()` — or dropping the owner's handle table at process exit —
/// releases the display with no dedicated cleanup path.
pub struct DisplayDevice {
    /// Kernel virtual base address of the framebuffer pages.
    framebuffer: VirtAddr,
    info: SurfaceInfo,
    _claim: ClaimGuard,
    /// Mailbox to notify on `EVENT_DISPLAY_CHANGED`, if the owner attached one.
    mailbox: Spinlock<Option<MailboxRef>>,
}

/// The mailbox of the current display owner, for posting
/// `EVENT_DISPLAY_CHANGED` from the driver (which has no access to the
/// owner's handle table).
///
/// There is at most one display owner at a time by construction — that is
/// what the claim table guarantees — so a single global slot is sufficient
/// and cannot be ambiguous. It is cleared when the owning resource drops.
static OWNER_MAILBOX: Spinlock<Option<MailboxRef>> = Spinlock::new(None);

impl DisplayDevice {
    /// Create a display resource for the framebuffer described by the
    /// current global framebuffer region, taking ownership of `claim`.
    ///
    /// Returns `None` if no framebuffer has been initialized (no display
    /// driver bound), in which case the caller should report `NotFound` and
    /// let `claim` drop, releasing the claim it took.
    pub fn new(claim: ClaimGuard) -> Option<Self> {
        let (framebuffer, info) = super::surface::framebuffer_region()?;
        Some(Self {
            framebuffer,
            info,
            _claim: claim,
            mailbox: Spinlock::new(None),
        })
    }

    /// Mode information for `OP_DISPLAY_INFO`.
    pub fn info(&self) -> SurfaceInfo {
        self.info
    }

    /// Size of the framebuffer in bytes, rounded up to whole pages.
    fn mapped_size(&self) -> usize {
        let bytes = self.info.stride as usize * self.info.height as usize;
        bytes.div_ceil(4096) * 4096
    }

    /// Map the framebuffer into `process` (which must be the CURRENT
    /// process, since this installs page-table entries directly) and return
    /// the userspace virtual address.
    ///
    /// The framebuffer is a physically contiguous DMA region owned by the
    /// GPU driver for the lifetime of the device, not a frame-tracked
    /// [`crate::resource::SharedBuffer`], so this maps it with the same
    /// low-level `map_external` primitive `SharedBuffer::map_into_process`
    /// uses but with `Mmio` backing: unmapping tears down this process's
    /// page-table entries and never touches the underlying memory. The
    /// resulting [`crate::memory::Mapping`] is registered with the process,
    /// so the mapping is removed automatically at process exit.
    ///
    /// Mode changes are the one case that can invalidate the mapping: the
    /// driver replaces the framebuffer allocation, and the owner is told to
    /// re-map via `EVENT_DISPLAY_CHANGED` (see [`notify_display_changed`]).
    /// Until it does, its stale mapping still points at memory the GPU
    /// driver has released — the "keep the old pages alive until the owner
    /// unmaps" refinement in plans/userspace-compositor.md (risk 5) needs
    /// frame ownership to move out of the virtio HAL and is not implemented
    /// here.
    pub fn map_into_process(&self, process: &mut Process) -> Result<usize, DisplayError> {
        let size = self.mapped_size();
        let num_pages = size / 4096;

        let vaddr = process
            .alloc_buffer_vaddr(num_pages)
            .ok_or(DisplayError::MappingFailed)?;

        let phys = virtual_address_to_physical(self.framebuffer);
        let mapping = map_external(
            phys,
            vaddr,
            size,
            MemoryMappingOptions {
                user: true,
                executable: false,
                writable: true,
            },
        );
        process.add_mapping(mapping);

        Ok(vaddr.as_u64() as usize)
    }

    /// Forward a damaged rectangle to the driver.
    ///
    /// `region` is validated against the display bounds and then handed to
    /// the driver. The virtio-gpu transfer+flush the driver performs is
    /// synchronous and whole-surface (the underlying driver exposes no
    /// partial-rect flush), so the rectangle currently serves as validation
    /// and future-proofing of the interface rather than as a bandwidth
    /// optimisation.
    pub fn flush(&self, region: Option<Rect>) -> Result<(), SurfaceError> {
        if let Some(rect) = region {
            let within = rect
                .x
                .checked_add(rect.width)
                .zip(rect.y.checked_add(rect.height))
                .map(|(right, bottom)| right <= self.info.width && bottom <= self.info.height)
                .unwrap_or(false);
            if !within {
                return Err(SurfaceError::InvalidBounds);
            }
        }

        crate::devices::virtio_gpu::flush_framebuffer();
        Ok(())
    }
}

impl Drop for DisplayDevice {
    fn drop(&mut self) {
        *OWNER_MAILBOX.lock() = None;
    }
}

impl Resource for DisplayDevice {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Display
    }

    fn as_display(&self) -> Option<&DisplayDevice> {
        Some(self)
    }

    fn supported_events(&self) -> u32 {
        panda_abi::EVENT_DISPLAY_CHANGED
    }

    fn attach_mailbox(&self, mailbox_ref: MailboxRef) {
        *self.mailbox.lock() = Some(mailbox_ref.clone());
        *OWNER_MAILBOX.lock() = Some(mailbox_ref);
    }
}

/// Post `EVENT_DISPLAY_CHANGED` to the display owner's mailbox, if any.
///
/// Called by the display driver after a successful mode change; the owner
/// responds by re-querying `OP_DISPLAY_INFO` and re-issuing `OP_DISPLAY_MAP`.
pub fn notify_display_changed() {
    if let Some(mailbox) = OWNER_MAILBOX.lock().as_ref() {
        mailbox.post_event(panda_abi::EVENT_DISPLAY_CHANGED);
    }
}
