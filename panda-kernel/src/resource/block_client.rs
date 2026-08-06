//! A kernel-internal `BlockDevice` backed by a userspace scheme provider.
//!
//! # Why this exists
//!
//! M4's block driver service moves virtio-blk out of the kernel and into a
//! userspace driver (`userspace/drivers/virtio-blk`) that registers `block:`
//! the same way `userspace/drivers/virtio-keyboard` registers `keyboard:`
//! (see `resource::scheme::UserSchemeProvider`/`register_user_scheme`). That
//! works unmodified for *client processes* opening `block:/...` via
//! `OP_ENVIRONMENT_OPEN` — they already get a `SchemeProxyResource` from the
//! scheme registry with no kernel changes needed.
//!
//! It does *not* work for `panda-kernel/src/vfs/ext2` and
//! `vfs::mount_ext2`, which are kernel-internal code, not a process: they
//! need a live [`super::BlockDevice`] trait object to drive many small
//! random-access reads/writes, and the only existing primitive for
//! "kernel-internal code holding a channel to a userspace scheme provider"
//! (`syscall::environment::handle_connect` / `resource::connect`) installs
//! its result into a *process's* handle table — there was no way for
//! kernel-internal code without a handle table of its own to hold one.
//!
//! [`SchemeBlockDevice`] is that missing primitive: it opens a `block:`-like
//! URI via the ordinary [`super::open`] path (exactly what a client process
//! opening `block:/...` would resolve to — a [`super::SchemeProxyResource`]
//! wrapped in a `Box<dyn Resource>`), keeps that `Box` alive for its own
//! lifetime, and implements `read_at`/`write_at`/`size`/`sync` in terms of
//! the scheme-proxy round trips already used to serve client `OP_FILE_READ`/
//! `OP_FILE_WRITE` — just called directly, without a process or syscall in
//! the loop. No new "kernel process" concept was needed: the
//! [`super::scheme::ProviderState`] round-trip machinery underneath
//! `SchemeProxyResource` was already independent of any process — the only
//! thing tying it to a process was `handle_open`/`handle_connect` installing
//! the *result* into a handle table, which this bypasses entirely by simply
//! owning the `Box<dyn Resource>` itself.
//!
//! Exclusivity (the M1 claim rules `block:` must honour — see
//! `plans/ROADMAP.md`'s M1 section and `claim_test`) is enforced by the
//! provider's own `Open` handling now, not by a kernel-side
//! `devices::claims::claim` call: opening this adapter *is* a `block:` open
//! like any other, so a provider that refuses a second concurrent open
//! (mirroring `devices::claims`' single-owner semantics) makes a raw
//! `block:` open from another process fail `Busy` while ext2 has this
//! adapter open, exactly as `devices::claims::ClaimOwner::Mount` used to.
//! Dropping this adapter (unmount) drops its `Box<dyn Resource>`, which
//! sends the provider a `Close` (see `SchemeProxyResource`'s `Drop`),
//! releasing the provider's exclusivity — no separate cleanup path needed.

use alloc::boxed::Box;
use async_trait::async_trait;

use super::{BlockDevice, BlockError, OpenError, Resource};

/// The default sector size assumed for a scheme-backed block device.
///
/// The wire protocol (`panda_abi::scheme_protocol`) has no sector-size
/// query — only [`Request::Size`](panda_abi::scheme_protocol::Request::Size)
/// for the total byte size — so this matches virtio-blk's near-universal
/// 512-byte sector (the deleted kernel driver,
/// `panda-kernel/src/devices/virtio_block`, hardcoded the same value for the
/// same reason: virtio-blk's `VIRTIO_BLK_F_BLK_SIZE` feature, which would
/// advertise a different size, is not negotiated by either driver).
const DEFAULT_SECTOR_SIZE: u32 = 512;

/// A [`BlockDevice`] backed by a `block:`-scheme userspace provider,
/// addressed as a byte offset/length round trip per operation. See the
/// module doc comment for why this exists and how it differs from a client
/// process's ordinary `block:` open.
pub struct SchemeBlockDevice {
    /// Kept alive for this adapter's whole lifetime: dropping it closes the
    /// provider's resource (see the module doc comment's "Exclusivity"
    /// paragraph). Always resolves to a `SchemeProxyResource` in practice
    /// (anything opened through a `UserSchemeProvider`-backed scheme does),
    /// checked via `as_scheme_proxy()` on every call rather than assumed
    /// once, since `Resource` gives no other way to downcast an owned
    /// `Box<dyn Resource>` into an owned `SchemeProxyResource`.
    resource: Box<dyn Resource>,
    /// Cached at [`open`](Self::open) time: `BlockDevice::size` is
    /// synchronous, but querying it is an async round trip
    /// (`Request::Size`), so it can't be fetched per-call.
    size: u64,
}

/// Errors opening a [`SchemeBlockDevice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemeBlockDeviceError {
    /// The URI didn't resolve to a scheme-proxy resource at all (e.g. no
    /// such scheme registered, or `Busy` — see [`OpenError`]).
    Open(OpenError),
    /// The resource opened, but querying its size failed (e.g. the
    /// provider replied `SizeErr`, or the round trip itself failed).
    Size(panda_abi::ErrorCode),
}

impl SchemeBlockDevice {
    /// Open `uri` (e.g. `"block:/pci/storage/0"`) as a scheme-backed block
    /// device: resolves it through the ordinary [`super::open`] scheme
    /// registry, then fetches and caches its size via `Request::Size`.
    pub async fn open(uri: &str) -> Result<Self, SchemeBlockDeviceError> {
        let resource = super::open(uri).await.map_err(SchemeBlockDeviceError::Open)?;
        let proxy = resource
            .as_scheme_proxy()
            .ok_or(SchemeBlockDeviceError::Open(OpenError::NotFound))?;
        let size = proxy.size().await.map_err(SchemeBlockDeviceError::Size)?;
        Ok(Self { resource, size })
    }
}

#[async_trait]
impl BlockDevice for SchemeBlockDevice {
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }
        // Safety of unwrap: see the `resource` field's doc comment — this
        // adapter is only ever constructed from a scheme-proxy open.
        let proxy = self.resource.as_scheme_proxy().unwrap();

        // A single request/response frame caps the transfer size (see
        // `panda_abi::scheme_protocol::MAX_TRANSFER_SIZE`); loop to satisfy
        // reads larger than that, same shape as
        // `syscall::file::handle_read_scheme_proxy`'s per-call cap but
        // repeated until `buf` is filled or the provider returns short
        // (EOF/device boundary).
        let mut done = 0;
        while done < buf.len() {
            let want = (buf.len() - done).min(panda_abi::scheme_protocol::MAX_TRANSFER_SIZE);
            let data = proxy
                .read_at(offset + done as u64, want, false)
                .await
                .map_err(|_| BlockError::IoError)?;
            if data.is_empty() {
                break;
            }
            let n = data.len().min(want);
            buf[done..done + n].copy_from_slice(&data[..n]);
            done += n;
            if n < want {
                // Short read: provider has no more data right now (EOF or
                // device boundary) — stop rather than looping forever.
                break;
            }
        }
        Ok(done)
    }

    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let proxy = self.resource.as_scheme_proxy().unwrap();

        let mut done = 0;
        while done < buf.len() {
            let end = (done + panda_abi::scheme_protocol::MAX_TRANSFER_SIZE).min(buf.len());
            let n = proxy
                .write_at(offset + done as u64, &buf[done..end])
                .await
                .map_err(|_| BlockError::IoError)?;
            if n == 0 {
                break;
            }
            done += n;
        }
        Ok(done)
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        DEFAULT_SECTOR_SIZE
    }

    async fn sync(&self) -> Result<(), BlockError> {
        // No `Request::Sync` in the wire protocol: every `Write` round trip
        // already only completes once the provider acknowledges it (matches
        // the deleted kernel driver's `VirtioBlockDevice::sync`, which was
        // also a no-op — virtio-blk is write-through, see that impl's doc
        // comment).
        Ok(())
    }
}
