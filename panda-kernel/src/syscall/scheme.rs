//! Scheme registration syscall handler (OP_SCHEME_REGISTER).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use panda_abi::HandleType;

use crate::resource::{self, ChannelEndpoint};
use crate::scheduler;

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserSlice};

/// Handle scheme registration.
///
/// Creates a channel pair, registers a `UserSchemeProvider` holding one
/// endpoint (`kernel_endpoint`) under `name` in the scheme registry, and
/// returns a handle to the other endpoint (`provider_endpoint`) — an
/// ordinary Channel handle. The calling process serves scheme requests with
/// the existing `OP_CHANNEL_SEND`/`OP_CHANNEL_RECV` syscalls; see
/// `docs/SYSCALLS.md` "Scheme provider operations" and
/// `panda_abi::scheme_protocol` for the wire format.
///
/// Arguments:
/// - name_ptr, name_len: the scheme name (e.g. "display")
///
/// Returns the provider endpoint handle on success, or a negative error
/// code: `InvalidArgument` for an empty or non-UTF-8 name, `AlreadyExists`
/// if the name is already registered, `TooManyHandles` if the caller's
/// handle table is full.
pub fn handle_register(ua: &UserAccess, name_ptr: usize, name_len: usize) -> SyscallFuture {
    let name_bytes = match ua.read(UserSlice::new(name_ptr, name_len)) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };
    let Ok(name) = String::from_utf8(name_bytes) else {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    };

    Box::pin(core::future::ready({
        let (kernel_endpoint, provider_endpoint) = ChannelEndpoint::create_pair();
        // Keep a copy of the name: `register_user_scheme` consumes the
        // original, but the `TooManyHandles` rollback path below needs the
        // name again to undo the registration it just made.
        let name_for_rollback = name.clone();

        match resource::register_user_scheme(name, kernel_endpoint) {
            Ok(()) => {
                let result = scheduler::with_current_process(|proc| {
                    proc.handles_mut()
                        .insert_typed(HandleType::Channel, Arc::new(provider_endpoint))
                        .ok()
                });
                match result {
                    Some(handle_id) => SyscallResult::ok(handle_id as isize),
                    None => {
                        // The handle-table insert failed and dropped
                        // provider_endpoint (closing that half of the
                        // channel), but the scheme is already registered
                        // under `name` with the other half stored in the
                        // registry. Without this rollback the name would be
                        // permanently squatted with no process able to serve
                        // it and no unregister path to free it.
                        resource::unregister_scheme_if_present(&name_for_rollback);
                        SyscallResult::err(panda_abi::ErrorCode::TooManyHandles)
                    }
                }
            }
            // provider_endpoint (and the kernel_endpoint moved into
            // register_user_scheme's Err path via the tuple above) are
            // dropped here on failure — register_user_scheme only stores
            // kernel_endpoint on success, so no channel leaks either way.
            Err(code) => SyscallResult::err(code),
        }
    }))
}
