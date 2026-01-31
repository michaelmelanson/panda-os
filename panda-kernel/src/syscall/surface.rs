//! Surface syscall handlers.

#![deny(unsafe_code)]

use alloc::boxed::Box;

use crate::resource::Buffer;
use crate::scheduler;

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess};

/// Calculate the byte size for a pixel buffer of given dimensions (4 bytes per pixel).
/// Returns `None` if the calculation would overflow.
fn checked_pixel_buffer_size(width: u32, height: u32) -> Option<usize> {
    width
        .checked_mul(height)
        .and_then(|v| v.checked_mul(4))
        .map(|v| v as usize)
}

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
pub fn handle_info(ua: &UserAccess, handle: u32, info_ptr: usize) -> SyscallFuture {
    if info_ptr == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
    }

    let result = scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return Err(());
        };

        let Some(surface) = resource.as_surface() else {
            return Err(());
        };

        let info = surface.info();
        Ok(panda_abi::SurfaceInfoOut {
            width: info.width,
            height: info.height,
            format: info.format as u32,
            stride: info.stride,
        })
    });

    match result {
        Ok(info) => {
            if ua.write_struct(info_ptr, &info).is_err() {
                return Box::pin(core::future::ready(SyscallResult::err(-1)));
            }
            Box::pin(core::future::ready(SyscallResult::ok(0)))
        }
        Err(()) => Box::pin(core::future::ready(SyscallResult::err(-1))),
    }
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
pub fn handle_blit(ua: &UserAccess, handle: u32, params_ptr: usize) -> SyscallFuture {
    if params_ptr == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
    }

    let params: panda_abi::BlitParams = match ua.read_struct(params_ptr) {
        Ok(p) => p,
        Err(_) => return Box::pin(core::future::ready(SyscallResult::err(-1))),
    };

    let result = scheduler::with_current_process(|proc| {
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

                let Some(expected_src_size) = checked_pixel_buffer_size(params.width, params.height) else {
                    return -1;
                };

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
            // For non-window surfaces, use traditional blit.
            // Get both buffer and surface handles in the same scope to avoid
            // raw pointer reconstruction.
            let Some(buffer_resource) = proc.handles().get(params.buffer_handle) else {
                return -1;
            };
            let Some(buffer) = buffer_resource.as_buffer() else {
                return -1;
            };
            let buffer_slice = buffer.as_slice();

            let Some(expected_size) = checked_pixel_buffer_size(params.width, params.height) else {
                return -1;
            };

            if buffer_slice.len() < expected_size {
                return -1;
            }

            let Some(resource) = proc.handles().get(handle) else {
                return -1;
            };
            let Some(surface) = resource.as_surface() else {
                return -1;
            };

            match surface.blit(
                params.x,
                params.y,
                params.width,
                params.height,
                &buffer_slice[..expected_size],
            ) {
                Ok(()) => 0,
                Err(_) => -1,
            }
        }
    });

    Box::pin(core::future::ready(SyscallResult::ok(result)))
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
pub fn handle_fill(ua: &UserAccess, handle: u32, params_ptr: usize) -> SyscallFuture {
    if params_ptr == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
    }

    let params: panda_abi::FillParams = match ua.read_struct(params_ptr) {
        Ok(p) => p,
        Err(_) => return Box::pin(core::future::ready(SyscallResult::err(-1))),
    };

    let result = scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return -1;
        };

        let Some(surface) = resource.as_surface() else {
            return -1;
        };

        match surface.fill(
            params.x,
            params.y,
            params.width,
            params.height,
            params.colour,
        ) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    });

    Box::pin(core::future::ready(SyscallResult::ok(result)))
}

/// Handle OP_SURFACE_FLUSH syscall.
///
/// Flush surface updates to the display. For windows, this blocks until the
/// compositor has completed a frame with the updated content.
///
/// Arguments:
/// - handle: Surface handle
/// - rect_ptr: Optional pointer to SurfaceRect (0 for full flush)
///
/// Returns:
/// - 0 on success
/// - negative error code on failure
pub fn handle_flush(ua: &UserAccess, handle: u32, rect_ptr: usize) -> SyscallFuture {
    let is_window = scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return None;
        };
        Some(resource.as_window().is_some())
    });

    let Some(is_window) = is_window else {
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
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
            return Box::pin(core::future::ready(SyscallResult::err(-1)));
        };

        if rect_ptr != 0 {
            // Read rect from userspace
            let rect: panda_abi::SurfaceRect = match ua.read_struct(rect_ptr) {
                Ok(r) => r,
                Err(_) => return Box::pin(core::future::ready(SyscallResult::err(-1))),
            };
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
        Box::pin(async move {
            crate::compositor::wait_for_next_frame().await;
            SyscallResult::ok(0)
        })
    } else {
        // For non-window surfaces, use traditional synchronous flush
        let result = scheduler::with_current_process(|proc| {
            let Some(resource) = proc.handles().get(handle) else {
                return -1;
            };

            let Some(surface) = resource.as_surface() else {
                return -1;
            };

            let region = if rect_ptr != 0 {
                let rect: panda_abi::SurfaceRect = match ua.read_struct(rect_ptr) {
                    Ok(r) => r,
                    Err(_) => return -1,
                };
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
        });

        Box::pin(core::future::ready(SyscallResult::ok(result)))
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
pub fn handle_update_params(ua: &UserAccess, handle: u32, params_ptr: usize) -> SyscallFuture {
    if params_ptr == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
    }

    let params: panda_abi::UpdateParamsIn = match ua.read_struct(params_ptr) {
        Ok(p) => p,
        Err(_) => return Box::pin(core::future::ready(SyscallResult::err(-1))),
    };

    let result = scheduler::with_current_process(|proc| {
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

                let Some(buffer_size) = checked_pixel_buffer_size(params.width, params.height) else {
                    return -1;
                };

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
    });

    Box::pin(core::future::ready(SyscallResult::ok(result)))
}
