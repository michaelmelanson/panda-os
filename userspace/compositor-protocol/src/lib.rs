//! Wire protocol between the userspace compositor and its clients.
//!
//! This crate deliberately lives outside `panda-abi`, which is reserved for
//! what the kernel actually implements — the compositor is an ordinary
//! userspace service (plans/userspace-compositor.md).
//!
//! It also owns the one canonical [`alpha_blend`] implementation, shared by
//! the compositor and by client-side drawing code.

#![no_std]

mod blend;
mod message;
mod rect;

pub use blend::{alpha_blend, is_region_opaque};
pub use message::{Event, FORMAT_BGRA8888, MAX_FORMATS, MAX_FRAME_SIZE, Request};
pub use rect::Rect;
