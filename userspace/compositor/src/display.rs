//! The compositing target: the framebuffer mapped from the `display:`
//! scheme.

use compositor_protocol::Rect;
use libpanda::mailbox::Mailbox;
use libpanda::{Handle, environment, sys};
use panda_abi::{ErrorCode, SurfaceInfoOut, SurfaceRect};

use crate::target::Target;

/// The exclusively-claimed display the compositor presents to.
pub const DISPLAY_PATH: &str = "display:/pci/display/0";

// `init` (`userspace/init/src/main.rs`) spawns the userspace virtio-gpu
// driver and waits for its readiness handshake (`libpanda::process::
// wait_ready`) before spawning the compositor, which closes the startup
// race against `display:` registration at the source.
// `Framebuffer::open`'s first attempt should therefore always succeed when
// spawned by `init` in the normal boot path.
//
// `environment::open_retrying` here is defense-in-depth for anything that
// isn't true of that path — e.g. a `Busy` while the driver is definitely
// up already (a real second claimant, which just delays an
// already-expected failure rather than changing the outcome) — not the
// mechanism the race is actually closed by. See
// `libpanda::environment::open_retrying`'s doc comment.

/// A mapped framebuffer.
pub struct Framebuffer {
    handle: Handle,
    /// Subscribed to `EVENT_DISPLAY_CHANGED` on `handle` — see
    /// [`Framebuffer::poll_resize`].
    mailbox: Mailbox,
    pixels: *mut u8,
    width: u32,
    height: u32,
    stride: u32,
}

impl Framebuffer {
    /// Claim the display, query its mode and map its framebuffer.
    ///
    /// Subscribes a fresh mailbox to `EVENT_DISPLAY_CHANGED` on the same
    /// handle, so [`Framebuffer::poll_resize`] can pick up later mode
    /// changes — see `docs/IPC.md`'s "Display events".
    pub fn open() -> Result<Self, ErrorCode> {
        let mailbox = Mailbox::create()?;
        let handle = environment::open_retrying(
            DISPLAY_PATH,
            mailbox.handle().as_raw(),
            panda_abi::EVENT_DISPLAY_CHANGED,
        )?;

        let mut info = SurfaceInfoOut {
            width: 0,
            height: 0,
            format: 0,
            stride: 0,
        };
        if sys::display::info(handle, &mut info) < 0 {
            let _ = sys::file::close(handle);
            return Err(ErrorCode::IoError);
        }

        let mapped = sys::display::map(handle);
        if mapped < 0 {
            let _ = sys::file::close(handle);
            return Err(ErrorCode::IoError);
        }

        Ok(Self {
            handle,
            mailbox,
            pixels: mapped as *mut u8,
            width: info.width,
            height: info.height,
            stride: info.stride,
        })
    }

    fn offset(&self, x: u32, y: u32) -> isize {
        (y * self.stride + x * 4) as isize
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        let _ = sys::file::close(self.handle);
    }
}

impl Target for Framebuffer {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn fill(&mut self, rect: &Rect, colour: u32) {
        let bytes = colour.to_le_bytes();
        for y in rect.y..(rect.y + rect.height).min(self.height) {
            for x in rect.x..(rect.x + rect.width).min(self.width) {
                // SAFETY: x/y are clipped to the mapped framebuffer.
                unsafe {
                    let ptr = self.pixels.offset(self.offset(x, y));
                    core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, 4);
                }
            }
        }
    }

    fn write_row(&mut self, x: u32, y: u32, width: u32, src: &[u8]) {
        if y >= self.height || x >= self.width {
            return;
        }
        let width = width.min(self.width - x);
        let bytes = width as usize * 4;
        if src.len() < bytes {
            return;
        }
        // SAFETY: the destination row is clipped to the mapped framebuffer
        // and `src` has at least `bytes` bytes.
        unsafe {
            let ptr = self.pixels.offset(self.offset(x, y));
            core::ptr::copy_nonoverlapping(src.as_ptr(), ptr, bytes);
        }
    }

    fn get_pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        // SAFETY: bounds-checked above.
        unsafe {
            let ptr = self.pixels.offset(self.offset(x, y));
            [*ptr, *ptr.offset(1), *ptr.offset(2), *ptr.offset(3)]
        }
    }

    fn set_pixel(&mut self, x: u32, y: u32, pixel: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        // SAFETY: bounds-checked above.
        unsafe {
            let ptr = self.pixels.offset(self.offset(x, y));
            core::ptr::copy_nonoverlapping(pixel.as_ptr(), ptr, 4);
        }
    }

    fn flush(&mut self, rect: &Rect) {
        let rect = SurfaceRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        };
        sys::display::flush(self.handle, Some(&rect));
    }

    /// Drain the display's mailbox and re-map the framebuffer on
    /// `EVENT_DISPLAY_CHANGED`.
    ///
    /// `OP_DISPLAY_MAP` has no counterpart unmap operation — the mapping is
    /// MMIO-style and torn down only on process exit (see
    /// `docs/SYSCALLS.md`'s `OP_DISPLAY_MAP` entry) — so a remap leaves the
    /// old mapping's virtual address range behind rather than freeing it.
    /// That's a small, bounded leak per mode change (mode changes are rare,
    /// typically at most once per boot), not an unbounded one.
    fn poll_resize(&mut self) -> bool {
        let mut resized = false;
        while let Some((handle, events)) = self.mailbox.try_recv() {
            if handle.as_raw() != self.handle.as_raw() || !events.is_display_changed() {
                continue;
            }

            let mut info = SurfaceInfoOut {
                width: 0,
                height: 0,
                format: 0,
                stride: 0,
            };
            if sys::display::info(self.handle, &mut info) < 0 {
                continue;
            }

            let mapped = sys::display::map(self.handle);
            if mapped < 0 {
                continue;
            }

            self.pixels = mapped as *mut u8;
            self.width = info.width;
            self.height = info.height;
            self.stride = info.stride;
            resized = true;
        }
        resized
    }
}
