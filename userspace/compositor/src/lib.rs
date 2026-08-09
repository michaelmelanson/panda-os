//! The userspace compositor.
//!
//! Window management, damage tracking and blending happen in this process,
//! which is an ordinary client of the `display:` scheme.
//!
//! The `os` feature (on by default) pulls in everything that talks to the
//! kernel. Without it only [`manager`] compiles, so the window-management
//! logic can be unit-tested on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod manager;
pub mod target;

#[cfg(feature = "os")]
pub mod display;
#[cfg(feature = "os")]
pub mod server;
