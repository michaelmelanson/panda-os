//! The Panda filesystem service.
//!
//! This crate holds the mount table, the `Filesystem`/`File` interfaces,
//! and the ext2 implementation. The kernel keeps only
//! `vfs::TarFs` (the in-memory ustar archive behind `initrd:`), which is
//! what bootstraps `init` and this service.
//!
//! # Layering
//!
//! - [`fs`] — the filesystem interfaces (`Filesystem`, `File`, `FsError`, …).
//! - [`mount`] — the mount table and path canonicalisation policy.
//! - [`block`] — the [`block::BlockDevice`] interface ext2 reads and writes
//!   through.
//! - [`ext2`] — the ext2 implementation.
//!
//! Everything above is platform-independent, so it builds (and unit-tests)
//! on the host. The `os` feature adds the parts that talk to the kernel:
//! [`schemes`] (a `block:` client used as a `BlockDevice`, and an `initrd:`
//! client used as a `Filesystem`) and the `fs` binary's serving loop in
//! `src/main.rs`. This mirrors `userspace/compositor`'s split.
//!
//! # Why synchronous?
//!
//! The kernel's version of this code was async because it ran inside the
//! kernel's task executor, where a block read had to yield rather than
//! block the whole kernel. This service is an ordinary single-threaded
//! process: a block read is a blocking `OP_FILE_READ` syscall on its
//! `block:` handle, and while it is blocked the kernel runs everything
//! else. There is nothing left for an `async fn` to yield to, so the
//! interfaces here are plain synchronous ones — which is also what lets the
//! mount table be unit-tested on the host without an executor.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod block;
pub mod ext2;
pub mod fs;
pub mod logging;
pub mod mount;

#[cfg(feature = "os")]
pub mod schemes;
