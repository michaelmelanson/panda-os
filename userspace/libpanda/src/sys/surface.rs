//! Low-level surface operations.
//!
//! These functions provide direct syscall access for graphics operations.
//! For higher-level abstractions, use `crate::graphics::Surface`.

use super::{Handle, send};
use panda_abi::*;

/// Get surface information.
///
/// Writes surface dimensions and format to `info`.
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn info(handle: Handle, info: &mut SurfaceInfoOut) -> isize {
    send(
        handle,
        OP_SURFACE_INFO,
        info as *mut SurfaceInfoOut as usize,
        0,
        0,
        0,
    )
}

/// Blit pixels from a buffer to a surface.
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn blit(handle: Handle, params: &BlitParams) -> isize {
    send(
        handle,
        OP_SURFACE_BLIT,
        params as *const BlitParams as usize,
        0,
        0,
        0,
    )
}

/// Fill a rectangle with a solid colour.
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn fill(handle: Handle, params: &FillParams) -> isize {
    send(
        handle,
        OP_SURFACE_FILL,
        params as *const FillParams as usize,
        0,
        0,
        0,
    )
}

/// Flush surface updates to the display.
///
/// If `rect` is `None`, flushes the entire surface.
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn flush(handle: Handle, rect: Option<&SurfaceRect>) -> isize {
    let rect_ptr = match rect {
        Some(r) => r as *const SurfaceRect as usize,
        None => 0,
    };
    send(handle, OP_SURFACE_FLUSH, rect_ptr, 0, 0, 0)
}

/// Update surface/window parameters (position, size, visibility).
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn update_params(handle: Handle, params: &UpdateParamsIn) -> isize {
    send(
        handle,
        OP_SURFACE_UPDATE_PARAMS,
        params as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    )
}
