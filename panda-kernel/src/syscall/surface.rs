//! Surface syscall handlers.

use log::debug;

use crate::resource::Buffer;
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
        // Check if this is a window resource
        let is_window = {
            let Some(resource) = proc.handles().get(handle) else {
                return -1;
            };
            let is_win = resource.as_window().is_some();
            debug!("handle_blit: is_window={}", is_win);
            is_win
        };

        if is_window {
            debug!("handle_blit: taking window path");
            // For windows, copy pixels from source buffer into window's pixel_data
            let source_buffer = {
                let Some(buffer_handle) = proc.handles().get(params.buffer_handle) else {
                    return -1;
                };
                let Some(buffer) = buffer_handle.resource_arc().as_shared_buffer() else {
                    return -1;
                };
                buffer
            };

            let Some(resource) = proc.handles().get(handle) else {
                return -1;
            };
            let Some(window_arc) = resource.as_window() else {
                return -1;
            };

            let window_id = {
                let mut window = window_arc.lock();

                // Copy pixels from source buffer into window's pixel_data at (x, y) offset
                let src_data = source_buffer.as_ref().as_slice();
                let expected_src_size = (params.width * params.height * 4) as usize;

                if src_data.len() < expected_src_size {
                    debug!("handle_blit: source buffer too small");
                    return -1;
                }

                let window_width = window.size.0;
                let window_height = window.size.1;

                // Bounds check
                if params.x + params.width > window_width
                    || params.y + params.height > window_height
                {
                    debug!("handle_blit: blit region out of bounds");
                    return -1;
                }

                // Copy row by row
                for row in 0..params.height {
                    let src_offset = (row * params.width * 4) as usize;
                    let dst_offset =
                        (((params.y + row) * window_width + params.x) * 4) as usize;
                    let row_bytes = (params.width * 4) as usize;

                    if dst_offset + row_bytes > window.pixel_data.len() {
                        debug!("handle_blit: destination overflow");
                        return -1;
                    }

                    window.pixel_data[dst_offset..dst_offset + row_bytes]
                        .copy_from_slice(&src_data[src_offset..src_offset + row_bytes]);
                }

                // Keep buffer reference for backwards compatibility
                window.content_buffer = Some(source_buffer);
                window.id
            };

            // Mark window dirty
            crate::compositor::mark_window_dirty(window_id);
            0
        } else {
            // For non-window surfaces, use traditional blit
            let buffer_slice_ptr = {
                let Some(buffer_resource) = proc.handles().get(params.buffer_handle) else {
                    return -1;
                };
                let Some(buffer) = buffer_resource.as_buffer() else {
                    return -1;
                };
                buffer.as_slice().as_ptr()
            };

            let Some(resource) = proc.handles().get(handle) else {
                return -1;
            };

            let Some(surface) = resource.as_surface() else {
                return -1;
            };

            // Calculate expected buffer size
            let expected_size = (params.width * params.height * 4) as usize;

            // Reconstruct slice from pointer - safe because we know buffer hasn't moved
            let buffer_slice =
                unsafe { core::slice::from_raw_parts(buffer_slice_ptr, expected_size) };

            // Blit from the buffer to the surface
            match surface.blit(params.x, params.y, params.width, params.height, buffer_slice) {
                Ok(()) => 0,
                Err(_) => -1,
            }
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
        let Some(resource) = proc.handles().get(handle) else {
            return -1;
        };

        let Some(surface) = resource.as_surface() else {
            return -1;
        };

        // Surface uses interior mutability, so we can call fill on &self
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
        // Check if this is a window resource
        let is_window = {
            let Some(resource) = proc.handles().get(handle) else {
                return -1;
            };
            let is_win = resource.as_window().is_some();
            debug!("handle_flush: is_window={}", is_win);
            is_win
        };

        if is_window {
            debug!("handle_flush: taking window path");
            // For windows, mark the region dirty in the compositor
            let Some(resource) = proc.handles().get(handle) else {
                return -1;
            };
            let Some(window_arc) = resource.as_window() else {
                return -1;
            };

            let window_id = window_arc.lock().id;

            if rect_ptr != 0 {
                // Mark specific region dirty
                let rect = unsafe { *(rect_ptr as *const panda_abi::SurfaceRect) };
                let compositor_rect = crate::resource::Rect {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                };
                crate::compositor::mark_window_region_dirty(window_id, compositor_rect);
            } else {
                // Mark entire window dirty
                crate::compositor::mark_window_dirty(window_id);
            }

            // Force immediate composite when flush is called
            crate::compositor::force_composite();
            0
        } else {
            // For non-window surfaces, use traditional flush
            let Some(resource) = proc.handles().get(handle) else {
                return -1;
            };

            let Some(surface) = resource.as_surface() else {
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

            // Surface uses interior mutability, so we can call flush on &self
            match surface.flush(region) {
                Ok(()) => 0,
                Err(_) => -1,
            }
        }
    })
}

/// Handle OP_SURFACE_UPDATE_PARAMS syscall.
///
/// Update window position, size, and visibility.
///
/// Arguments:
/// - handle: Window handle
/// - params_ptr: Pointer to UpdateParamsIn struct
///
/// Returns:
/// - 0 on success
/// - negative error code on failure
pub fn handle_update_params(handle: u32, params_ptr: usize) -> isize {
    debug!(
        "handle_update_params: handle={}, params_ptr={:#x}",
        handle, params_ptr
    );

    if params_ptr == 0 {
        return -1;
    }

    let params = unsafe { *(params_ptr as *const panda_abi::UpdateParamsIn) };

    debug!("About to call with_current_process");
    scheduler::with_current_process(|proc| {
        debug!("Inside with_current_process closure");
        let Some(resource) = proc.handles().get(handle) else {
            debug!("No resource with handle {}", handle);
            return -1;
        };

        debug!("Got resource, checking if window");
        let Some(window_arc) = resource.as_window() else {
            debug!("Resource is not a window");
            return -1;
        };

        debug!("Got window, locking it");
        let window_id = {
            let mut window = window_arc.lock();
            debug!("Window locked");

            // Update window parameters
            window.position = (params.x, params.y);

            // Resize pixel buffer if size changed
            let new_size = (params.width, params.height);
            if window.size != new_size {
                window.size = new_size;
                let buffer_size = (params.width * params.height * 4) as usize;
                window.pixel_data.resize(buffer_size, 0);
            }

            window.visible = params.visible != 0;

            window.id
        };

        // Mark window dirty if visible
        if params.visible != 0 && params.width > 0 && params.height > 0 {
            debug!("Marking window {} dirty", window_id);
            crate::compositor::mark_window_dirty(window_id);
            debug!("Marked window dirty, returning");
        }

        debug!("handle_update_params returning 0");
        0
    })
}
