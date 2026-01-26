//! Surface syscall handlers.

use alloc::boxed::Box;

use crate::process::PendingSyscall;
use crate::resource::Buffer;
use crate::scheduler;
use crate::syscall::SyscallContext;

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
            resource.as_window().is_some()
        };

        if is_window {
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
                    return -1;
                }

                let window_width = window.size.0;
                let window_height = window.size.1;

                // Bounds check
                if params.x + params.width > window_width
                    || params.y + params.height > window_height
                {
                    return -1;
                }

                // Blit with alpha blending
                for row in 0..params.height {
                    for col in 0..params.width {
                        let src_offset = ((row * params.width + col) * 4) as usize;
                        let dst_offset =
                            (((params.y + row) * window_width + params.x + col) * 4) as usize;

                        if dst_offset + 4 > window.pixel_data.len() {
                            return -1;
                        }

                        let src_a = src_data[src_offset + 3] as u16;

                        if src_a == 0 {
                            // Fully transparent - skip
                            continue;
                        } else if src_a == 255 {
                            // Fully opaque - direct copy
                            window.pixel_data[dst_offset..dst_offset + 4]
                                .copy_from_slice(&src_data[src_offset..src_offset + 4]);
                        } else {
                            // Alpha blend: out = src * alpha + dst * (1 - alpha)
                            let inv_a = 255 - src_a;
                            for i in 0..3 {
                                let src_c = src_data[src_offset + i] as u16;
                                let dst_c = window.pixel_data[dst_offset + i] as u16;
                                window.pixel_data[dst_offset + i] =
                                    ((src_c * src_a + dst_c * inv_a) / 255) as u8;
                            }
                            // Output alpha: src_a + dst_a * (1 - src_a)
                            let dst_a = window.pixel_data[dst_offset + 3] as u16;
                            window.pixel_data[dst_offset + 3] =
                                (src_a + (dst_a * inv_a) / 255) as u8;
                        }
                    }
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
            match surface.blit(
                params.x,
                params.y,
                params.width,
                params.height,
                buffer_slice,
            ) {
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
        match surface.fill(
            params.x,
            params.y,
            params.width,
            params.height,
            params.color,
        ) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
}

/// Handle OP_SURFACE_FLUSH syscall.
///
/// Flush surface updates to the display. For windows, this blocks until the
/// compositor has completed a frame with the updated content.
///
/// Arguments:
/// - ctx: Syscall context (needed for async waiting on windows)
/// - handle: Surface handle
/// - rect_ptr: Optional pointer to SurfaceRect (0 for full flush)
///
/// Returns:
/// - 0 on success
/// - negative error code on failure
pub fn handle_flush(ctx: &SyscallContext, handle: u32, rect_ptr: usize) -> isize {
    let is_window = scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return None;
        };
        Some(resource.as_window().is_some())
    });

    let Some(is_window) = is_window else {
        return -1;
    };

    if is_window {
        // For windows, mark the region dirty and wait for compositor
        let window_id = scheduler::with_current_process(|proc| {
            let Some(resource) = proc.handles().get(handle) else {
                return None;
            };
            let Some(window_arc) = resource.as_window() else {
                return None;
            };
            Some(window_arc.lock().id)
        });

        let Some(window_id) = window_id else {
            return -1;
        };

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

        // Wait for next compositor tick to complete
        let future = Box::pin(async move {
            crate::compositor::wait_for_next_frame().await;
            0isize
        });

        scheduler::with_current_process(|proc| {
            proc.set_pending_syscall(PendingSyscall::new(future));
        });

        ctx.yield_for_async()
    } else {
        // For non-window surfaces, use traditional synchronous flush
        scheduler::with_current_process(|proc| {
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
        })
    }
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
    if params_ptr == 0 {
        return -1;
    }

    let params = unsafe { *(params_ptr as *const panda_abi::UpdateParamsIn) };

    scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return -1;
        };

        let Some(window_arc) = resource.as_window() else {
            return -1;
        };

        let window_id = {
            let mut window = window_arc.lock();

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
            crate::compositor::mark_window_dirty(window_id);
        }

        0
    })
}
