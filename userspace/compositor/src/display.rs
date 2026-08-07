//! The compositing target: the framebuffer mapped from the `display:`
//! scheme.

use compositor_protocol::Rect;
use libpanda::{Handle, environment, process, sys};
use panda_abi::{ErrorCode, SurfaceInfoOut, SurfaceRect};

use crate::target::Target;

/// The exclusively-claimed display the compositor presents to.
pub const DISPLAY_PATH: &str = "display:/pci/display/0";

/// `init` (`userspace/init/src/main.rs`) spawns the userspace virtio-gpu
/// driver and then immediately spawns the compositor right after — but
/// spawning a process only starts it, it doesn't block until the driver
/// has actually claimed the GPU and called `OP_SCHEME_REGISTER` for
/// `display:`. Opening `display:/pci/display/0` before that registration
/// lands fails immediately (there's no scheme provider to answer yet), and
/// `Framebuffer::open` used to treat that first failure as final —
/// `Compositor::new` would catch it, log "running without output", and the
/// compositor would then run headless forever with no further retry,
/// silently losing the race under real multi-device boot timing (virtio-gpu
/// competing with virtio-blk/virtio-keyboard/virtio-mouse for scheduler
/// time makes the race far less favourable than any single test's
/// virtio-gpu-only boot). Retrying briefly instead of failing on the first
/// attempt closes that race, the same way `vfs::mount_ext2` already does
/// for the identical `block:` race against virtio-blk's startup (see
/// `MOUNT_RETRY_ATTEMPTS`/`MOUNT_RETRY_SLEEP_MS` in
/// `panda-kernel/src/vfs/mod.rs`) — same constants, same 2s total budget.
const DISPLAY_OPEN_RETRY_ATTEMPTS: u32 = 200;
const DISPLAY_OPEN_RETRY_SLEEP_MS: u64 = 10;

/// A mapped framebuffer.
pub struct Framebuffer {
    handle: Handle,
    pixels: *mut u8,
    width: u32,
    height: u32,
    stride: u32,
}

impl Framebuffer {
    /// Claim the display, query its mode and map its framebuffer.
    ///
    /// Retries the initial open for up to `DISPLAY_OPEN_RETRY_ATTEMPTS *
    /// DISPLAY_OPEN_RETRY_SLEEP_MS` (~2s) — see
    /// `DISPLAY_OPEN_RETRY_ATTEMPTS`'s doc comment for why: the very first
    /// attempt can lose a startup race against the virtio-gpu driver
    /// process still claiming the device and registering `display:`. A
    /// `Busy` after the driver is definitely up (a real second claimant)
    /// also retries here rather than failing fast — harmless, since it
    /// just delays an already-expected failure by up to the same ~2s
    /// instead of changing the outcome.
    pub fn open() -> Result<Self, ErrorCode> {
        let mut attempt = 0;
        let handle = loop {
            match environment::open(DISPLAY_PATH, 0, 0) {
                Ok(handle) => break handle,
                Err(_) if attempt < DISPLAY_OPEN_RETRY_ATTEMPTS => {
                    attempt += 1;
                    process::sleep(DISPLAY_OPEN_RETRY_SLEEP_MS);
                }
                Err(err) => return Err(err),
            }
        };

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
}
