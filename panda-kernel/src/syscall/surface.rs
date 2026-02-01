//! Surface syscall handlers.

#![deny(unsafe_code)]

use alloc::boxed::Box;

use crate::resource::BufferExt;
use crate::scheduler;

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserPtr};

/// Calculate the byte size for a pixel buffer of given dimensions (4 bytes per pixel).
/// Returns `None` if the calculation would overflow.
///
/// This is the canonical pattern for safe pixel arithmetic in this module.
/// All buffer size and pixel offset calculations for userspace-controlled `u32`
/// values must use checked arithmetic to prevent silent wrapping.
fn checked_pixel_buffer_size(width: u32, height: u32) -> Option<usize> {
    width
        .checked_mul(height)
        .and_then(|v| v.checked_mul(4))
        .map(|v| v as usize)
}

/// Calculate the expected source buffer byte size for a blit with sub-region parameters.
/// Returns `None` if any intermediate calculation would overflow.
///
/// Uses checked arithmetic because all parameters are userspace-controlled `u32` values
/// that could be crafted to cause silent wrapping.
fn checked_src_buffer_size(
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    src_stride: u32,
) -> Option<usize> {
    if height == 0 || width == 0 {
        return Some(0);
    }
    // (src_y + height - 1) * src_stride + src_x + width, then * 4
    let last_row = src_y.checked_add(height)?.checked_sub(1)?;
    let row_offset = last_row.checked_mul(src_stride)?;
    let end_pixel = row_offset.checked_add(src_x)?.checked_add(width)?;
    let byte_size = end_pixel.checked_mul(4)?;
    Some(byte_size as usize)
}

/// Check if all pixels in a rectangular region have alpha == 255.
/// Bails early on the first non-opaque pixel, so the cost for mixed-alpha
/// surfaces is negligible.
///
/// Callers must ensure that all pixel offsets within the region are valid
/// indices into `data`. If any offset overflows or falls outside `data`,
/// this function conservatively returns `false`.
fn is_region_opaque(
    data: &[u8],
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    stride: u32,
) -> bool {
    for row in 0..height {
        // Use checked arithmetic — parameters are userspace-controlled u32 values.
        let Some(y_off) = src_y.checked_add(row) else {
            return false;
        };
        let Some(row_pixel) = y_off.checked_mul(stride).and_then(|v| v.checked_add(src_x)) else {
            return false;
        };
        let Some(row_start) = (row_pixel as usize).checked_mul(4) else {
            return false;
        };
        for col in 0..width as usize {
            let Some(offset) = col.checked_mul(4).and_then(|v| row_start.checked_add(v)) else {
                return false;
            };
            let Some(alpha_idx) = offset.checked_add(3) else {
                return false;
            };
            if data.get(alpha_idx).copied() != Some(255) {
                return false;
            }
        }
    }
    true
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
pub fn handle_info(
    ua: &UserAccess,
    handle: u64,
    info_ptr: UserPtr<panda_abi::SurfaceInfoOut>,
) -> SyscallFuture {
    if info_ptr.addr() == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    }

    let out = info_ptr;

    let result = scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return Err(panda_abi::ErrorCode::InvalidHandle);
        };

        let Some(surface) = resource.as_surface() else {
            return Err(panda_abi::ErrorCode::InvalidHandle);
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
            if ua.write_user(out, &info).is_err() {
                return Box::pin(core::future::ready(SyscallResult::err(
                    panda_abi::ErrorCode::InvalidArgument,
                )));
            }
            Box::pin(core::future::ready(SyscallResult::ok(0)))
        }
        Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
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
pub fn handle_blit(
    ua: &UserAccess,
    handle: u64,
    params_ptr: UserPtr<panda_abi::BlitParams>,
) -> SyscallFuture {
    if params_ptr.addr() == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    }

    let params: panda_abi::BlitParams = match ua.read_user(params_ptr) {
        Ok(p) => p,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    let result: Result<isize, panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
        // Check if this is a window resource
        let is_window = {
            let Some(resource) = proc.handles().get(handle) else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };
            resource.as_window().is_some()
        };

        if is_window {
            // For windows, copy pixels from source buffer into window's pixel_data
            let source_buffer = {
                let Some(buffer_handle) = proc.handles().get(params.buffer_handle) else {
                    return Err(panda_abi::ErrorCode::InvalidHandle);
                };
                let Some(buffer) = buffer_handle.resource_arc().as_shared_buffer() else {
                    return Err(panda_abi::ErrorCode::InvalidHandle);
                };
                buffer
            };

            // When src_stride is 0, fall back to width (backwards compatibility)
            let src_stride = if params.src_stride > 0 {
                params.src_stride
            } else {
                params.width
            };

            // Checked arithmetic — all BlitParams fields are userspace-controlled u32 values.
            let Some(expected_src_size) = checked_src_buffer_size(
                params.src_x,
                params.src_y,
                params.width,
                params.height,
                src_stride,
            ) else {
                return Err(panda_abi::ErrorCode::InvalidArgument);
            };

            // Copy pixel data from user-mapped buffer into a kernel-owned Vec.
            // This keeps the SMAP window short — only the memcpy runs with AC set.
            let src_data = source_buffer.as_ref().with_slice(|s| {
                if s.len() < expected_src_size {
                    None
                } else {
                    Some(s[..expected_src_size].to_vec())
                }
            });

            let Some(src_data) = src_data else {
                return Err(panda_abi::ErrorCode::BufferTooSmall);
            };

            let Some(resource) = proc.handles().get(handle) else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };
            let Some(window_arc) = resource.as_window() else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };

            let window_id = {
                let mut window = window_arc.lock();

                let window_width = window.size.0;
                let window_height = window.size.1;

                // Bounds check — use checked arithmetic to prevent wrapping.
                let Some(dst_right) = params.x.checked_add(params.width) else {
                    return Err(panda_abi::ErrorCode::InvalidArgument);
                };
                let Some(dst_bottom) = params.y.checked_add(params.height) else {
                    return Err(panda_abi::ErrorCode::InvalidArgument);
                };
                if dst_right > window_width || dst_bottom > window_height {
                    return Err(panda_abi::ErrorCode::InvalidArgument);
                }

                // Choose fast path (row memcpy) or slow path (per-pixel alpha blend).
                // The opaque scan bails early on the first non-255 alpha byte,
                // so mixed-alpha surfaces pay almost nothing for the check.
                if is_region_opaque(
                    &src_data,
                    params.src_x,
                    params.src_y,
                    params.width,
                    params.height,
                    src_stride,
                ) {
                    // Fast path: copy entire rows at once (one memcpy per scanline).
                    // Pixel offsets use checked arithmetic — userspace-controlled u32 values.
                    let Some(row_bytes) = (params.width as usize).checked_mul(4) else {
                        return Err(panda_abi::ErrorCode::InvalidArgument);
                    };
                    for row in 0..params.height {
                        let Some(src_row_start) = params
                            .src_y
                            .checked_add(row)
                            .and_then(|v| v.checked_mul(src_stride))
                            .and_then(|v| v.checked_add(params.src_x))
                            .and_then(|v| (v as usize).checked_mul(4))
                        else {
                            return Err(panda_abi::ErrorCode::InvalidArgument);
                        };
                        let Some(dst_row_start) = params
                            .y
                            .checked_add(row)
                            .and_then(|v| v.checked_mul(window_width))
                            .and_then(|v| v.checked_add(params.x))
                            .and_then(|v| (v as usize).checked_mul(4))
                        else {
                            return Err(panda_abi::ErrorCode::InvalidArgument);
                        };

                        if dst_row_start + row_bytes > window.pixel_data.len() {
                            return Err(panda_abi::ErrorCode::InvalidArgument);
                        }
                        if src_row_start + row_bytes > src_data.len() {
                            return Err(panda_abi::ErrorCode::InvalidArgument);
                        }

                        window.pixel_data[dst_row_start..dst_row_start + row_bytes]
                            .copy_from_slice(&src_data[src_row_start..src_row_start + row_bytes]);
                    }
                } else {
                    // Slow path: per-pixel alpha blending.
                    // Pixel offsets use checked arithmetic — userspace-controlled u32 values.
                    for row in 0..params.height {
                        for col in 0..params.width {
                            let Some(src_offset) = params
                                .src_y
                                .checked_add(row)
                                .and_then(|v| v.checked_mul(src_stride))
                                .and_then(|v| v.checked_add(params.src_x))
                                .and_then(|v| v.checked_add(col))
                                .and_then(|v| v.checked_mul(4))
                                .map(|v| v as usize)
                            else {
                                return Err(panda_abi::ErrorCode::InvalidArgument);
                            };
                            let Some(dst_offset) = params
                                .y
                                .checked_add(row)
                                .and_then(|v| v.checked_mul(window_width))
                                .and_then(|v| v.checked_add(params.x))
                                .and_then(|v| v.checked_add(col))
                                .and_then(|v| v.checked_mul(4))
                                .map(|v| v as usize)
                            else {
                                return Err(panda_abi::ErrorCode::InvalidArgument);
                            };

                            if dst_offset + 4 > window.pixel_data.len() {
                                return Err(panda_abi::ErrorCode::InvalidArgument);
                            }

                            let src_a = src_data[src_offset + 3] as u16;

                            if src_a == 0 {
                                // Fully transparent — skip
                                continue;
                            } else if src_a == 255 {
                                // Fully opaque — direct copy
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
                }

                // Keep buffer reference for backwards compatibility
                window.content_buffer = Some(source_buffer);
                window.id
            };

            // Mark window dirty
            crate::compositor::mark_window_dirty(window_id);
            Ok(0)
        } else {
            // For non-window surfaces, use traditional blit.
            // Get both buffer and surface handles in the same scope to avoid
            // raw pointer reconstruction.
            let Some(buffer_resource) = proc.handles().get(params.buffer_handle) else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };
            let Some(buffer) = buffer_resource.as_buffer() else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };

            let Some(expected_size) = checked_pixel_buffer_size(params.width, params.height) else {
                return Err(panda_abi::ErrorCode::InvalidArgument);
            };

            // Copy pixel data from user-mapped buffer into kernel-owned Vec
            // to keep the SMAP window short.
            let pixel_data = buffer.with_slice(|buffer_slice| {
                if buffer_slice.len() < expected_size {
                    None
                } else {
                    Some(buffer_slice[..expected_size].to_vec())
                }
            });

            let Some(pixel_data) = pixel_data else {
                return Err(panda_abi::ErrorCode::BufferTooSmall);
            };

            let Some(resource) = proc.handles().get(handle) else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };
            let Some(surface) = resource.as_surface() else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };

            match surface.blit(params.x, params.y, params.width, params.height, &pixel_data) {
                Ok(()) => Ok(0),
                Err(_) => Err(panda_abi::ErrorCode::IoError),
            }
        }
    });

    match result {
        Ok(v) => Box::pin(core::future::ready(SyscallResult::ok(v))),
        Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
    }
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
pub fn handle_fill(
    ua: &UserAccess,
    handle: u64,
    params_ptr: UserPtr<panda_abi::FillParams>,
) -> SyscallFuture {
    if params_ptr.addr() == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    }

    let params: panda_abi::FillParams = match ua.read_user(params_ptr) {
        Ok(p) => p,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    let result: Result<isize, panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return Err(panda_abi::ErrorCode::InvalidHandle);
        };

        let Some(surface) = resource.as_surface() else {
            return Err(panda_abi::ErrorCode::InvalidHandle);
        };

        match surface.fill(
            params.x,
            params.y,
            params.width,
            params.height,
            params.colour,
        ) {
            Ok(()) => Ok(0),
            Err(_) => Err(panda_abi::ErrorCode::IoError),
        }
    });

    match result {
        Ok(v) => Box::pin(core::future::ready(SyscallResult::ok(v))),
        Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
    }
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
pub fn handle_flush(
    ua: &UserAccess,
    handle: u64,
    rect_ptr: Option<UserPtr<panda_abi::SurfaceRect>>,
) -> SyscallFuture {
    let is_window = scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return None;
        };
        Some(resource.as_window().is_some())
    });

    let Some(is_window) = is_window else {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )));
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
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidHandle,
            )));
        };

        if let Some(rp) = rect_ptr {
            // Read rect from userspace
            let rect: panda_abi::SurfaceRect = match ua.read_user(rp) {
                Ok(r) => r,
                Err(_) => {
                    return Box::pin(core::future::ready(SyscallResult::err(
                        panda_abi::ErrorCode::InvalidArgument,
                    )));
                }
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
        let result: Result<isize, panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
            let Some(resource) = proc.handles().get(handle) else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };

            let Some(surface) = resource.as_surface() else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };

            let region = if let Some(rp) = rect_ptr {
                let rect: panda_abi::SurfaceRect = match ua.read_user(rp) {
                    Ok(r) => r,
                    Err(_) => return Err(panda_abi::ErrorCode::InvalidArgument),
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
                Ok(()) => Ok(0),
                Err(_) => Err(panda_abi::ErrorCode::IoError),
            }
        });

        match result {
            Ok(v) => Box::pin(core::future::ready(SyscallResult::ok(v))),
            Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
        }
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
pub fn handle_update_params(
    ua: &UserAccess,
    handle: u64,
    params_ptr: UserPtr<panda_abi::UpdateParamsIn>,
) -> SyscallFuture {
    if params_ptr.addr() == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    }

    let params: panda_abi::UpdateParamsIn = match ua.read_user(params_ptr) {
        Ok(p) => p,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    let result: Result<isize, panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
        let Some(resource) = proc.handles().get(handle) else {
            return Err(panda_abi::ErrorCode::InvalidHandle);
        };

        let Some(window_arc) = resource.as_window() else {
            return Err(panda_abi::ErrorCode::InvalidHandle);
        };

        let window_id = {
            let mut window = window_arc.lock();

            // Update window parameters
            window.position = (params.x, params.y);

            // Resize pixel buffer if size changed
            let new_size = (params.width, params.height);
            if window.size != new_size {
                window.size = new_size;

                let Some(buffer_size) = checked_pixel_buffer_size(params.width, params.height)
                else {
                    return Err(panda_abi::ErrorCode::InvalidArgument);
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

        Ok(0)
    });

    match result {
        Ok(v) => Box::pin(core::future::ready(SyscallResult::ok(v))),
        Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
    }
}
