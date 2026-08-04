//! Display syscall handlers (`OP_DISPLAY_*`).
//!
//! These operate on a handle opened from the `display:` scheme. That handle
//! can only exist in the process that won the display's exclusive claim, so
//! holding it is the whole permission model: every handler below simply
//! rejects a handle that is not a display with `InvalidHandle`.

#![deny(unsafe_code)]

use alloc::boxed::Box;

use crate::resource::Rect;
use crate::scheduler;

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserPtr};

/// Handle `OP_DISPLAY_INFO`: write the display's mode info to `info_ptr`.
///
/// Reuses [`panda_abi::SurfaceInfoOut`] — width, height, format, stride is
/// exactly the display's mode description, and duplicating the struct under a
/// second name would buy nothing.
pub fn handle_info(
    ua: &UserAccess,
    handle: u64,
    info_ptr: UserPtr<panda_abi::SurfaceInfoOut>,
) -> SyscallFuture {
    if info_ptr.addr() == 0 {
        return err(panda_abi::ErrorCode::InvalidArgument);
    }

    let result = scheduler::with_current_process(|proc| {
        let resource = proc
            .handles()
            .get(handle)
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        let display = resource
            .as_display()
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;

        let info = display.info();
        Ok(panda_abi::SurfaceInfoOut {
            width: info.width,
            height: info.height,
            format: info.format as u32,
            stride: info.stride,
        })
    });

    match result {
        Ok(info) => {
            if ua.write_user(info_ptr, &info).is_err() {
                return err(panda_abi::ErrorCode::InvalidArgument);
            }
            Box::pin(core::future::ready(SyscallResult::ok(0)))
        }
        Err(code) => err(code),
    }
}

/// Handle `OP_DISPLAY_MAP`: map the framebuffer into the calling process and
/// return the userspace virtual address.
pub fn handle_map(handle: u64) -> SyscallFuture {
    let result: Result<usize, panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
        // `map_into_process` needs `&mut Process`, so take an owned `Arc` to
        // the resource first: the borrow of `proc.handles()` must end before
        // the process is borrowed mutably.
        let resource = {
            let handle = proc
                .handles()
                .get(handle)
                .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
            handle.resource_arc()
        };

        resource
            .as_display()
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?
            .map_into_process(proc)
            .map_err(|_| panda_abi::ErrorCode::IoError)
    });

    match result {
        Ok(vaddr) => Box::pin(core::future::ready(SyscallResult::ok(vaddr as isize))),
        Err(code) => err(code),
    }
}

/// Handle `OP_DISPLAY_FLUSH`: forward a damaged rectangle to the driver.
///
/// `rect_ptr` is `None` for a full-screen flush.
pub fn handle_flush(
    ua: &UserAccess,
    handle: u64,
    rect_ptr: Option<UserPtr<panda_abi::SurfaceRect>>,
) -> SyscallFuture {
    let region = match rect_ptr {
        Some(ptr) => match ua.read_user(ptr) {
            Ok(rect) => Some(Rect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            }),
            Err(_) => return err(panda_abi::ErrorCode::InvalidArgument),
        },
        None => None,
    };

    let result = scheduler::with_current_process(|proc| {
        let resource = proc
            .handles()
            .get(handle)
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        let display = resource
            .as_display()
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;

        display
            .flush(region)
            .map_err(|_| panda_abi::ErrorCode::InvalidArgument)
    });

    match result {
        Ok(()) => Box::pin(core::future::ready(SyscallResult::ok(0))),
        Err(code) => err(code),
    }
}

fn err(code: panda_abi::ErrorCode) -> SyscallFuture {
    Box::pin(core::future::ready(SyscallResult::err(code)))
}
