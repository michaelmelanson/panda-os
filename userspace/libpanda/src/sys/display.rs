//! Low-level display operations.
//!
//! These act on a handle opened from the `display:` scheme
//! (`display:/pci/display/0`), which is exclusively claimed: the open itself
//! fails with `Busy` if another process — or, until the compositor moves to
//! userspace, the in-kernel compositor — already owns the display.

use super::{Handle, send};
use panda_abi::*;

/// Get the display's mode info (width, height, format, stride).
///
/// Returns 0 on success, or a negative error code.
#[inline(always)]
pub fn info(handle: Handle, info: &mut SurfaceInfoOut) -> isize {
    send(
        handle,
        OP_DISPLAY_INFO,
        info as *mut SurfaceInfoOut as usize,
        0,
        0,
        0,
    )
}

/// Map the display's framebuffer into this process.
///
/// Returns the virtual address of the mapping, or a negative error code.
#[inline(always)]
pub fn map(handle: Handle) -> isize {
    send(handle, OP_DISPLAY_MAP, 0, 0, 0, 0)
}

/// Flush a damaged rectangle to the display.
///
/// If `rect` is `None`, the whole screen is flushed.
/// Returns 0 on success, or a negative error code.
#[inline(always)]
pub fn flush(handle: Handle, rect: Option<&SurfaceRect>) -> isize {
    let rect_ptr = match rect {
        Some(r) => r as *const SurfaceRect as usize,
        None => 0,
    };
    send(handle, OP_DISPLAY_FLUSH, rect_ptr, 0, 0, 0)
}
