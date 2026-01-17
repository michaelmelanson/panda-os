//! Surface syscall handlers.

use log::debug;

use crate::scheduler;

/// Handle OP_SURFACE_INFO syscall.
///
/// Gets surface dimensions and pixel format.
///
/// Arguments:
/// - handle: Surface handle
/// - info_ptr: Pointer to SurfaceInfoOut struct to fill
///
/// Returns:
/// - 0 on success
/// - negative error code on failure
pub fn handle_info(handle: u32, info_ptr: usize) -> isize {
    debug!("handle_info: handle={}, info_ptr={:#x}", handle, info_ptr);

    if info_ptr == 0 {
        return -1;
    }

    scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return -1;
        };

        let Some(surface) = resource.as_surface() else {
            return -1;
        };

        let info = surface.info();

        unsafe {
            let out = info_ptr as *mut panda_abi::SurfaceInfoOut;
            (*out).width = info.width;
            (*out).height = info.height;
            (*out).format = info.format as u32;
            (*out).stride = info.stride;
        }

        0
    })
}

/// Handle OP_SURFACE_BLIT syscall.
///
/// Blit pixels from a buffer to the surface.
///
/// Arguments:
/// - handle: Surface handle
/// - params_ptr: Pointer to BlitParams struct
///
/// Returns:
/// - 0 on success
/// - negative error code on failure
pub fn handle_blit(handle: u32, params_ptr: usize) -> isize {
    debug!("handle_blit: handle={}, params_ptr={:#x}", handle, params_ptr);

    if params_ptr == 0 {
        return -1;
    }

    let params = unsafe { *(params_ptr as *const panda_abi::BlitParams) };

    scheduler::with_current_process(|proc| {
        // Get buffer data first (immutable borrow)
        let buffer_slice_ptr = {
            let Some(buffer_resource) = proc.handles().get(params.buffer_handle) else {
                return -1;
            };
            let Some(buffer) = buffer_resource.as_buffer() else {
                return -1;
            };
            buffer.as_slice().as_ptr()
        };

        // Now get mutable surface (mutable borrow, after immutable is dropped)
        let Some(resource) = proc.handles_mut().get_mut(handle) else {
            return -1;
        };

        let Some(surface) = resource.as_surface_mut() else {
            return -1;
        };

        // Calculate expected buffer size
        let expected_size = (params.width * params.height * 4) as usize;

        // Reconstruct slice from pointer - safe because we know buffer hasn't moved
        let buffer_slice = unsafe { core::slice::from_raw_parts(buffer_slice_ptr, expected_size) };

        // Blit from the buffer to the surface
        match surface.blit(params.x, params.y, params.width, params.height, buffer_slice) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
}

/// Handle OP_SURFACE_FILL syscall.
///
/// Fill a rectangle with a solid color.
///
/// Arguments:
/// - handle: Surface handle
/// - params_ptr: Pointer to FillParams struct
///
/// Returns:
/// - 0 on success
/// - negative error code on failure
pub fn handle_fill(handle: u32, params_ptr: usize) -> isize {
    debug!("handle_fill: handle={}, params_ptr={:#x}", handle, params_ptr);

    if params_ptr == 0 {
        return -1;
    }

    let params = unsafe { *(params_ptr as *const panda_abi::FillParams) };

    scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles_mut().get_mut(handle) else {
            return -1;
        };

        let Some(surface) = resource.as_surface_mut() else {
            return -1;
        };

        match surface.fill(params.x, params.y, params.width, params.height, params.color) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
}

/// Handle OP_SURFACE_FLUSH syscall.
///
/// Flush surface updates to the display.
///
/// Arguments:
/// - handle: Surface handle
/// - rect_ptr: Optional pointer to SurfaceRect (0 for full flush)
///
/// Returns:
/// - 0 on success
/// - negative error code on failure
pub fn handle_flush(handle: u32, rect_ptr: usize) -> isize {
    debug!("handle_flush: handle={}, rect_ptr={:#x}", handle, rect_ptr);

    scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles_mut().get_mut(handle) else {
            return -1;
        };

        let Some(surface) = resource.as_surface_mut() else {
            return -1;
        };

        let region = if rect_ptr != 0 {
            let rect = unsafe { *(rect_ptr as *const panda_abi::SurfaceRect) };
            Some(crate::resource::Rect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
        } else {
            None
        };

        match surface.flush(region) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
}
