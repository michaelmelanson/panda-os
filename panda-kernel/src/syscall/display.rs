//! Display syscall handlers (`OP_DISPLAY_*`).
//!
//! `display:` is served by a userspace driver process (roadmap M4 —
//! `userspace/drivers/virtio-gpu`), so every handle this operates on is a
//! [`crate::resource::SchemeProxyResource`]. Each handler forwards to the
//! provider (via [`crate::resource::SchemeProxyResource::display_info`]/
//! `display_map`/`display_flush`) and, for `OP_DISPLAY_MAP`, maps the
//! physical range the provider discloses directly into the calling
//! process — see `panda_abi::scheme_protocol::MSG_DISPLAY_MAP`'s doc
//! comment for why that's a raw physical address rather than proxied bytes.
//! This is the same client-facing contract the old kernel-resident
//! `DisplayDevice` served; only the provider moved to userspace.

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;

use x86_64::PhysAddr;

use crate::memory::{MemoryMappingOptions, map_external};
use crate::resource::Resource;
use crate::scheduler;

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserPtr};

/// Resolve `handle` to a userspace scheme provider's proxy resource, if it
/// is one — the only kind of resource `display:` opens produce now. Mirrors
/// `syscall/file.rs`'s `get_scheme_proxy`.
fn get_scheme_proxy(handle: u64) -> Option<Arc<dyn Resource>> {
    scheduler::with_current_process(|proc| {
        let h = proc.handles().get(handle)?;
        if h.as_scheme_proxy().is_some() {
            Some(h.resource_arc())
        } else {
            None
        }
    })
}

/// Handle `OP_DISPLAY_INFO`: write the display's mode info to `info_ptr`.
pub fn handle_info(
    _ua: &UserAccess,
    handle: u64,
    info_ptr: UserPtr<panda_abi::SurfaceInfoOut>,
) -> SyscallFuture {
    if info_ptr.addr() == 0 {
        return err(panda_abi::ErrorCode::InvalidArgument);
    }
    // The actual write happens after the round trip below, via
    // `SyscallResult::write_back_struct` (checked and copied out by the
    // top-level syscall dispatcher with a fresh `UserAccess`) rather than
    // through `ua` directly here — `UserAccess` must not be held across an
    // `.await` (see `syscall/file.rs`'s scheme-proxy read path for the same
    // convention).
    let Some(resource) = get_scheme_proxy(handle) else {
        return err(panda_abi::ErrorCode::InvalidHandle);
    };

    Box::pin(async move {
        // Safety of unwrap: `resource` was resolved via `get_scheme_proxy`,
        // which only returns handles where `as_scheme_proxy()` is `Some`.
        let proxy = resource.as_scheme_proxy().unwrap();
        match proxy.display_info().await {
            Ok((width, height, format, stride)) => {
                let info = panda_abi::SurfaceInfoOut { width, height, format, stride };
                SyscallResult::write_back_struct(0, &info, info_ptr.as_slice())
            }
            Err(code) => SyscallResult::err(code),
        }
    })
}

/// Handle `OP_DISPLAY_MAP`: map the framebuffer into the calling process and
/// return the userspace virtual address.
pub fn handle_map(handle: u64) -> SyscallFuture {
    let Some(resource) = get_scheme_proxy(handle) else {
        return err(panda_abi::ErrorCode::InvalidHandle);
    };

    Box::pin(async move {
        let proxy = resource.as_scheme_proxy().unwrap();
        let (phys_addr, size) = match proxy.display_map().await {
            Ok(v) => v,
            Err(code) => return SyscallResult::err(code),
        };
        let size = size as usize;
        let num_pages = size.div_ceil(4096);

        // `proxy.display_map().await` above is a cross-process round trip to
        // the userspace driver — by the time it resolves, an arbitrary
        // number of other processes may have been scheduled in between, so
        // the CPU's currently-loaded CR3 is *not* guaranteed to be the
        // calling process's page table anymore (unlike a plain syscall
        // entry, which always starts with the caller's CR3 already active).
        // `map_external`/`is_mapped` below walk page tables via the
        // recursive self-map, which is relative to whatever CR3 is loaded
        // right now — so without an explicit switch here they can silently
        // operate on a *different* process's address space (this surfaced
        // as a `NOT PRESENT` page fault back in the calling process the
        // first time a client actually wrote to the "mapped" framebuffer:
        // the mapping had gone into whichever process was current at the
        // time this code ran, not the caller's). Explicitly switch to the
        // calling process's page table before doing the mapping work — see
        // `memory::ensure_page_table`'s doc comment for why it's left
        // active afterward rather than restored.
        let result: Result<usize, panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
            let vaddr = proc
                .alloc_buffer_vaddr(num_pages)
                .ok_or(panda_abi::ErrorCode::IoError)?;

            let target_page_table = proc.page_table_phys();
            crate::memory::ensure_page_table(target_page_table);
            let mapping = map_external(
                PhysAddr::new(phys_addr),
                vaddr,
                size,
                MemoryMappingOptions {
                    user: true,
                    executable: false,
                    writable: true,
                },
            );
            proc.add_mapping(mapping);

            Ok(vaddr.as_u64() as usize)
        });

        match result {
            Ok(vaddr) => SyscallResult::ok(vaddr as isize),
            Err(code) => SyscallResult::err(code),
        }
    })
}

/// Handle `OP_DISPLAY_FLUSH`: forward a damaged rectangle to the driver.
///
/// `rect_ptr` is `None` for a full-screen flush.
pub fn handle_flush(
    ua: &UserAccess,
    handle: u64,
    rect_ptr: Option<UserPtr<panda_abi::SurfaceRect>>,
) -> SyscallFuture {
    let rect = match rect_ptr {
        Some(ptr) => match ua.read_user(ptr) {
            Ok(rect) => Some((rect.x, rect.y, rect.width, rect.height)),
            Err(_) => return err(panda_abi::ErrorCode::InvalidArgument),
        },
        None => None,
    };
    let Some(resource) = get_scheme_proxy(handle) else {
        return err(panda_abi::ErrorCode::InvalidHandle);
    };

    Box::pin(async move {
        let proxy = resource.as_scheme_proxy().unwrap();
        match proxy.display_flush(rect).await {
            Ok(()) => SyscallResult::ok(0),
            Err(code) => SyscallResult::err(code),
        }
    })
}

fn err(code: panda_abi::ErrorCode) -> SyscallFuture {
    Box::pin(core::future::ready(SyscallResult::err(code)))
}
