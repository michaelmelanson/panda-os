//! Low-level scheme provider registration.
//!
//! For the ergonomic serve-loop API, use `crate::scheme::SchemeProvider`.

use super::{Handle, send};
use panda_abi::*;

/// Register a userspace scheme provider (raw syscall).
///
/// Returns the provider endpoint handle (an ordinary Channel handle — see
/// docs/SYSCALLS.md "Scheme provider operations") on success, or a negative
/// error code.
#[inline(always)]
pub fn register(name: &str) -> isize {
    send(
        Handle::from(0u64), // no source handle for registration
        OP_SCHEME_REGISTER,
        name.as_ptr() as usize,
        name.len(),
        0,
        0,
    )
}
